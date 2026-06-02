#requires -Version 7.0
Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$script:Passed = 0
$script:Failed = 0
$script:Skipped = 0
$script:IndexTextCache = [System.Collections.Generic.Dictionary[string, string]]::new([System.StringComparer]::Ordinal)
$script:MaxBatchedBlobBytes = 2 * 1024 * 1024
$script:PreloadError = $null

function Write-Step {
    param([string]$Message)
    Write-Host "[pre-commit] $Message"
}

function Pass {
    param([string]$Name)
    Write-Host "PASS: $Name"
    $script:Passed++
}

function Fail {
    param(
        [string]$Name,
        [string]$Message
    )
    Write-Host "FAIL: $Name"
    Write-Host "[pre-commit] ERROR: $Message"
    $script:Failed++
}

function Skip {
    param(
        [string]$Name,
        [string]$Reason
    )
    Write-Host "SKIP: $Name ($Reason)"
    $script:Skipped++
}

function Invoke-Native {
    param(
        [Parameter(Mandatory = $true)][string]$FileName,
        [Parameter(Mandatory = $true)][string[]]$Arguments
    )

    $psi = [System.Diagnostics.ProcessStartInfo]::new()
    $psi.FileName = $FileName
    foreach ($argument in $Arguments) {
        [void]$psi.ArgumentList.Add($argument)
    }
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    $psi.UseShellExecute = $false
    $utf8NoBom = [System.Text.UTF8Encoding]::new($false)
    $psi.StandardOutputEncoding = $utf8NoBom
    $psi.StandardErrorEncoding = $utf8NoBom

    $process = [System.Diagnostics.Process]::Start($psi)
    $stdout = $process.StandardOutput.ReadToEnd()
    $stderr = $process.StandardError.ReadToEnd()
    $process.WaitForExit()

    [pscustomobject]@{
        ExitCode = $process.ExitCode
        Stdout = $stdout
        Stderr = $stderr
        Output = $stdout + $stderr
    }
}

function Invoke-NativeWithInput {
    param(
        [Parameter(Mandatory = $true)][string]$FileName,
        [Parameter(Mandatory = $true)][string[]]$Arguments,
        [Parameter(Mandatory = $true)][string]$InputText
    )

    $psi = [System.Diagnostics.ProcessStartInfo]::new()
    $psi.FileName = $FileName
    foreach ($argument in $Arguments) {
        [void]$psi.ArgumentList.Add($argument)
    }
    $psi.RedirectStandardInput = $true
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    $psi.UseShellExecute = $false
    $utf8NoBom = [System.Text.UTF8Encoding]::new($false)
    $psi.StandardOutputEncoding = $utf8NoBom
    $psi.StandardErrorEncoding = $utf8NoBom

    $process = [System.Diagnostics.Process]::Start($psi)
    $process.StandardInput.Write($InputText)
    $process.StandardInput.Close()
    $stdout = $process.StandardOutput.ReadToEnd()
    $stderr = $process.StandardError.ReadToEnd()
    $process.WaitForExit()

    [pscustomobject]@{
        ExitCode = $process.ExitCode
        Stdout = $stdout
        Stderr = $stderr
        Output = $stdout + $stderr
    }
}

function Invoke-NativeBytesWithInput {
    param(
        [Parameter(Mandatory = $true)][string]$FileName,
        [Parameter(Mandatory = $true)][string[]]$Arguments,
        [Parameter(Mandatory = $true)][string]$InputText
    )

    $psi = [System.Diagnostics.ProcessStartInfo]::new()
    $psi.FileName = $FileName
    foreach ($argument in $Arguments) {
        [void]$psi.ArgumentList.Add($argument)
    }
    $psi.RedirectStandardInput = $true
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    $psi.UseShellExecute = $false
    $utf8NoBom = [System.Text.UTF8Encoding]::new($false)
    $psi.StandardOutputEncoding = $utf8NoBom
    $psi.StandardErrorEncoding = $utf8NoBom

    $process = [System.Diagnostics.Process]::Start($psi)
    $stderrTask = $process.StandardError.ReadToEndAsync()
    $process.StandardInput.Write($InputText)
    $process.StandardInput.Close()

    $stdoutStream = [System.IO.MemoryStream]::new()
    try {
        $process.StandardOutput.BaseStream.CopyTo($stdoutStream)
        $process.WaitForExit()
        $stderr = $stderrTask.GetAwaiter().GetResult()

        [pscustomobject]@{
            ExitCode = $process.ExitCode
            StdoutBytes = $stdoutStream.ToArray()
            Stderr = $stderr
            Output = $stderr
        }
    } finally {
        $stdoutStream.Dispose()
        $process.Dispose()
    }
}

