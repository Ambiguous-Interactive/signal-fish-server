#requires -Version 7.0
param(
    [switch]$SourceOnly,
    [switch]$Worktree,
    [switch]$EnforceBudget
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$script:PreCommitTimer = [System.Diagnostics.Stopwatch]::StartNew()
$script:Passed = 0
$script:Failed = 0
$script:Skipped = 0
$script:IndexTextCache = [System.Collections.Generic.Dictionary[string, string]]::new([System.StringComparer]::Ordinal)
$script:WorktreeTextCache = [System.Collections.Generic.Dictionary[string, string]]::new([System.StringComparer]::Ordinal)
# Keep hooks sub-second on normal local workloads and auto-report slowest checks if exceeded.
$script:MaxBatchedBlobBytes = 2 * 1024 * 1024
# Batch staged blob reads only when enough files are involved to beat per-file git show calls.
$script:PreloadBatchThreshold = 3
$script:PreloadError = $null
$script:InspectWorktree = $false
$script:HookBudgetMs = 1000
$script:CheckTimings = [System.Collections.Generic.List[object]]::new()
$script:HookPolicyFiles = @(
    ".githooks/pre-commit",
    ".githooks/pre-push",
    "scripts/hooks/native-process.ps1",
    "scripts/hooks/pre-commit.ps1",
    "scripts/hooks/pre-push.ps1"
)
$script:HookPolicyChangedFiles = @()

. (Join-Path $PSScriptRoot "native-process.ps1")

function Write-Step {
    param([string]$Message)
    Write-Host "[pre-commit] $Message"
}

function Write-Profile {
    param(
        [string]$Name,
        [long]$ElapsedMilliseconds
    )

    if ($env:SIGNAL_FISH_HOOK_PROFILE -eq "1") {
        Write-Host "[pre-commit] PROFILE: $Name ${ElapsedMilliseconds}ms"
    }
}

function Invoke-Check {
    param(
        [Parameter(Mandatory = $true)][string]$Name,
        [Parameter(Mandatory = $true)][scriptblock]$Check
    )

    $failedBefore = $script:Failed
    $timer = [System.Diagnostics.Stopwatch]::StartNew()
    try {
        & $Check
    } finally {
        $timer.Stop()
        [void]$script:CheckTimings.Add([pscustomobject]@{
                Name = $Name
                ElapsedMilliseconds = [long]$timer.ElapsedMilliseconds
            })
        Write-Profile -Name $Name -ElapsedMilliseconds $timer.ElapsedMilliseconds
    }

    return $script:Failed -eq $failedBefore
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

function Get-StagedFiles {
    param([string[]]$Pathspecs = @())

    $arguments = @("diff", "--cached", "--name-only", "-z", "--diff-filter=ACDMR", "--") + $Pathspecs
    $result = Invoke-Git -Arguments $arguments
    if ([string]::IsNullOrEmpty($result.Stdout)) {
        return @()
    }

    $result.Stdout.Split([char]0, [System.StringSplitOptions]::RemoveEmptyEntries)
}

function Add-UniqueGitPaths {
    param(
        [Parameter(Mandatory = $true)][AllowEmptyString()][string]$NulDelimitedPaths,
        [Parameter(Mandatory = $true)][AllowEmptyCollection()][System.Collections.Generic.List[string]]$OrderedPaths,
        [Parameter(Mandatory = $true)][AllowEmptyCollection()][System.Collections.Generic.HashSet[string]]$SeenPaths
    )

    if ([string]::IsNullOrEmpty($NulDelimitedPaths)) {
        return
    }

    foreach ($path in $NulDelimitedPaths.Split([char]0, [System.StringSplitOptions]::RemoveEmptyEntries)) {
        if ($SeenPaths.Add($path)) {
            [void]$OrderedPaths.Add($path)
        }
    }
}

function Get-WorktreeChangedFiles {
    param([string[]]$Pathspecs = @())

    $orderedPaths = [System.Collections.Generic.List[string]]::new()
    $seenPaths = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::Ordinal)
    $statusArgs = @("status", "--porcelain=v1", "-z", "--") + $Pathspecs
    $result = Invoke-Git -Arguments $statusArgs
    if ([string]::IsNullOrEmpty($result.Stdout)) {
        return @()
    }

    $records = @($result.Stdout.Split([char]0, [System.StringSplitOptions]::RemoveEmptyEntries))
    for ($index = 0; $index -lt $records.Count; $index++) {
        $record = $records[$index]
        if ($record.Length -lt 4) {
            continue
        }

        $status = $record.Substring(0, 2)
        $path = $record.Substring(3)
        if ($seenPaths.Add($path)) {
            [void]$orderedPaths.Add($path)
        }

        if ($status[0] -in @([char]"R", [char]"C") -or $status[1] -in @([char]"R", [char]"C")) {
            $index++
        }
    }

    foreach ($path in $orderedPaths) {
        $path
    }
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

function Get-WorktreeText {
    param([Parameter(Mandatory = $true)][string]$Path)

    if ($script:WorktreeTextCache.ContainsKey($Path)) {
        return $script:WorktreeTextCache[$Path]
    }

    $absolutePath = Join-Path $script:RepoRoot $Path
    if (-not (Test-Path -LiteralPath $absolutePath)) {
        return $null
    }

    $text = [System.IO.File]::ReadAllText($absolutePath)
    $script:WorktreeTextCache[$Path] = $text
    $text
}

function Get-PolicyText {
    param([Parameter(Mandatory = $true)][string]$Path)

    if ($script:InspectWorktree) {
        return Get-WorktreeText -Path $Path
    }

    Get-IndexText -Path $Path
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
    param([Parameter(Mandatory = $true)][AllowEmptyCollection()][string[]]$Pathspecs)

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
    param([AllowEmptyCollection()][string[]]$Pathspecs)

    if ($Pathspecs.Count -eq 0) {
        return
    }

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

function Test-FastHookSource {
    if ($null -ne $script:PreloadError) {
        Skip "Hook speed policy" "staged content preload failed"
        return
    }

    $stagedHookFiles = [string[]]@(if ($script:HookPolicyChangedFiles.Count -gt 0) {
        $script:HookPolicyChangedFiles
    } else {
        $script:StagedFiles | Where-Object { $script:HookPolicyFiles -contains $_ }
    })
    if ($stagedHookFiles.Count -eq 0) {
        Skip "Hook speed policy" "no hook files staged"
        return
    }

    $slowCommandPattern = "^\s*(&\s*)?[""']?cargo[""']?\s+(fmt|clippy|test|doc|check|build|install)\b|^\s*(&\s*)?[""']?npm[""']?\s+(install|ci)\b|^\s*(&\s*)?[""']?npx[""']?\b|Invoke-Native\s+-FileName\s+[""'](cargo|npm|npx)[""']|Start-Process\s+[""']?(cargo|npm|npx)[""']?"
    $violations = [System.Collections.Generic.List[string]]::new()

    foreach ($file in $stagedHookFiles) {
        $content = Get-PolicyText -Path $file
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

function Add-StagedContentPreload {
    $skillsIndexTriggered = @($script:StagedFiles | Where-Object {
            $_ -eq "scripts/generate-skills-index.sh" -or
            ($_.StartsWith(".llm/skills/") -and $_.EndsWith(".md"))
        }).Count -gt 0
    $preloadPaths = @($script:StagedFiles | Where-Object {
            ($script:HookPolicyFiles -contains $_) -or
            ($_.StartsWith(".llm/") -and $_.EndsWith(".md") -and
                -not ($skillsIndexTriggered -and $_.StartsWith(".llm/skills/"))) -or
            $_ -eq "README.md"
        })

    try {
        $preloadTimer = [System.Diagnostics.Stopwatch]::StartNew()
        if ($preloadPaths.Count -gt $script:PreloadBatchThreshold) {
            Add-IndexTextCache -Pathspecs $preloadPaths
        }
    } catch {
        $script:PreloadError = $_.Exception.Message
    } finally {
        $preloadTimer.Stop()
        Write-Profile -Name "Staged content preload" -ElapsedMilliseconds $preloadTimer.ElapsedMilliseconds
    }
}

function Test-ProductionRustSourcePath {
    param([Parameter(Mandatory = $true)][string]$Path)

    if (-not ($Path.StartsWith("src/") -and $Path.EndsWith(".rs"))) {
        return $false
    }

    $fileName = Get-GitPathFileName -Path $Path
    if ($fileName -in @("test.rs", "tests.rs") -or $fileName.EndsWith("_test.rs") -or $fileName.EndsWith("_tests.rs")) {
        return $false
    }

    -not ($Path.Contains("/test/") -or $Path.Contains("/tests/"))
}

function Add-LineRange {
    param(
        [Parameter(Mandatory = $true)][AllowEmptyCollection()][System.Collections.Generic.HashSet[int]]$LineSet,
        [Parameter(Mandatory = $true)][int]$StartLine,
        [Parameter(Mandatory = $true)][int]$EndLine
    )

    for ($lineNumber = $StartLine; $lineNumber -le $EndLine; $lineNumber++) {
        [void]$LineSet.Add($lineNumber)
    }
}

function Test-RustCfgTestAttributeLine {
    param([Parameter(Mandatory = $true)][AllowEmptyString()][string]$Line)

    if (-not $Line.TrimStart().StartsWith("#[")) {
        return $false
    }

    if ($Line -notmatch "#\[\s*cfg\s*\((?<expr>[^\]]*)\)\s*\]") {
        return $false
    }

    Test-RustCfgExpressionRequiresTest -Expression $Matches["expr"]
}

function Test-RustDirectTestAttributeLine {
    param([Parameter(Mandatory = $true)][AllowEmptyString()][string]$Line)

    if (-not $Line.TrimStart().StartsWith("#[")) {
        return $false
    }

    $Line -match "#\[\s*(test|tokio::test|async_std::test|rstest)(\(|\])"
}

function Split-RustCfgArguments {
    param([Parameter(Mandatory = $true)][AllowEmptyString()][string]$Expression)

    $arguments = [System.Collections.Generic.List[string]]::new()
    $start = 0
    $depth = 0
    $inString = $false
    $escape = $false

    for ($index = 0; $index -lt $Expression.Length; $index++) {
        $char = $Expression[$index]
        if ($inString) {
            if ($escape) {
                $escape = $false
            } elseif ($char -eq [char]"\") {
                $escape = $true
            } elseif ($char -eq [char]"""") {
                $inString = $false
            }
            continue
        }

        if ($char -eq [char]"""") {
            $inString = $true
            $escape = $false
        } elseif ($char -eq [char]"(") {
            $depth++
        } elseif ($char -eq [char]")") {
            if ($depth -gt 0) {
                $depth--
            }
        } elseif ($char -eq [char]"," -and $depth -eq 0) {
            [void]$arguments.Add($Expression.Substring($start, $index - $start).Trim())
            $start = $index + 1
        }
    }

    $tail = $Expression.Substring($start).Trim()
    if (-not [string]::IsNullOrWhiteSpace($tail)) {
        [void]$arguments.Add($tail)
    }

    $arguments.ToArray()
}

function Test-RustCfgExpressionRequiresTest {
    param([Parameter(Mandatory = $true)][AllowEmptyString()][string]$Expression)

    $expr = $Expression.Trim()
    if ($expr -eq "test") {
        return $true
    }
    if ($expr.StartsWith("not(")) {
        return $false
    }
    if ($expr.StartsWith("all(") -and $expr.EndsWith(")")) {
        $inner = $expr.Substring(4, $expr.Length - 5)
        foreach ($argument in (Split-RustCfgArguments -Expression $inner)) {
            if (Test-RustCfgExpressionRequiresTest -Expression $argument) {
                return $true
            }
        }
        return $false
    }
    if ($expr.StartsWith("any(") -and $expr.EndsWith(")")) {
        $arguments = @(Split-RustCfgArguments -Expression $expr.Substring(4, $expr.Length - 5))
        if ($arguments.Count -eq 0) {
            return $false
        }
        foreach ($argument in $arguments) {
            if (-not (Test-RustCfgExpressionRequiresTest -Expression $argument)) {
                return $false
            }
        }
        return $true
    }

    $false
}

function Get-RustAttributedItemLine {
    param(
        [Parameter(Mandatory = $true)][AllowEmptyString()][string[]]$Lines,
        [Parameter(Mandatory = $true)][int]$StartIndex
    )

    for ($index = $StartIndex; $index -lt $Lines.Count; $index++) {
        $remaining = $Lines[$index].Trim()
        if ([string]::IsNullOrWhiteSpace($remaining)) {
            continue
        }

        while ($remaining.StartsWith("#[")) {
            $attributeEnd = $remaining.IndexOf("]")
            if ($attributeEnd -lt 0) {
                break
            }
            $remaining = $remaining.Substring($attributeEnd + 1).TrimStart()
        }

        if ([string]::IsNullOrWhiteSpace($remaining)) {
            continue
        }

        return $index
    }

    -1
}

function Get-RustItemEndLine {
    param(
        [Parameter(Mandatory = $true)][AllowEmptyString()][string[]]$Lines,
        [Parameter(Mandatory = $true)][int]$StartIndex
    )

    $depth = 0
    $opened = $false
    $blockCommentDepth = 0
    $inString = $false
    $inChar = $false
    $escape = $false
    $rawTerminator = ""

    for ($index = $StartIndex; $index -lt $Lines.Count; $index++) {
        $line = $Lines[$index]
        $charIndex = 0
        while ($charIndex -lt $line.Length) {
            if ($blockCommentDepth -gt 0) {
                if ($charIndex + 1 -lt $line.Length -and $line[$charIndex] -eq [char]"/" -and $line[$charIndex + 1] -eq [char]"*") {
                    $blockCommentDepth++
                    $charIndex += 2
                    continue
                }
                if ($charIndex + 1 -lt $line.Length -and $line[$charIndex] -eq [char]"*" -and $line[$charIndex + 1] -eq [char]"/") {
                    $blockCommentDepth--
                    $charIndex += 2
                    continue
                }
                $charIndex++
                continue
            }

            if (-not [string]::IsNullOrEmpty($rawTerminator)) {
                if ($line.Substring($charIndex).StartsWith($rawTerminator)) {
                    $charIndex += $rawTerminator.Length
                    $rawTerminator = ""
                    continue
                }
                $charIndex++
                continue
            }

            if ($inString) {
                if ($escape) {
                    $escape = $false
                } elseif ($line[$charIndex] -eq [char]"\") {
                    $escape = $true
                } elseif ($line[$charIndex] -eq [char]"""") {
                    $inString = $false
                }
                $charIndex++
                continue
            }

            if ($inChar) {
                if ($escape) {
                    $escape = $false
                } elseif ($line[$charIndex] -eq [char]"\") {
                    $escape = $true
                } elseif ($line[$charIndex] -eq [char]"'") {
                    $inChar = $false
                }
                $charIndex++
                continue
            }

            if ($charIndex + 1 -lt $line.Length -and $line[$charIndex] -eq [char]"/" -and $line[$charIndex + 1] -eq [char]"/") {
                break
            }
            if ($charIndex + 1 -lt $line.Length -and $line[$charIndex] -eq [char]"/" -and $line[$charIndex + 1] -eq [char]"*") {
                $blockCommentDepth = 1
                $charIndex += 2
                continue
            }

            $rawStart = -1
            if ($line[$charIndex] -eq [char]"r") {
                $rawStart = $charIndex
            } elseif ($charIndex + 1 -lt $line.Length -and $line[$charIndex] -eq [char]"b" -and $line[$charIndex + 1] -eq [char]"r") {
                $rawStart = $charIndex + 1
            }
            if ($rawStart -ge 0) {
                $probe = $rawStart + 1
                while ($probe -lt $line.Length -and $line[$probe] -eq [char]"#") {
                    $probe++
                }
                if ($probe -lt $line.Length -and $line[$probe] -eq [char]"""") {
                    $hashes = $line.Substring($rawStart + 1, $probe - $rawStart - 1)
                    $rawTerminator = """" + $hashes
                    $charIndex = $probe + 1
                    continue
                }
            }

            if ($line[$charIndex] -eq [char]"""" -or (
                    $charIndex + 1 -lt $line.Length -and
                    $line[$charIndex] -eq [char]"b" -and
                    $line[$charIndex + 1] -eq [char]""""
                )) {
                $inString = $true
                $escape = $false
                $charIndex += if ($line[$charIndex] -eq [char]"b") { 2 } else { 1 }
                continue
            }

            if ($line[$charIndex] -eq [char]"'") {
                $nextIndex = $charIndex + 1
                if ($nextIndex -lt $line.Length -and ([string]$line[$nextIndex]) -match "[A-Za-z_]") {
                    $afterIdentifier = $nextIndex + 1
                    while ($afterIdentifier -lt $line.Length -and ([string]$line[$afterIdentifier]) -match "[A-Za-z0-9_]") {
                        $afterIdentifier++
                    }
                    if ($afterIdentifier -lt $line.Length -and $line[$afterIdentifier] -eq [char]"'") {
                        $charIndex = $afterIdentifier + 1
                        continue
                    }

                    $charIndex++
                    continue
                }

                $probe = $nextIndex
                $escapedChar = $false
                $charLiteralEnd = -1
                while ($probe -lt $line.Length) {
                    if ($escapedChar) {
                        $escapedChar = $false
                    } elseif ($line[$probe] -eq [char]"\") {
                        $escapedChar = $true
                    } elseif ($line[$probe] -eq [char]"'") {
                        $charLiteralEnd = $probe
                        break
                    }
                    $probe++
                }
                if ($charLiteralEnd -ge 0) {
                    $charIndex = $charLiteralEnd + 1
                    continue
                }

                $charIndex++
                continue
            }

            if ($line[$charIndex] -eq [char]"{") {
                $opened = $true
                $depth++
            } elseif ($line[$charIndex] -eq [char]"}") {
                $depth--
            } elseif ($line[$charIndex] -eq [char]";" -and -not $opened) {
                return $index + 1
            }

            $charIndex++
        }

        if ($opened -and $depth -le 0) {
            return $index + 1
        }
    }

    $Lines.Count
}

function Get-RustTestLineSet {
    param(
        [AllowNull()][string]$Content,
        [int]$MaxLine = 0
    )

    $testLines = [System.Collections.Generic.HashSet[int]]::new()
    if ([string]::IsNullOrEmpty($Content)) {
        Write-Output -NoEnumerate $testLines
        return
    }

    $lines = [string[]]($Content -split "`r?`n")
    $lineLimit = $lines.Count
    if ($MaxLine -gt 0) {
        $lineLimit = [System.Math]::Min($MaxLine, $lines.Count)
    }

    for ($index = 0; $index -lt $lineLimit; $index++) {
        $trimmed = $lines[$index].Trim()

        if ((Test-RustCfgTestAttributeLine -Line $trimmed) -or (Test-RustDirectTestAttributeLine -Line $trimmed)) {
            $itemIndex = Get-RustAttributedItemLine -Lines $lines -StartIndex $index
            if ($itemIndex -lt 0) {
                continue
            }

            $endLine = Get-RustItemEndLine -Lines $lines -StartIndex $itemIndex
            Add-LineRange -LineSet $testLines -StartLine ($index + 1) -EndLine ([System.Math]::Min($endLine, $lineLimit))
            $index = $endLine - 1
        }
    }

    Write-Output -NoEnumerate $testLines
}

function Test-RustPanicMacroLine {
    param([Parameter(Mandatory = $true)][AllowEmptyString()][string]$Line)

    $macroNames = [System.Collections.Generic.HashSet[string]]::new(
        [string[]]@("panic", "todo", "unimplemented", "unreachable"),
        [System.StringComparer]::Ordinal
    )

    $charIndex = 0
    $inString = $false
    $inChar = $false
    $escape = $false
    $rawTerminator = ""
    while ($charIndex -lt $Line.Length) {
        if (-not [string]::IsNullOrEmpty($rawTerminator)) {
            if ($Line.Substring($charIndex).StartsWith($rawTerminator, [System.StringComparison]::Ordinal)) {
                $charIndex += $rawTerminator.Length
                $rawTerminator = ""
                continue
            }
            $charIndex++
            continue
        }

        if ($inString) {
            if ($escape) {
                $escape = $false
            } elseif ($Line[$charIndex] -eq [char]"\") {
                $escape = $true
            } elseif ($Line[$charIndex] -eq [char]"""") {
                $inString = $false
            }
            $charIndex++
            continue
        }

        if ($inChar) {
            if ($escape) {
                $escape = $false
            } elseif ($Line[$charIndex] -eq [char]"\") {
                $escape = $true
            } elseif ($Line[$charIndex] -eq [char]"'") {
                $inChar = $false
            }
            $charIndex++
            continue
        }

        if ($charIndex + 1 -lt $Line.Length -and $Line[$charIndex] -eq [char]"/" -and $Line[$charIndex + 1] -eq [char]"/") {
            return $false
        }
        if ($charIndex + 1 -lt $Line.Length -and $Line[$charIndex] -eq [char]"/" -and $Line[$charIndex + 1] -eq [char]"*") {
            $end = $Line.IndexOf("*/", $charIndex + 2, [System.StringComparison]::Ordinal)
            if ($end -lt 0) {
                return $false
            }
            $charIndex = $end + 2
            continue
        }

        $rawStart = -1
        if ($Line[$charIndex] -eq [char]"r") {
            $rawStart = $charIndex
        } elseif ($charIndex + 1 -lt $Line.Length -and $Line[$charIndex] -eq [char]"b" -and $Line[$charIndex + 1] -eq [char]"r") {
            $rawStart = $charIndex + 1
        }
        if ($rawStart -ge 0) {
            $probe = $rawStart + 1
            while ($probe -lt $Line.Length -and $Line[$probe] -eq [char]"#") {
                $probe++
            }
            if ($probe -lt $Line.Length -and $Line[$probe] -eq [char]"""") {
                $hashes = $Line.Substring($rawStart + 1, $probe - $rawStart - 1)
                $rawTerminator = """" + $hashes
                $charIndex = $probe + 1
                continue
            }
        }

        if (
            $Line[$charIndex] -eq [char]"""" -or
            ($charIndex + 1 -lt $Line.Length -and $Line[$charIndex] -eq [char]"b" -and $Line[$charIndex + 1] -eq [char]"""")
        ) {
            $inString = $true
            $escape = $false
            $charIndex += if ($Line[$charIndex] -eq [char]"b") { 2 } else { 1 }
            continue
        }

        if ($Line[$charIndex] -eq [char]"'") {
            $nextIndex = $charIndex + 1
            if ($nextIndex -lt $Line.Length -and (Test-RustIdentifierStartChar -Char $Line[$nextIndex])) {
                $afterIdentifier = $nextIndex + 1
                while ($afterIdentifier -lt $Line.Length -and (Test-RustIdentifierChar -Char $Line[$afterIdentifier])) {
                    $afterIdentifier++
                }
                if ($afterIdentifier -lt $Line.Length -and $Line[$afterIdentifier] -ne [char]"'") {
                    $charIndex = $afterIdentifier
                    continue
                }
            }

            $inChar = $true
            $escape = $false
            $charIndex++
            continue
        }

        if (Test-RustIdentifierStartChar -Char $Line[$charIndex]) {
            $start = $charIndex
            $charIndex++
            while ($charIndex -lt $Line.Length -and (Test-RustIdentifierChar -Char $Line[$charIndex])) {
                $charIndex++
            }

            $identifier = $Line.Substring($start, $charIndex - $start)
            if ($macroNames.Contains($identifier)) {
                $probe = $charIndex
                $canProbe = $true
                foreach ($expected in @("!", "delimiter")) {
                    while ($probe -lt $Line.Length) {
                        if ([char]::IsWhiteSpace($Line[$probe])) {
                            $probe++
                            continue
                        }
                        if ($probe + 1 -lt $Line.Length -and $Line[$probe] -eq [char]"/" -and $Line[$probe + 1] -eq [char]"*") {
                            $end = $Line.IndexOf("*/", $probe + 2, [System.StringComparison]::Ordinal)
                            if ($end -lt 0) {
                                $canProbe = $false
                                break
                            }
                            $probe = $end + 2
                            continue
                        }
                        break
                    }
                    if (-not $canProbe -or $probe -ge $Line.Length) {
                        $canProbe = $false
                        break
                    }
                    if ($expected -eq "delimiter") {
                        if ($Line[$probe] -notin @([char]"(", [char]"[", [char]"{")) {
                            $canProbe = $false
                            break
                        }
                    } elseif ($Line[$probe] -ne [char]$expected) {
                        $canProbe = $false
                        break
                    }
                    $probe++
                }
                if ($canProbe) {
                    return $true
                }
            }
            continue
        }

        $charIndex++
    }

    $false
}

function Test-RustIdentifierStartChar {
    param([Parameter(Mandatory = $true)][char]$Char)

    $code = [int]$Char
    ($code -ge 65 -and $code -le 90) -or
    ($code -ge 97 -and $code -le 122) -or
    $Char -eq [char]"_"
}

function Test-RustIdentifierChar {
    param([Parameter(Mandatory = $true)][char]$Char)

    $code = [int]$Char
    ($code -ge 65 -and $code -le 90) -or
    ($code -ge 97 -and $code -le 122) -or
    ($code -ge 48 -and $code -le 57) -or
    $Char -eq [char]"_"
}

function Get-RustExecutablePanicMacroLineSet {
    param(
        [AllowNull()][string]$Content,
        [Parameter(Mandatory = $true)][AllowEmptyCollection()][int[]]$CandidateLines
    )

    $macroLines = [System.Collections.Generic.HashSet[int]]::new()
    if ([string]::IsNullOrEmpty($Content) -or $CandidateLines.Count -eq 0) {
        Write-Output -NoEnumerate $macroLines
        return
    }

    $candidateLineSet = [System.Collections.Generic.HashSet[int]]::new()
    $maxCandidateLine = 0
    foreach ($lineNumber in $CandidateLines) {
        [void]$candidateLineSet.Add($lineNumber)
        if ($lineNumber -gt $maxCandidateLine) {
            $maxCandidateLine = $lineNumber
        }
    }

    $lines = [string[]](($Content -replace "`r`n", "`n" -replace "`r", "`n") -split "`n")
    $lineLimit = [System.Math]::Min($maxCandidateLine, $lines.Count)
    $blockCommentDepth = 0
    $rawTerminator = ""
    $inString = $false
    $inChar = $false
    $escape = $false

    for ($index = 0; $index -lt $lineLimit; $index++) {
        $lineNumber = $index + 1
        $line = $lines[$index]
        $lineStartsInsideSuppressedContext =
            $blockCommentDepth -gt 0 -or
            -not [string]::IsNullOrEmpty($rawTerminator) -or
            $inString -or
            $inChar

        if (
            $candidateLineSet.Contains($lineNumber) -and
            -not $lineStartsInsideSuppressedContext -and
            (Test-RustPanicMacroLine -Line $line)
        ) {
            [void]$macroLines.Add($lineNumber)
        }

        $charIndex = 0
        while ($charIndex -lt $line.Length) {
            if ($blockCommentDepth -gt 0) {
                if ($charIndex + 1 -lt $line.Length -and $line[$charIndex] -eq [char]"/" -and $line[$charIndex + 1] -eq [char]"*") {
                    $blockCommentDepth++
                    $charIndex += 2
                    continue
                }
                if ($charIndex + 1 -lt $line.Length -and $line[$charIndex] -eq [char]"*" -and $line[$charIndex + 1] -eq [char]"/") {
                    $blockCommentDepth--
                    $charIndex += 2
                    continue
                }
                $charIndex++
                continue
            }

            if (-not [string]::IsNullOrEmpty($rawTerminator)) {
                if ($line.Substring($charIndex).StartsWith($rawTerminator, [System.StringComparison]::Ordinal)) {
                    $charIndex += $rawTerminator.Length
                    $rawTerminator = ""
                    continue
                }
                $charIndex++
                continue
            }

            if ($inString) {
                if ($escape) {
                    $escape = $false
                } elseif ($line[$charIndex] -eq [char]"\") {
                    $escape = $true
                } elseif ($line[$charIndex] -eq [char]"""") {
                    $inString = $false
                }
                $charIndex++
                continue
            }

            if ($inChar) {
                if ($escape) {
                    $escape = $false
                } elseif ($line[$charIndex] -eq [char]"\") {
                    $escape = $true
                } elseif ($line[$charIndex] -eq [char]"'") {
                    $inChar = $false
                }
                $charIndex++
                continue
            }

            if ($charIndex + 1 -lt $line.Length -and $line[$charIndex] -eq [char]"/" -and $line[$charIndex + 1] -eq [char]"/") {
                break
            }
            if ($charIndex + 1 -lt $line.Length -and $line[$charIndex] -eq [char]"/" -and $line[$charIndex + 1] -eq [char]"*") {
                $blockCommentDepth = 1
                $charIndex += 2
                continue
            }

            $rawStart = -1
            if ($line[$charIndex] -eq [char]"r") {
                $rawStart = $charIndex
            } elseif ($charIndex + 1 -lt $line.Length -and $line[$charIndex] -eq [char]"b" -and $line[$charIndex + 1] -eq [char]"r") {
                $rawStart = $charIndex + 1
            }
            if ($rawStart -ge 0) {
                $probe = $rawStart + 1
                while ($probe -lt $line.Length -and $line[$probe] -eq [char]"#") {
                    $probe++
                }
                if ($probe -lt $line.Length -and $line[$probe] -eq [char]"""") {
                    $hashes = $line.Substring($rawStart + 1, $probe - $rawStart - 1)
                    $rawTerminator = """" + $hashes
                    $charIndex = $probe + 1
                    continue
                }
            }

            if (
                $line[$charIndex] -eq [char]"""" -or
                ($charIndex + 1 -lt $line.Length -and $line[$charIndex] -eq [char]"b" -and $line[$charIndex + 1] -eq [char]"""")
            ) {
                $inString = $true
                $escape = $false
                $charIndex += if ($line[$charIndex] -eq [char]"b") { 2 } else { 1 }
                continue
            }

            if ($line[$charIndex] -eq [char]"'") {
                $inChar = $true
                $escape = $false
                $charIndex++
                continue
            }

            $charIndex++
        }
    }

    Write-Output -NoEnumerate $macroLines
}

function Get-RustLineBraceDelta {
    param([Parameter(Mandatory = $true)][AllowEmptyString()][string]$Line)

    $withoutLineComment = $Line.Split([string[]]@("//"), 2, [System.StringSplitOptions]::None)[0]
    $delta = 0
    foreach ($char in $withoutLineComment.ToCharArray()) {
        if ($char -eq [char]"{") {
            $delta++
        } elseif ($char -eq [char]"}") {
            $delta--
        }
    }

    $delta
}

function Get-RustCandidateTestLineSet {
    param(
        [AllowNull()][string]$Content,
        [Parameter(Mandatory = $true)][AllowEmptyCollection()][int[]]$CandidateLines
    )

    $testCandidateLines = [System.Collections.Generic.HashSet[int]]::new()
    if ([string]::IsNullOrEmpty($Content) -or $CandidateLines.Count -eq 0) {
        Write-Output -NoEnumerate $testCandidateLines
        return
    }

    $candidateLineSet = [System.Collections.Generic.HashSet[int]]::new()
    $maxCandidateLine = 0
    foreach ($lineNumber in $CandidateLines) {
        [void]$candidateLineSet.Add($lineNumber)
        if ($lineNumber -gt $maxCandidateLine) {
            $maxCandidateLine = $lineNumber
        }
    }
    $lines = [string[]](($Content -replace "`r`n", "`n" -replace "`r", "`n") -split "`n")
    $lineLimit = [System.Math]::Min($maxCandidateLine, $lines.Count)
    $depth = 0
    $pendingTestAttribute = $false
    $pendingTestItemDepth = $null
    $testRegionDepths = [System.Collections.Generic.List[int]]::new()

    for ($index = 0; $index -lt $lineLimit; $index++) {
        $line = $lines[$index]
        $trimmed = $line.Trim()

        for ($regionIndex = $testRegionDepths.Count - 1; $regionIndex -ge 0; $regionIndex--) {
            if ($depth -lt $testRegionDepths[$regionIndex]) {
                $testRegionDepths.RemoveAt($regionIndex)
            }
        }

        $lineNumber = $index + 1
        if ($candidateLineSet.Contains($lineNumber)) {
            foreach ($regionDepth in $testRegionDepths) {
                if ($depth -ge $regionDepth) {
                    [void]$testCandidateLines.Add($lineNumber)
                    break
                }
            }
        }

        if ($trimmed.StartsWith("#[")) {
            if ((Test-RustCfgTestAttributeLine -Line $trimmed) -or (Test-RustDirectTestAttributeLine -Line $trimmed)) {
                $pendingTestAttribute = $true
            }
            continue
        }
        if ([string]::IsNullOrWhiteSpace($trimmed)) {
            continue
        }

        if ($pendingTestAttribute) {
            $pendingTestItemDepth = $depth
            $pendingTestAttribute = $false
        }

        $newDepth = $depth + (Get-RustLineBraceDelta -Line $line)
        if ($null -ne $pendingTestItemDepth) {
            if ($newDepth -gt $pendingTestItemDepth) {
                [void]$testRegionDepths.Add($pendingTestItemDepth + 1)
                $pendingTestItemDepth = $null
            } elseif ($trimmed.Contains(";")) {
                $pendingTestItemDepth = $null
            }
        }

        $depth = [System.Math]::Max(0, $newDepth)
        for ($regionIndex = $testRegionDepths.Count - 1; $regionIndex -ge 0; $regionIndex--) {
            if ($depth -lt $testRegionDepths[$regionIndex]) {
                $testRegionDepths.RemoveAt($regionIndex)
            }
        }
    }

    Write-Output -NoEnumerate $testCandidateLines
}

function Get-WorktreeRustPanicMacroCandidates {
    param(
        [Parameter(Mandatory = $true)][AllowEmptyCollection()][string[]]$Pathspecs
    )

    $candidates = [System.Collections.Generic.List[object]]::new()
    if ($Pathspecs.Count -eq 0) {
        return @()
    }

    $trackedFiles = [string[]]@(Get-IndexFiles -Pathspecs $Pathspecs)
    $tracked = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::Ordinal)
    foreach ($file in $trackedFiles) {
        [void]$tracked.Add($file)
    }

    if ($trackedFiles.Count -gt 0) {
        $grepPattern = "(^|[^[:alnum:]_])(panic|todo|unimplemented|unreachable)[[:space:]]*(/\*[^*]*\*+([^/*][^*]*\*+)*/[[:space:]]*)*!"
        $result = Invoke-Native -FileName "git" -Arguments (@("grep", "-n", "-E", $grepPattern, "--") + $trackedFiles)
        if ($result.ExitCode -ne 0 -and $result.ExitCode -ne 1) {
            throw "git grep failed:`n$($result.Output)"
        }
        if ($result.ExitCode -eq 0) {
            foreach ($rawLine in $result.Stdout.Split("`n", [System.StringSplitOptions]::RemoveEmptyEntries)) {
                $line = $rawLine.TrimEnd("`r")
                if ($line -notmatch "^(?<file>[^:]+):(?<line>\d+):(?<text>.*)$") {
                    throw "Unable to parse git grep output line: $line"
                }

                $text = $Matches["text"]
                if (Test-RustPanicMacroLine -Line $text) {
                    [void]$candidates.Add([pscustomobject]@{
                            File = $Matches["file"]
                            Line = [int]$Matches["line"]
                            Text = $text.TrimStart()
                        })
                }
            }
        }
    }

    foreach ($file in $Pathspecs) {
        if ($tracked.Contains($file)) {
            continue
        }

        $content = Get-PolicyText -Path $file
        if ($null -eq $content) {
            continue
        }

        $lineNumber = 0
        foreach ($line in $content -split "`r?`n") {
            $lineNumber++
            if (Test-RustPanicMacroLine -Line $line) {
                [void]$candidates.Add([pscustomobject]@{
                        File = $file
                        Line = $lineNumber
                        Text = $line.TrimStart()
                    })
            }
        }
    }

    return [object[]]$candidates.ToArray()
}

function Get-StagedRustPanicMacroCandidates {
    param([Parameter(Mandatory = $true)][AllowEmptyCollection()][string[]]$Pathspecs)

    if ($Pathspecs.Count -eq 0) {
        return @()
    }

    $grepPattern = "(^|[^[:alnum:]_])(panic|todo|unimplemented|unreachable)[[:space:]]*(/\*[^*]*\*+([^/*][^*]*\*+)*/[[:space:]]*)*!"
    $result = Invoke-Native -FileName "git" -Arguments (@("grep", "--cached", "-n", "-E", $grepPattern, "--") + $Pathspecs)
    if ($result.ExitCode -eq 1) {
        return @()
    }
    if ($result.ExitCode -ne 0) {
        throw "git grep --cached failed:`n$($result.Output)"
    }

    $candidates = [System.Collections.Generic.List[object]]::new()
    foreach ($rawLine in $result.Stdout.Split("`n", [System.StringSplitOptions]::RemoveEmptyEntries)) {
        $line = $rawLine.TrimEnd("`r")
        if ($line -notmatch "^(?<file>[^:]+):(?<line>\d+):(?<text>.*)$") {
            throw "Unable to parse git grep output line: $line"
        }

        $text = $Matches["text"]
        if (Test-RustPanicMacroLine -Line $text) {
            [void]$candidates.Add([pscustomobject]@{
                    File = $Matches["file"]
                    Line = [int]$Matches["line"]
                    Text = $text.TrimStart()
                })
        }
    }

    return [object[]]$candidates.ToArray()
}

function Get-HeadRustPanicMacroCandidates {
    param([Parameter(Mandatory = $true)][AllowEmptyCollection()][string[]]$Pathspecs)

    if ($Pathspecs.Count -eq 0) {
        return @()
    }

    $grepPattern = "(^|[^[:alnum:]_])(panic|todo|unimplemented|unreachable)[[:space:]]*(/\*[^*]*\*+([^/*][^*]*\*+)*/[[:space:]]*)*!"
    $result = Invoke-Native -FileName "git" -Arguments (@("grep", "-n", "-E", $grepPattern, "HEAD", "--") + $Pathspecs)
    if ($result.ExitCode -eq 1) {
        return @()
    }
    if ($result.ExitCode -ne 0) {
        if ($result.Output -match "unknown revision|bad revision|ambiguous argument 'HEAD'|Not a valid object name HEAD") {
            return @()
        }
        throw "git grep HEAD failed:`n$($result.Output)"
    }

    $candidates = [System.Collections.Generic.List[object]]::new()
    foreach ($rawLine in $result.Stdout.Split("`n", [System.StringSplitOptions]::RemoveEmptyEntries)) {
        $line = $rawLine.TrimEnd("`r")
        if ($line -notmatch "^HEAD:(?<file>[^:]+):(?<line>\d+):(?<text>.*)$") {
            throw "Unable to parse git grep HEAD output line: $line"
        }

        $text = $Matches["text"]
        if (Test-RustPanicMacroLine -Line $text) {
            [void]$candidates.Add([pscustomobject]@{
                    File = $Matches["file"]
                    Line = [int]$Matches["line"]
                    Text = $text.TrimStart()
                })
        }
    }

    return [object[]]$candidates.ToArray()
}

function Get-HeadText {
    param([Parameter(Mandatory = $true)][string]$Path)

    $result = Invoke-Native -FileName "git" -Arguments @("show", "HEAD:$Path")
    if ($result.ExitCode -ne 0) {
        return $null
    }

    $result.Stdout
}

function Get-RustTestContextMarkerSignature {
    param([AllowNull()][string]$Content)

    if ([string]::IsNullOrEmpty($Content)) {
        return ""
    }

    $markers = [System.Collections.Generic.List[string]]::new()
    $lineNumber = 0
    foreach ($line in $Content -split "`r?`n") {
        $lineNumber++
        $trimmed = $line.Trim()
        if ((Test-RustCfgTestAttributeLine -Line $trimmed) -or (Test-RustDirectTestAttributeLine -Line $trimmed)) {
            [void]$markers.Add("${lineNumber}:$trimmed")
        }
    }

    $markers -join "`n"
}

function Get-RustTestContextMarkerSignatureFromGit {
    param(
        [Parameter(Mandatory = $true)][AllowEmptyCollection()][string[]]$CandidateFiles,
        [switch]$Head
    )

    if ($CandidateFiles.Count -eq 0) {
        return ""
    }

    $grepPattern = "#\[[[:space:]]*(cfg[[:space:]]*\(|test|tokio::test|async_std::test|rstest)"
    $arguments = @("grep")
    if ($Head) {
        $arguments += @("-n", "-E", $grepPattern, "HEAD")
    } elseif (-not $script:InspectWorktree) {
        $arguments += @("--cached", "-n", "-E", $grepPattern)
    } else {
        $arguments += @("-n", "-E", $grepPattern)
    }
    $arguments += @("--") + $CandidateFiles

    $result = Invoke-Native -FileName "git" -Arguments $arguments
    if ($result.ExitCode -eq 1) {
        return ""
    }
    if ($result.ExitCode -ne 0) {
        if ($Head -and $result.Output -match "unknown revision|bad revision|ambiguous argument 'HEAD'|Not a valid object name HEAD") {
            return ""
        }
        throw "git grep test context markers failed:`n$($result.Output)"
    }

    $markers = [System.Collections.Generic.List[string]]::new()
    foreach ($rawLine in $result.Stdout.Split("`n", [System.StringSplitOptions]::RemoveEmptyEntries)) {
        $line = $rawLine.TrimEnd("`r")
        if ($Head -and $line.StartsWith("HEAD:", [System.StringComparison]::Ordinal)) {
            $line = $line.Substring("HEAD:".Length)
        }
        [void]$markers.Add($line)
    }

    $markerArray = [string[]]$markers
    [array]::Sort($markerArray, [System.StringComparer]::Ordinal)
    $markerArray -join "`n"
}

function Test-RustTestContextMarkersChanged {
    param([Parameter(Mandatory = $true)][AllowEmptyCollection()][string[]]$CandidateFiles)

    (Get-RustTestContextMarkerSignatureFromGit -CandidateFiles $CandidateFiles) -ne
    (Get-RustTestContextMarkerSignatureFromGit -CandidateFiles $CandidateFiles -Head)
}

function Test-RustPanicMacroDiffChanged {
    param([Parameter(Mandatory = $true)][AllowEmptyCollection()][string[]]$CandidateFiles)

    if ($CandidateFiles.Count -eq 0) {
        return $false
    }

    if ($script:InspectWorktree) {
        $tracked = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::Ordinal)
        foreach ($file in (Get-IndexFiles -Pathspecs $CandidateFiles)) {
            [void]$tracked.Add($file)
        }
        foreach ($file in $CandidateFiles) {
            if (-not $tracked.Contains($file)) {
                return $true
            }
        }
    }

    $grepPattern = "(^|[^[:alnum:]_])(panic|todo|unimplemented|unreachable)[[:space:]]*(/\*[^*]*\*+([^/*][^*]*\*+)*/[[:space:]]*)*!"
    $arguments = @("diff")
    if ($script:InspectWorktree) {
        $arguments += "HEAD"
    } else {
        $arguments += "--cached"
    }
    $arguments += @("--quiet", "-G", $grepPattern, "--") + $CandidateFiles
    $result = Invoke-Native -FileName "git" -Arguments $arguments
    if ($result.ExitCode -eq 0) {
        return $false
    }
    if ($result.ExitCode -eq 1) {
        return $true
    }
    if ($script:InspectWorktree -and $result.Output -match "unknown revision|bad revision|ambiguous argument 'HEAD'|Not a valid object name HEAD") {
        return $true
    }

    throw "git diff panic macro check failed:`n$($result.Output)"
}

function Select-ProductionRustPanicMacroCandidates {
    param(
        [Parameter(Mandatory = $true)][AllowEmptyCollection()][object[]]$Candidates,
        [switch]$Head
    )

    $productionCandidates = [System.Collections.Generic.List[object]]::new()
    if ($Candidates.Count -eq 0) {
        return @()
    }

    $candidateFiles = [string[]]@($Candidates | ForEach-Object { $_.File } | Sort-Object -Unique)
    foreach ($file in $candidateFiles) {
        $content = if ($Head) { Get-HeadText -Path $file } else { Get-PolicyText -Path $file }
        if ($null -eq $content) {
            continue
        }

        $fileCandidates = @($Candidates | Where-Object { $_.File -eq $file } | Sort-Object -Property Line)
        if ($fileCandidates.Count -eq 0) {
            continue
        }

        $lineNumbers = [int[]]@($fileCandidates | ForEach-Object { [int]$_.Line })
        $executableMacroLines = Get-RustExecutablePanicMacroLineSet -Content $content -CandidateLines $lineNumbers
        $testCandidateLines = Get-RustCandidateTestLineSet -Content $content -CandidateLines $lineNumbers
        foreach ($candidate in $fileCandidates) {
            if (-not $executableMacroLines.Contains($candidate.Line)) {
                continue
            }
            if ($testCandidateLines.Contains($candidate.Line)) {
                continue
            }

            [void]$productionCandidates.Add($candidate)
        }
    }

    return [object[]]$productionCandidates.ToArray()
}

function Get-RustPanicCandidateKey {
    param(
        [Parameter(Mandatory = $true)]$Candidate,
        [switch]$IncludeLine
    )

    if ($IncludeLine) {
        return "$($Candidate.File)`0$($Candidate.Line)`0$($Candidate.Text)"
    }

    "$($Candidate.File)`0$($Candidate.Text)"
}

function Get-NewRustPanicMacroCandidates {
    param(
        [Parameter(Mandatory = $true)][AllowEmptyCollection()][object[]]$Candidates,
        [Parameter(Mandatory = $true)][AllowEmptyCollection()][string[]]$CandidateFiles,
        [Nullable[bool]]$TestContextMarkersChanged = $null
    )

    if ($Candidates.Count -eq 0) {
        return @()
    }

    $contextMarkersChanged = if ($null -eq $TestContextMarkersChanged) {
        Test-RustTestContextMarkersChanged -CandidateFiles $CandidateFiles
    } else {
        [bool]$TestContextMarkersChanged
    }

    if ($contextMarkersChanged) {
        $productionCandidates = @(Select-ProductionRustPanicMacroCandidates -Candidates $Candidates)
        if ($productionCandidates.Count -eq 0) {
            return @()
        }

        $headCandidates = @(Get-HeadRustPanicMacroCandidates -Pathspecs $CandidateFiles)
        $productionHeadCandidates = @(Select-ProductionRustPanicMacroCandidates -Candidates $headCandidates -Head)
        $headProductionCounts = [System.Collections.Generic.Dictionary[string, int]]::new([System.StringComparer]::Ordinal)
        foreach ($candidate in $productionHeadCandidates) {
            $key = Get-RustPanicCandidateKey -Candidate $candidate
            if ($headProductionCounts.ContainsKey($key)) {
                $headProductionCounts[$key]++
            } else {
                $headProductionCounts[$key] = 1
            }
        }

        $newProductionCandidates = [System.Collections.Generic.List[object]]::new()
        foreach ($candidate in @($productionCandidates | Sort-Object -Property File, Line)) {
            $key = Get-RustPanicCandidateKey -Candidate $candidate
            if ($headProductionCounts.ContainsKey($key) -and $headProductionCounts[$key] -gt 0) {
                $headProductionCounts[$key]--
                continue
            }

            [void]$newProductionCandidates.Add($candidate)
        }

        return [object[]]$newProductionCandidates.ToArray()
    }

    $headCandidates = Get-HeadRustPanicMacroCandidates -Pathspecs $CandidateFiles
    $headCounts = [System.Collections.Generic.Dictionary[string, int]]::new([System.StringComparer]::Ordinal)
    foreach ($candidate in $headCandidates) {
        $key = Get-RustPanicCandidateKey -Candidate $candidate -IncludeLine
        if ($headCounts.ContainsKey($key)) {
            $headCounts[$key]++
        } else {
            $headCounts[$key] = 1
        }
    }

    $newCandidates = [System.Collections.Generic.List[object]]::new()
    foreach ($candidate in @($Candidates | Sort-Object -Property File, Line)) {
        $key = Get-RustPanicCandidateKey -Candidate $candidate -IncludeLine
        if ($headCounts.ContainsKey($key) -and $headCounts[$key] -gt 0) {
            $headCounts[$key]--
            continue
        }

        [void]$newCandidates.Add($candidate)
    }

    return [object[]]$newCandidates.ToArray()
}

function Test-RustAddedPanicPatterns {
    $changedProductionRustFiles = [string[]]@($script:StagedFiles | Where-Object { Test-ProductionRustSourcePath -Path $_ })
    if ($changedProductionRustFiles.Count -eq 0) {
        Skip "Rust panic patterns" "no production Rust files staged"
        return
    }

    $violations = [System.Collections.Generic.List[string]]::new()
    $candidates = @()
    $candidatesPreFilteredToProduction = $false

    if ($script:InspectWorktree) {
        $scannableFiles = [string[]]@($changedProductionRustFiles | Where-Object {
                Test-Path -LiteralPath (Join-Path $script:RepoRoot $_)
            })
        $candidates = @(Get-WorktreeRustPanicMacroCandidates -Pathspecs $scannableFiles)
    } else {
        try {
            $candidates = @(Get-StagedRustPanicMacroCandidates -Pathspecs $changedProductionRustFiles)
        } catch {
            Fail "Rust panic patterns" $_.Exception.Message
            return
        }
    }

    if ($candidates.Count -eq 0) {
        Pass "Rust panic patterns"
        return
    }

    $candidateFiles = [string[]]@($candidates | ForEach-Object { $_.File } | Sort-Object -Unique)
    try {
        $testContextMarkersChanged = Test-RustTestContextMarkersChanged -CandidateFiles $candidateFiles
        if (-not $testContextMarkersChanged) {
            $panicMacroDiffChanged = Test-RustPanicMacroDiffChanged -CandidateFiles $candidateFiles
            if (-not $panicMacroDiffChanged) {
                Pass "Rust panic patterns"
                return
            }
        }

        $candidates = @(Get-NewRustPanicMacroCandidates -Candidates $candidates -CandidateFiles $candidateFiles -TestContextMarkersChanged:$testContextMarkersChanged)
        $candidatesPreFilteredToProduction = $testContextMarkersChanged
    } catch {
        Fail "Rust panic patterns" $_.Exception.Message
        return
    }
    if ($candidates.Count -eq 0) {
        Pass "Rust panic patterns"
        return
    }
    $candidateFiles = [string[]]@($candidates | ForEach-Object { $_.File } | Sort-Object -Unique)

    if (-not $script:InspectWorktree -and $candidateFiles.Count -gt 2) {
        try {
            Add-IndexTextCache -Pathspecs $candidateFiles
        } catch {
            Fail "Rust panic patterns" $_.Exception.Message
            return
        }
    }

    foreach ($file in @($candidateFiles | Sort-Object)) {
        $fileCandidates = @($candidates | Where-Object { $_.File -eq $file } | Sort-Object -Property Line)
        if ($fileCandidates.Count -eq 0) {
            continue
        }

        if ($candidatesPreFilteredToProduction) {
            foreach ($candidate in $fileCandidates) {
                [void]$violations.Add("$($candidate.File):$($candidate.Line): $($candidate.Text)")
                if ($violations.Count -ge 5) {
                    break
                }
            }
            if ($violations.Count -ge 5) {
                break
            }
            continue
        }

        $content = Get-PolicyText -Path $file
        if ($null -eq $content) {
            continue
        }

        $lineNumbers = [int[]]@($fileCandidates | ForEach-Object { [int]$_.Line })
        $executableMacroLines = Get-RustExecutablePanicMacroLineSet -Content $content -CandidateLines $lineNumbers
        $testCandidateLines = Get-RustCandidateTestLineSet -Content $content -CandidateLines $lineNumbers
        foreach ($candidate in $fileCandidates) {
            if (-not $executableMacroLines.Contains($candidate.Line)) {
                continue
            }
            if ($testCandidateLines.Contains($candidate.Line)) {
                continue
            }

            [void]$violations.Add("$($candidate.File):$($candidate.Line): $($candidate.Text)")
            if ($violations.Count -ge 5) {
                break
            }
        }
        if ($violations.Count -ge 5) {
            break
        }
    }

    if ($violations.Count -gt 0) {
        Fail "Rust panic patterns" "Changed production Rust files contain explicit panic macros. Replace panic!/todo!/unimplemented!/unreachable! with typed errors or move test-only macros behind #[cfg(test)]. Full .expect/.unwrap policy runs in local CI and CI.`n$($violations -join "`n")"
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
        $script:IndexTextCache[$path] = $content
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

function ConvertTo-GitRelativePath {
    param([Parameter(Mandatory = $true)][string]$Path)

    $relative = [System.IO.Path]::GetRelativePath($script:RepoRoot, $Path)
    $relative.Replace([System.IO.Path]::DirectorySeparatorChar, "/").Replace([System.IO.Path]::AltDirectorySeparatorChar, "/")
}

function New-WorktreeSkillsIndexContent {
    $warning = "> WARNING: Auto-generated by ``scripts/generate-skills-index.sh``. Do not edit manually."
    $lines = [System.Collections.Generic.List[string]]::new()
    [void]$lines.Add("# Skills Index")
    [void]$lines.Add("")
    [void]$lines.Add($warning)
    [void]$lines.Add("")
    [void]$lines.Add("## Files")
    [void]$lines.Add("")

    $skillsRoot = Join-Path $script:RepoRoot ".llm/skills"
    if (-not (Test-Path -LiteralPath $skillsRoot)) {
        return ($lines -join "`n") + "`n"
    }

    $paths = [string[]]@(
        [System.IO.Directory]::EnumerateFiles($skillsRoot, "*.md", [System.IO.SearchOption]::TopDirectoryOnly) |
            ForEach-Object {
                $fileName = [System.IO.Path]::GetFileName($_)
                if ($fileName -ne "index.md") {
                    ".llm/skills/$fileName"
                }
            }
    )
    [array]::Sort($paths, [System.StringComparer]::Ordinal)

    foreach ($path in $paths) {
        $fileName = Get-GitPathFileName -Path $path
        $title = ""
        $absolutePath = Join-Path $script:RepoRoot $path
        try {
            foreach ($line in [System.IO.File]::ReadLines($absolutePath)) {
                if ($line.StartsWith("# Skill:")) {
                    $title = $line.Substring("# Skill:".Length).Trim()
                    break
                }
            }
        } catch {
            continue
        }
        if ([string]::IsNullOrWhiteSpace($title)) {
            [void]$lines.Add("- [$fileName](./$fileName)")
        } else {
            [void]$lines.Add("- [$title](./$fileName)")
        }
    }

    ($lines -join "`n") + "`n"
}

function Test-WorktreeSkillsIndexDefinitelyFresh {
    param([Parameter(Mandatory = $true)][AllowEmptyCollection()][string[]]$ChangedSkillFiles)

    if ($ChangedSkillFiles.Count -eq 0) {
        return $false
    }

    if ($ChangedSkillFiles -contains ".llm/skills/index.md") {
        return $false
    }

    $trackedFiles = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::Ordinal)
    foreach ($file in (Get-IndexFiles -Pathspecs $ChangedSkillFiles)) {
        [void]$trackedFiles.Add($file)
    }
    foreach ($file in $ChangedSkillFiles) {
        if (-not $trackedFiles.Contains($file)) {
            return $false
        }
    }

    $fileListChanges = Invoke-Native -FileName "git" -Arguments (@("diff", "--name-only", "--diff-filter=ACDRTUXB", "HEAD", "--") + $ChangedSkillFiles)
    if ($fileListChanges.ExitCode -ne 0) {
        if ($fileListChanges.Output -match "unknown revision|bad revision|ambiguous argument 'HEAD'|Not a valid object name HEAD") {
            return $false
        }
        throw "git diff skill file-list check failed:`n$($fileListChanges.Output)"
    }
    if (-not [string]::IsNullOrWhiteSpace($fileListChanges.Stdout)) {
        return $false
    }

    $headTitles = [System.Collections.Generic.Dictionary[string, string]]::new([System.StringComparer]::Ordinal)
    $headTitleResult = Invoke-Native -FileName "git" -Arguments (@("grep", "-n", "-E", "^# Skill:", "HEAD", "--") + $ChangedSkillFiles)
    if ($headTitleResult.ExitCode -eq 1) {
        return $false
    }
    if ($headTitleResult.ExitCode -ne 0) {
        if ($headTitleResult.Output -match "unknown revision|bad revision|ambiguous argument 'HEAD'|Not a valid object name HEAD") {
            return $false
        }
        throw "git grep skill title check failed:`n$($headTitleResult.Output)"
    }

    foreach ($rawLine in $headTitleResult.Stdout.Split("`n", [System.StringSplitOptions]::RemoveEmptyEntries)) {
        $line = $rawLine.TrimEnd("`r")
        $match = [regex]::Match($line, "^HEAD:(?<file>.+?):\d+:# Skill:\s*(?<title>.*)$")
        if ($match.Success) {
            $headTitles[$match.Groups["file"].Value] = $match.Groups["title"].Value.Trim()
        }
    }

    foreach ($file in $ChangedSkillFiles) {
        if (-not $headTitles.ContainsKey($file)) {
            return $false
        }

        $worktreeTitle = ""
        $absolutePath = Join-Path $script:RepoRoot $file
        foreach ($line in [System.IO.File]::ReadLines($absolutePath)) {
            if ($line.StartsWith("# Skill:")) {
                $worktreeTitle = $line.Substring("# Skill:".Length).Trim()
                break
            }
        }
        if (-not [System.StringComparer]::Ordinal.Equals($headTitles[$file], $worktreeTitle)) {
            return $false
        }
    }

    $true
}

function Repair-SkillsIndexIfNeeded {
    $changedSkillFiles = [string[]]@($script:StagedFiles | Where-Object {
            $_.StartsWith(".llm/skills/") -and $_.EndsWith(".md")
        })
    $triggered = @($script:StagedFiles | Where-Object {
            $_ -eq "scripts/generate-skills-index.sh" -or
            ($_.StartsWith(".llm/skills/") -and $_.EndsWith(".md"))
        }).Count -gt 0
    if (-not $triggered) {
        Skip "Skills index freshness" "no skills index inputs staged"
        return
    }

    if ($script:InspectWorktree) {
        if (
            ($script:StagedFiles -notcontains "scripts/generate-skills-index.sh") -and
            (Test-WorktreeSkillsIndexDefinitelyFresh -ChangedSkillFiles $changedSkillFiles)
        ) {
            Pass "Skills index freshness"
            return
        }

        try {
            $expected = New-WorktreeSkillsIndexContent
        } catch {
            Fail "Skills index freshness" $_.Exception.Message
            return
        }
        $actual = Get-WorktreeText -Path ".llm/skills/index.md"
        if ($null -eq $actual) { $actual = "" }

        if ($actual -eq $expected) {
            Pass "Skills index freshness"
            return
        }

        $indexPath = Join-Path $script:RepoRoot ".llm/skills/index.md"
        [System.IO.File]::WriteAllText($indexPath, $expected, [System.Text.UTF8Encoding]::new($false))
        $script:WorktreeTextCache[".llm/skills/index.md"] = $expected
        Pass "Skills index freshness (auto-repaired)"
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

# ---------------------------------------------------------------------------
# Doc version sync (auto-repair)
#
# Cargo.toml [package].version is the single source of truth for the crate
# version. A version bump must propagate to the human-facing docs that quote it,
# or scripts/check-doc-consistency.sh (run in CI and run-local-ci) fails. This
# check regenerates those quoted versions from Cargo.toml and re-stages them, so
# a bump can never land a stale doc and break CI.
#
# SITES (must mirror the version-sync section of
# scripts/check-doc-consistency.sh; the parity is locked by
# tests/doc_consistency_policy_tests.rs):
#   - docs/library-usage.md : signal-fish-server = "X"
#                             signal-fish-server = { version = "X", ... }
#   - .llm/context.md       : - **Version:** X
# Dependency lines containing a <version> placeholder are intentionally left
# untouched, matching the checker.
$script:DocVersionSyncFiles = @("docs/library-usage.md", ".llm/context.md")

function Read-CargoPackageVersion {
    param([Parameter(Mandatory = $true)][AllowEmptyString()][AllowNull()][string]$CargoTomlText)

    if ([string]::IsNullOrEmpty($CargoTomlText)) {
        return $null
    }

    # Mirror read_cargo_package_version() in scripts/check-doc-consistency.sh:
    # the first `version = "..."` inside the [package] table, and only there.
    $inPackage = $false
    foreach ($rawLine in ($CargoTomlText -split "`n")) {
        $line = $rawLine.TrimEnd("`r")
        if ($line -match '^\s*\[package\]\s*$') {
            $inPackage = $true
            continue
        }
        if ($line -match '^\s*\[[^\]]+\]\s*$') {
            $inPackage = $false
            continue
        }
        if ($inPackage -and $line -match '^\s*version\s*=\s*"([^"]+)"') {
            return $Matches[1]
        }
    }
    $null
}

function Get-DocVersionSyncedContent {
    param(
        [Parameter(Mandatory = $true)][string]$Path,
        [Parameter(Mandatory = $true)][AllowEmptyString()][string]$Content,
        [Parameter(Mandatory = $true)][string]$Version
    )

    switch ($Path) {
        "docs/library-usage.md" {
            # Rewrite the version inside any signal-fish-server dependency
            # assignment, both the plain (= "X") and table
            # (= { version = "X", ... }) forms. The leading group captures
            # everything up to and including the opening quote; [^"<]+ matches
            # the version without spanning the closing quote and without
            # matching <version> placeholders. Line-ending bytes stay outside
            # the match, so LF/CRLF is preserved verbatim.
            return [regex]::Replace(
                $Content,
                '(signal-fish-server[ \t]*=[ \t]*(?:\{[^}\r\n]*?\bversion[ \t]*=[ \t]*)?")[^"<]+(")',
                { param($m) "$($m.Groups[1].Value)$Version$($m.Groups[2].Value)" })
        }
        ".llm/context.md" {
            # Rewrite the exact metadata line. [^\r\n]* stops before the line
            # ending so LF/CRLF is preserved verbatim.
            return [regex]::Replace(
                $Content,
                '(?m)^(- \*\*Version:\*\* )[^\r\n]*',
                { param($m) "$($m.Groups[1].Value)$Version" })
        }
        default {
            return $Content
        }
    }
}

# Report the residual reason the CI checker (scripts/check-doc-consistency.sh)
# would STILL reject $Content after an auto-sync attempt, or $null if it is
# compliant. This keeps the hook honest: it must never report success on a
# state CI rejects (e.g. a path/git dependency with no version key to rewrite,
# or a context.md whose required version line was deleted entirely and cannot
# be re-synthesized). Mirrors the checker's per-file rules.
function Get-DocVersionResidualProblem {
    param(
        [Parameter(Mandatory = $true)][string]$Path,
        [Parameter(Mandatory = $true)][AllowEmptyString()][string]$Content,
        [Parameter(Mandatory = $true)][string]$Version
    )

    switch ($Path) {
        "docs/library-usage.md" {
            # Mirror validate_signal_fish_dependency_versions() in
            # scripts/check-doc-consistency.sh EXACTLY: parse each
            # signal-fish-server dependency line's version (table form first,
            # then plain form) and require it to equal the crate version.
            # A <version> placeholder is exempt; a line with no extractable
            # version (path/git dep) is unfixable. We extract-and-compare rather
            # than substring-test so a version-less line that merely mentions
            # the crate version elsewhere (e.g. a git tag) is still flagged.
            $unfixable = [System.Collections.Generic.List[string]]::new()
            foreach ($match in [regex]::Matches($Content, '(?m)^.*signal-fish-server[ \t]*=.*$')) {
                $line = $match.Value
                if ($line -match '<version>') {
                    continue
                }
                $observed = $null
                $tableForm = [regex]::Match($line, 'signal-fish-server[ \t]*=[ \t]*\{[^}]*version[ \t]*=[ \t]*"([^"]+)"')
                if ($tableForm.Success) {
                    $observed = $tableForm.Groups[1].Value
                } else {
                    $plainForm = [regex]::Match($line, 'signal-fish-server[ \t]*=[ \t]*"([^"]+)"')
                    if ($plainForm.Success) {
                        $observed = $plainForm.Groups[1].Value
                    }
                }
                if ($null -eq $observed -or $observed -ne $Version) {
                    [void]$unfixable.Add($line)
                }
            }
            if ($unfixable.Count -gt 0) {
                return "docs/library-usage.md has a signal-fish-server dependency line that cannot be auto-synced to $Version (e.g. a path/git dependency without a version key). Fix it by hand:`n  $($unfixable -join "`n  ")"
            }
            return $null
        }
        ".llm/context.md" {
            # The checker requires the exact line; if it was deleted entirely the
            # regex has nothing to rewrite and we must not silently pass.
            $expectedLine = "- **Version:** $Version"
            foreach ($line in ($Content -split "`n")) {
                if ($line.TrimEnd("`r") -eq $expectedLine) {
                    return $null
                }
            }
            return ".llm/context.md is missing the required line '$expectedLine' and cannot be auto-created. Add it under the project identity section."
        }
        default {
            return $null
        }
    }
}

function Repair-DocVersionsIfNeeded {
    param([Parameter(Mandatory = $true)][AllowEmptyCollection()][string[]]$ChangedFiles)

    # Only meaningful when the crate version or a tracked version-doc changed.
    # Mirrors the trigger gate of Repair-SkillsIndexIfNeeded so the common
    # commit pays no extra git calls.
    $triggerFiles = @("Cargo.toml") + $script:DocVersionSyncFiles
    if (@($ChangedFiles | Where-Object { $triggerFiles -contains $_ }).Count -eq 0) {
        Skip "Doc version sync" "no Cargo.toml or version-doc changes"
        return
    }

    $cargoText = Get-PolicyText -Path "Cargo.toml"
    $version = Read-CargoPackageVersion -CargoTomlText $cargoText
    if ([string]::IsNullOrEmpty($version)) {
        Skip "Doc version sync" "Cargo.toml [package].version not readable"
        return
    }

    $repaired = [System.Collections.Generic.List[string]]::new()
    foreach ($path in $script:DocVersionSyncFiles) {
        $actual = Get-PolicyText -Path $path
        if ($null -eq $actual) {
            # File not tracked in this commit / not present: nothing to sync.
            continue
        }

        $expected = Get-DocVersionSyncedContent -Path $path -Content $actual -Version $version

        $problem = Get-DocVersionResidualProblem -Path $path -Content $expected -Version $version
        if ($null -ne $problem) {
            Fail "Doc version sync" $problem
            return
        }

        if ($expected -eq $actual) {
            continue
        }

        if ($script:InspectWorktree) {
            $absolutePath = Join-Path $script:RepoRoot $path
            [System.IO.File]::WriteAllText($absolutePath, $expected, [System.Text.UTF8Encoding]::new($false))
            $script:WorktreeTextCache[$path] = $expected
            [void]$repaired.Add($path)
            continue
        }

        $expectedHash = Set-IndexText -Path $path -Content $expected
        $updatedHash = Get-IndexObjectId -Path $path
        if ($updatedHash -ne $expectedHash) {
            Fail "Doc version sync" "Unable to re-stage $path with crate version $version."
            return
        }
        [void]$repaired.Add($path)
    }

    if ($repaired.Count -gt 0) {
        Pass "Doc version sync (auto-repaired: $($repaired -join ', '))"
    } else {
        Pass "Doc version sync"
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
        $content = Get-PolicyText -Path $file
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

    $readme = Get-PolicyText -Path "README.md"
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

if ($SourceOnly) {
    return
}

function Complete-PreCommit {
    $script:PreCommitTimer.Stop()
    Write-Host "[pre-commit] Completed in $($script:PreCommitTimer.ElapsedMilliseconds)ms"

    if ($script:PreCommitTimer.ElapsedMilliseconds -gt $script:HookBudgetMs) {
        $slowestChecks = @($script:CheckTimings |
                Sort-Object -Property ElapsedMilliseconds -Descending |
                Select-Object -First 3)
        if ($slowestChecks.Count -gt 0) {
            $summary = @($slowestChecks | ForEach-Object { "$($_.Name)=$($_.ElapsedMilliseconds)ms" }) -join ", "
            Write-Host "[pre-commit] WARN: runtime $($script:PreCommitTimer.ElapsedMilliseconds)ms exceeded ${script:HookBudgetMs}ms target. Slowest checks: $summary"
        } else {
            Write-Host "[pre-commit] WARN: runtime $($script:PreCommitTimer.ElapsedMilliseconds)ms exceeded ${script:HookBudgetMs}ms target."
        }
        if ($EnforceBudget) {
            Fail "Hook runtime budget" "Runtime $($script:PreCommitTimer.ElapsedMilliseconds)ms exceeded ${script:HookBudgetMs}ms target."
        }
    }

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
}

$script:RepoRoot = (Invoke-Git -Arguments @("rev-parse", "--show-toplevel")).Stdout.Trim()
Set-Location $script:RepoRoot
$script:InspectWorktree = [bool]$Worktree
$worktreePolicyPathspecs = @(
    "src",
    ".llm",
    "README.md",
    "scripts/generate-skills-index.sh",
    "Cargo.toml"
) + $script:DocVersionSyncFiles + $script:HookPolicyFiles
$allChangedFiles = [string[]]@(if ($script:InspectWorktree) {
        Get-WorktreeChangedFiles -Pathspecs $worktreePolicyPathspecs
    } else {
        Get-StagedFiles
    })
$script:HookPolicyChangedFiles = [string[]]@($allChangedFiles | Where-Object { $script:HookPolicyFiles -contains $_ })
$script:StagedFiles = [string[]]@($allChangedFiles | Where-Object { Test-ProductionRustSourcePath -Path $_ })

if ($script:InspectWorktree) {
    Write-Step "Running fast worktree preflight checks..."
} else {
    Write-Step "Running fast last-resort checks..."
}

# Keep the crate version and the docs that quote it in lockstep for every
# commit shape (runs before the Rust early-exit below so a Cargo.toml bump that
# also touches Rust still auto-syncs).
if (-not (Invoke-Check "Doc version sync" { Repair-DocVersionsIfNeeded -ChangedFiles $allChangedFiles })) { Complete-PreCommit }

$hasProductionRust = @($script:StagedFiles | Where-Object { Test-ProductionRustSourcePath -Path $_ }).Count -gt 0
if ($hasProductionRust) {
    if ($script:HookPolicyChangedFiles.Count -gt 0) {
        if (-not (Invoke-Check "Hook speed policy" { Test-FastHookSource })) { Complete-PreCommit }
    }
    if (-not (Invoke-Check "Rust panic patterns" { Test-RustAddedPanicPatterns })) { Complete-PreCommit }
    Complete-PreCommit
}

$script:StagedFiles = $allChangedFiles
if (-not (Invoke-Check "Hook speed policy" { Test-FastHookSource })) { Complete-PreCommit }
if (-not (Invoke-Check "Rust panic patterns" { Test-RustAddedPanicPatterns })) { Complete-PreCommit }

Add-StagedContentPreload
if ($null -ne $script:PreloadError) {
    Fail "Staged content preload" $script:PreloadError
    Complete-PreCommit
}

if (-not (Invoke-Check "Skills index freshness" { Repair-SkillsIndexIfNeeded })) { Complete-PreCommit }
if (-not (Invoke-Check "LLM file sizes" { Test-LlmFileSizes })) { Complete-PreCommit }
if (-not (Invoke-Check "README badge styles" { Test-ReadmeBadges })) { Complete-PreCommit }

Complete-PreCommit