function Invoke-Git {
    param([Parameter(Mandatory = $true)][string[]]$Arguments)
    $result = Invoke-Native -FileName "git" -Arguments $Arguments
    if ($result.ExitCode -ne 0) {
        throw "git $($Arguments -join ' ') failed:`n$($result.Output)"
    }
    $result
}

function Get-StagedFiles {
    param([string[]]$Pathspecs = @())

    $arguments = @("diff", "--cached", "--name-only", "-z", "--diff-filter=ACDMR", "--") + $Pathspecs
    $result = Invoke-Git -Arguments $arguments
    if ([string]::IsNullOrEmpty($result.Stdout)) {
        return @()
    }

    $result.Stdout.Split([char]0, [System.StringSplitOptions]::RemoveEmptyEntries)
}

function Test-StagedAny {
    param([string[]]$Pathspecs)
    @(Get-StagedFiles -Pathspecs $Pathspecs).Count -gt 0
}

function Get-IndexText {
    param([Parameter(Mandatory = $true)][string]$Path)

    if ($script:IndexTextCache.ContainsKey($Path)) {
        return $script:IndexTextCache[$Path]
    }

    $result = Invoke-Native -FileName "git" -Arguments @("show", ":$Path")
    if ($result.ExitCode -ne 0) {
        return $null
    }
    $script:IndexTextCache[$Path] = $result.Stdout
    $result.Stdout
}

function Get-StagedOrWorkingTreeText {
    param([Parameter(Mandatory = $true)][string]$Path)

    $text = Get-IndexText -Path $Path
    if ($null -ne $text) {
        return $text
    }

    $absolutePath = Join-Path $script:RepoRoot $Path
    if (Test-Path -LiteralPath $absolutePath) {
        return [System.IO.File]::ReadAllText($absolutePath)
    }

    $null
}

function Get-IndexFiles {
    param([string[]]$Pathspecs)

    $arguments = @("ls-files", "-z", "--") + $Pathspecs
    $result = Invoke-Git -Arguments $arguments
    if ([string]::IsNullOrEmpty($result.Stdout)) {
        return @()
    }
    $result.Stdout.Split([char]0, [System.StringSplitOptions]::RemoveEmptyEntries)
}

function Get-GitPathFileName {
    param([Parameter(Mandatory = $true)][string]$Path)

    $lastSlash = $Path.LastIndexOf("/")
    if ($lastSlash -lt 0) {
        return $Path
    }

    $Path.Substring($lastSlash + 1)
}

function Get-IndexObjectId {
    param([Parameter(Mandatory = $true)][string]$Path)

    $result = Invoke-Git -Arguments @("ls-files", "-s", "--", $Path)
    foreach ($rawLine in $result.Stdout -split "`n") {
        $line = $rawLine.TrimEnd("`r")
        if ([string]::IsNullOrWhiteSpace($line)) {
            continue
        }

        $tabIndex = $line.IndexOf("`t")
        if ($tabIndex -lt 0) {
            continue
        }

        $metadata = $line.Substring(0, $tabIndex)
        $fields = @($metadata.Split(" ", [System.StringSplitOptions]::RemoveEmptyEntries))
        if ($fields.Count -ge 2) {
            return $fields[1]
        }
    }

    $null
}

function Get-IndexSkillFileObjectIds {
    $result = Invoke-Git -Arguments @("ls-files", "-s", "-z", "--", ":(glob).llm/skills/*.md")
    $objectIdsByPath = [System.Collections.Generic.Dictionary[string, string]]::new([System.StringComparer]::Ordinal)
    if ([string]::IsNullOrEmpty($result.Stdout)) {
        return $objectIdsByPath
    }

    foreach ($record in $result.Stdout.Split([char]0, [System.StringSplitOptions]::RemoveEmptyEntries)) {
        $tabIndex = $record.IndexOf("`t")
        if ($tabIndex -lt 0) {
            throw "Unable to parse git ls-files record: $record"
        }

        $metadata = $record.Substring(0, $tabIndex)
        $path = $record.Substring($tabIndex + 1)
        if ((Get-GitPathFileName -Path $path) -eq "index.md") {
            continue
        }

        $fields = @($metadata.Split(" ", [System.StringSplitOptions]::RemoveEmptyEntries))
        if ($fields.Count -lt 2) {
            throw "Unable to parse git ls-files metadata: $metadata"
        }

        $objectIdsByPath[$path] = $fields[1]
    }

    return $objectIdsByPath
}

function Get-IndexFileObjectIds {
    param([Parameter(Mandatory = $true)][string[]]$Pathspecs)

    $objectIdsByPath = [System.Collections.Generic.Dictionary[string, string]]::new([System.StringComparer]::Ordinal)
    if ($Pathspecs.Count -eq 0) {
        return $objectIdsByPath
    }

    $arguments = @("ls-files", "-s", "-z", "--") + $Pathspecs
    $result = Invoke-Git -Arguments $arguments
    if ([string]::IsNullOrEmpty($result.Stdout)) {
        return $objectIdsByPath
    }

    foreach ($record in $result.Stdout.Split([char]0, [System.StringSplitOptions]::RemoveEmptyEntries)) {
        $tabIndex = $record.IndexOf("`t")
        if ($tabIndex -lt 0) {
            throw "Unable to parse git ls-files record: $record"
        }

        $metadata = $record.Substring(0, $tabIndex)
        $path = $record.Substring($tabIndex + 1)
        $fields = @($metadata.Split(" ", [System.StringSplitOptions]::RemoveEmptyEntries))
        if ($fields.Count -lt 2) {
            throw "Unable to parse git ls-files metadata: $metadata"
        }

        $objectIdsByPath[$path] = $fields[1]
    }

    return $objectIdsByPath
}

function Read-AsciiLineFromBytes {
    param(
        [Parameter(Mandatory = $true)][byte[]]$Bytes,
        [Parameter(Mandatory = $true)][ref]$Offset
    )

    if ($Offset.Value -ge $Bytes.Length) {
        return $null
    }

    $start = $Offset.Value
    while ($Offset.Value -lt $Bytes.Length -and $Bytes[$Offset.Value] -ne 10) {
        $Offset.Value++
    }

    $length = $Offset.Value - $start
    $line = [System.Text.Encoding]::ASCII.GetString($Bytes, $start, $length).TrimEnd("`r")
    if ($Offset.Value -lt $Bytes.Length -and $Bytes[$Offset.Value] -eq 10) {
        $Offset.Value++
    }

    $line
}

function Get-IndexBlobTexts {
    param([Parameter(Mandatory = $true)][string[]]$ObjectIds)

    $textsByObjectId = [System.Collections.Generic.Dictionary[string, string]]::new([System.StringComparer]::OrdinalIgnoreCase)
    $orderedObjectIds = [System.Collections.Generic.List[string]]::new()
    $seenObjectIds = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::OrdinalIgnoreCase)
    foreach ($objectId in $ObjectIds) {
        if ($seenObjectIds.Add($objectId)) {
            [void]$orderedObjectIds.Add($objectId)
        }
    }

    if ($orderedObjectIds.Count -eq 0) {
        return $textsByObjectId
    }

    $batchInput = ($orderedObjectIds -join "`n") + "`n"
    $sizeCheck = Invoke-NativeWithInput -FileName "git" -Arguments @("cat-file", "--batch-check=%(objectname) %(objecttype) %(objectsize)") -InputText $batchInput
    if ($sizeCheck.ExitCode -ne 0) {
        throw "git cat-file --batch-check failed:`n$($sizeCheck.Output)"
    }

    $totalByteCount = [int64]0
    foreach ($rawLine in $sizeCheck.Stdout -split "`n") {
        $line = $rawLine.Trim()
        if ([string]::IsNullOrWhiteSpace($line)) {
            continue
        }

        $fields = @($line.Split(" ", [System.StringSplitOptions]::RemoveEmptyEntries))
        if ($fields.Count -ne 3 -or $fields[1] -ne "blob") {
            throw "Unexpected git cat-file --batch-check line: $line"
        }

        $totalByteCount += [int64]::Parse($fields[2], [System.Globalization.CultureInfo]::InvariantCulture)
        if ($totalByteCount -gt $script:MaxBatchedBlobBytes) {
            throw "Staged blob batch is $totalByteCount bytes, above the $script:MaxBatchedBlobBytes byte pre-commit limit. Split oversized .llm files and run ./scripts/generate-skills-index.sh."
        }
    }

    $result = Invoke-NativeBytesWithInput -FileName "git" -Arguments @("cat-file", "--batch") -InputText $batchInput
    if ($result.ExitCode -ne 0) {
        throw "git cat-file --batch failed:`n$($result.Output)"
    }

    $offset = 0
    $utf8NoBom = [System.Text.UTF8Encoding]::new($false)
    foreach ($requestedObjectId in $orderedObjectIds) {
        $header = Read-AsciiLineFromBytes -Bytes $result.StdoutBytes -Offset ([ref]$offset)
        if ($null -eq $header) {
            throw "git cat-file --batch ended before object $requestedObjectId"
        }

        $headerFields = @($header.Split(" ", [System.StringSplitOptions]::RemoveEmptyEntries))
        if ($headerFields.Count -ne 3 -or $headerFields[1] -ne "blob") {
            throw "Unexpected git cat-file header for ${requestedObjectId}: $header"
        }

        $objectId = $headerFields[0]
        if (-not [System.StringComparer]::OrdinalIgnoreCase.Equals($objectId, $requestedObjectId)) {
            throw "git cat-file --batch returned $objectId while reading $requestedObjectId"
        }

        $byteCount = [int64]::Parse($headerFields[2], [System.Globalization.CultureInfo]::InvariantCulture)
        if ($byteCount -gt [int]::MaxValue) {
            throw "Skill file blob is too large to read in pre-commit: $objectId"
        }
        if ($offset + $byteCount -gt $result.StdoutBytes.Length) {
            throw "git cat-file --batch returned truncated content for $objectId"
        }

        $content = $utf8NoBom.GetString($result.StdoutBytes, $offset, [int]$byteCount)
        $textsByObjectId[$objectId] = $content
        $offset += [int]$byteCount
        if ($offset -lt $result.StdoutBytes.Length -and $result.StdoutBytes[$offset] -eq 10) {
            $offset++
        }
    }

    return $textsByObjectId
}

function Add-IndexTextCache {
    param([string[]]$Pathspecs)

    $objectIdsByPath = Get-IndexFileObjectIds -Pathspecs $Pathspecs
    if ($objectIdsByPath.Count -eq 0) {
        return
    }

    $paths = [string[]]@($objectIdsByPath.Keys)
    $blobTextsByObjectId = Get-IndexBlobTexts -ObjectIds ([string[]]@($paths | ForEach-Object { $objectIdsByPath[$_] }))
    foreach ($path in $paths) {
        $objectId = $objectIdsByPath[$path]
        if ($blobTextsByObjectId.ContainsKey($objectId)) {
            $script:IndexTextCache[$path] = $blobTextsByObjectId[$objectId]
        }
    }
}

function Get-LineCount {
    param([AllowNull()][string]$Text)

    if ([string]::IsNullOrEmpty($Text)) {
        return 0
    }

    $normalized = $Text -replace "`r`n", "`n" -replace "`r", "`n"
    $count = $normalized.Split("`n").Count
    if ($normalized.EndsWith("`n")) {
        $count--
    }
    $count
}

function Set-IndexText {
    param(
        [Parameter(Mandatory = $true)][string]$Path,
        [Parameter(Mandatory = $true)][string]$Content
    )

    $tempPath = [System.IO.Path]::GetTempFileName()
    try {
        [System.IO.File]::WriteAllText(
            $tempPath,
            $Content,
            [System.Text.UTF8Encoding]::new($false)
        )
        $hash = (Invoke-Git -Arguments @("hash-object", "-w", "--", $tempPath)).Stdout.Trim()
        [void](Invoke-Git -Arguments @("update-index", "--add", "--cacheinfo", "100644", $hash, $Path))
        $script:IndexTextCache[$Path] = $Content
        $hash
    } finally {
        Remove-Item -LiteralPath $tempPath -Force -ErrorAction SilentlyContinue
    }
}

function Get-StagedAddedLines {
    param([string[]]$Pathspecs)

    $arguments = @("diff", "--cached", "--unified=0", "--no-color", "--diff-filter=ACMR", "--") + $Pathspecs
    (Invoke-Git -Arguments $arguments).Stdout
}

function Test-FastHookSource {
    if ($null -ne $script:PreloadError) {
        Skip "Hook speed policy" "staged content preload failed"
        return
    }

    $files = @(
        ".githooks/pre-commit",
        ".githooks/pre-push",
        "scripts/hooks/pre-commit.ps1",
        "scripts/hooks/pre-push.ps1"
    )
    $stagedHookFiles = @($script:StagedFiles | Where-Object { $files -contains $_ })
    if ($stagedHookFiles.Count -eq 0) {
        Skip "Hook speed policy" "no hook files staged"
        return
    }

    $slowCommandPattern = "^\s*(&\s*)?[""']?cargo[""']?\s+(fmt|clippy|test|doc|check|build|install)\b|^\s*(&\s*)?[""']?npm[""']?\s+(install|ci)\b|^\s*(&\s*)?[""']?npx[""']?\b|Invoke-Native\s+-FileName\s+[""'](cargo|npm|npx)[""']|Start-Process\s+[""']?(cargo|npm|npx)[""']?"
    $violations = [System.Collections.Generic.List[string]]::new()

    foreach ($file in $stagedHookFiles) {
        $content = Get-IndexText -Path $file
        if ($null -eq $content) {
            continue
        }

        $lineNumber = 0
        foreach ($line in $content -split "`r?`n") {
            $lineNumber++
            $trimmed = $line.TrimStart()
            if ($trimmed.StartsWith("#")) {
                continue
            }
            if ($trimmed -match $slowCommandPattern) {
                [void]$violations.Add("${file}:${lineNumber}: $trimmed")
            }
        }
    }

    if ($violations.Count -gt 0) {
        Fail "Hook speed policy" "Git hooks must not run slow semantic or install commands.`n$($violations -join "`n")"
    } else {
        Pass "Hook speed policy"
    }
}

function Test-Whitespace {
    $result = Invoke-Native -FileName "git" -Arguments @("diff", "--cached", "--check")
    if ($result.ExitCode -eq 0) {
        Pass "Staged diff whitespace"
    } else {
        Fail "Staged diff whitespace" "Fix whitespace errors in the staged diff.`n$($result.Output.Trim())"
    }
}

function Test-RustAddedPanicPatterns {
    if (@($script:StagedFiles | Where-Object { $_.StartsWith("src/") -and $_.EndsWith(".rs") }).Count -eq 0) {
        Skip "Rust panic patterns" "no production Rust files staged"
        return
    }

    $diff = Get-StagedAddedLines -Pathspecs @("src/*.rs", ":(glob)src/**/*.rs")
    $currentFile = ""
    $violations = [System.Collections.Generic.List[string]]::new()

    foreach ($rawLine in $diff -split "`n") {
        $line = $rawLine.TrimEnd("`r")
        if ($line.StartsWith("+++ b/")) {
            $currentFile = $line.Substring(6)
            continue
        }
        if (-not $line.StartsWith("+") -or $line.StartsWith("+++")) {
            continue
        }
        if (-not $currentFile.StartsWith("src/")) {
            continue
        }

        $added = $line.Substring(1)
        $trimmed = $added.TrimStart()
        if ($trimmed.StartsWith("//")) {
            continue
        }

        if ($added -match "(\.unwrap\s*\(|\.expect\s*\(|\bpanic!\s*\(|\btodo!\s*\(|\bunimplemented!\s*\(|\bunreachable!\s*\()") {
            [void]$violations.Add("${currentFile}: $trimmed")
        }
    }

    if ($violations.Count -gt 0) {
        Fail "Rust panic patterns" "Production Rust additions include panic-prone patterns.`n$($violations -join "`n")"
    } else {
        Pass "Rust panic patterns"
    }
}

function New-SkillsIndexContent {
    $warning = "> WARNING: Auto-generated by ``scripts/generate-skills-index.sh``. Do not edit manually."
    $lines = [System.Collections.Generic.List[string]]::new()
    [void]$lines.Add("# Skills Index")
    [void]$lines.Add("")
    [void]$lines.Add($warning)
    [void]$lines.Add("")
    [void]$lines.Add("## Files")
    [void]$lines.Add("")

    $objectIdsByPath = Get-IndexSkillFileObjectIds
    $paths = [string[]]@($objectIdsByPath.Keys)
    [array]::Sort($paths, [System.StringComparer]::Ordinal)
    $blobTextsByObjectId = Get-IndexBlobTexts -ObjectIds ([string[]]@($paths | ForEach-Object { $objectIdsByPath[$_] }))

    foreach ($path in $paths) {
        $fileName = Get-GitPathFileName -Path $path
        $title = ""
        $objectId = $objectIdsByPath[$path]
        if (-not $blobTextsByObjectId.ContainsKey($objectId)) {
            continue
        }
        $content = $blobTextsByObjectId[$objectId]
        foreach ($line in $content -split "`r?`n") {
            if ($line.StartsWith("# Skill:")) {
                $title = $line.Substring("# Skill:".Length).Trim()
                break
            }
        }
        if ([string]::IsNullOrWhiteSpace($title)) {
            [void]$lines.Add("- [$fileName](./$fileName)")
        } else {
            [void]$lines.Add("- [$title](./$fileName)")
        }
    }

    ($lines -join "`n") + "`n"
}

function Repair-SkillsIndexIfNeeded {
    $triggered = @($script:StagedFiles | Where-Object {
            $_ -eq ".llm/context.md" -or
            $_ -eq "scripts/generate-skills-index.sh" -or
            ($_.StartsWith(".llm/skills/") -and $_.EndsWith(".md"))
        }).Count -gt 0
    if (-not $triggered) {
        Skip "Skills index freshness" "no skills index inputs staged"
        return
    }

    try {
        $expected = New-SkillsIndexContent
    } catch {
        Fail "Skills index freshness" $_.Exception.Message
        return
    }
    $actual = Get-IndexText -Path ".llm/skills/index.md"
    if ($null -eq $actual) { $actual = "" }

    if ($actual -eq $expected) {
        Pass "Skills index freshness"
        return
    }

    $expectedHash = Set-IndexText -Path ".llm/skills/index.md" -Content $expected
    $updatedHash = Get-IndexObjectId -Path ".llm/skills/index.md"

    if ($updatedHash -eq $expectedHash) {
        Pass "Skills index freshness (auto-repaired)"
    } else {
        Fail "Skills index freshness" "Unable to regenerate .llm/skills/index.md."
    }
}

function Test-LlmFileSizes {
    if ($null -ne $script:PreloadError) {
        Skip "LLM file sizes" "staged content preload failed"
        return
    }

    $llmFiles = @($script:StagedFiles | Where-Object { $_.StartsWith(".llm/") -and $_.EndsWith(".md") })
    if ($llmFiles.Count -eq 0) {
        Skip "LLM file sizes" "no .llm/*.md files staged"
        return
    }

    $violations = [System.Collections.Generic.List[string]]::new()
    foreach ($file in $llmFiles) {
        $content = Get-IndexText -Path $file
        if ($null -eq $content) {
            continue
        }
        $lineCount = Get-LineCount -Text $content
        if ($lineCount -gt 300) {
            [void]$violations.Add("${file}: ${lineCount} lines")
        }
    }

    if ($violations.Count -gt 0) {
        Fail "LLM file sizes" "Split oversized .llm files to 300 lines or fewer.`n$($violations -join "`n")"
    } else {
        Pass "LLM file sizes"
    }
}

function Test-ReadmeBadges {
    if ($null -ne $script:PreloadError) {
        Skip "README badge styles" "staged content preload failed"
        return
    }

    if ($script:StagedFiles -notcontains "README.md") {
        Skip "README badge styles" "README.md not staged"
        return
    }

    $readme = Get-IndexText -Path "README.md"
    if ($null -eq $readme) {
        Skip "README badge styles" "README.md not present in index"
        return
    }

    $violations = [System.Collections.Generic.List[string]]::new()
    $lineNumber = 0
    foreach ($line in $readme -split "`r?`n") {
        $lineNumber++
        foreach ($match in [regex]::Matches($line, "https://img\.shields\.io/[^'""\)\>\s]+")) {
            $url = $match.Value
            if ($url -notmatch "[?&]style=for-the-badge([&#]|$)") {
                [void]$violations.Add("README.md:${lineNumber}: $url")
            }
        }
    }

    if ($violations.Count -gt 0) {
        Fail "README badge styles" "All Shields.io badges must use style=for-the-badge.`n$($violations -join "`n")"
    } else {
        Pass "README badge styles"
    }
}

$timer = [System.Diagnostics.Stopwatch]::StartNew()
$script:RepoRoot = (Invoke-Git -Arguments @("rev-parse", "--show-toplevel")).Stdout.Trim()
Set-Location $script:RepoRoot
$script:StagedFiles = @(Get-StagedFiles)
$hookSourceFiles = @(
    ".githooks/pre-commit",
    ".githooks/pre-push",
    "scripts/hooks/pre-commit.ps1",
    "scripts/hooks/pre-push.ps1"
)
$preloadPaths = @($script:StagedFiles | Where-Object {
        ($hookSourceFiles -contains $_) -or
        ($_.StartsWith(".llm/") -and $_.EndsWith(".md")) -or
        $_ -eq "README.md"
    })
try {
    Add-IndexTextCache -Pathspecs $preloadPaths
} catch {
    $script:PreloadError = $_.Exception.Message
}

Write-Step "Running fast last-resort checks..."
if ($null -ne $script:PreloadError) {
    Fail "Staged content preload" $script:PreloadError
}
Test-FastHookSource
Test-Whitespace
Test-RustAddedPanicPatterns
Repair-SkillsIndexIfNeeded
Test-LlmFileSizes
Test-ReadmeBadges

$timer.Stop()
Write-Host "[pre-commit] Completed in $($timer.ElapsedMilliseconds)ms"

if ($script:Failed -gt 0) {
    Write-Host "[pre-commit] $script:Failed failed, $script:Passed passed, $script:Skipped skipped."
    Write-Host "[pre-commit] Slow semantic checks stay outside git hooks. Run:"
    Write-Host "  cargo fmt --check"
    Write-Host "  cargo clippy --locked --all-targets --all-features -- -D warnings"
    Write-Host "  cargo test --locked --all-features"
    Write-Host "  ./scripts/run-local-ci.sh"
    exit 1
}

Write-Host "[pre-commit] All checks passed ($script:Passed passed, $script:Skipped skipped)."
exit 0
