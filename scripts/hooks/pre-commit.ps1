#requires -Version 7.0
param([switch]$SourceOnly)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$script:Passed = 0
$script:Failed = 0
$script:Skipped = 0
$script:IndexTextCache = [System.Collections.Generic.Dictionary[string, string]]::new([System.StringComparer]::Ordinal)
$script:MaxBatchedBlobBytes = 2 * 1024 * 1024
$script:PreloadBatchThreshold = 3
$script:PreloadError = $null

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

function Get-StagedAddedLines {
    param(
        [string[]]$Pathspecs,
        [string]$PickaxePattern = ""
    )

    $arguments = @("diff", "--cached", "--unified=0", "--no-color", "--no-ext-diff", "--no-textconv", "--diff-filter=ACMR")
    if (-not [string]::IsNullOrEmpty($PickaxePattern)) {
        $arguments += @("-G", $PickaxePattern)
    }
    $arguments += @("--") + $Pathspecs
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
        "scripts/hooks/native-process.ps1",
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
    $result = Invoke-Native -FileName "git" -Arguments @("diff", "--cached", "--check", "--no-ext-diff", "--no-textconv")
    if ($result.ExitCode -eq 0) {
        Pass "Staged diff whitespace"
    } else {
        Fail "Staged diff whitespace" "Fix whitespace errors in the staged diff.`n$($result.Output.Trim())"
    }
}

function Add-StagedContentPreload {
    $skillsIndexTriggered = @($script:StagedFiles | Where-Object {
            $_ -eq "scripts/generate-skills-index.sh" -or
            ($_.StartsWith(".llm/skills/") -and $_.EndsWith(".md"))
        }).Count -gt 0
    $hookSourceFiles = @(
        ".githooks/pre-commit",
        ".githooks/pre-push",
        "scripts/hooks/native-process.ps1",
        "scripts/hooks/pre-commit.ps1",
        "scripts/hooks/pre-push.ps1"
    )
    $preloadPaths = @($script:StagedFiles | Where-Object {
            ($hookSourceFiles -contains $_) -or
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

    Write-Output -NoEnumerate $arguments
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

function Test-RustAddedPanicPatterns {
    $productionRustFiles = [string[]]@($script:StagedFiles | Where-Object { Test-ProductionRustSourcePath -Path $_ })
    if ($productionRustFiles.Count -eq 0) {
        Skip "Rust panic patterns" "no production Rust files staged"
        return
    }

    $rustWhitespace = "(?:\s|/\*.*?\*/)*"
    $panicPattern = "\.(?:unwrap|expect)\b|\b(?:panic|todo|unimplemented|unreachable)$rustWhitespace!$rustWhitespace\("
    $pickaxePattern = "unwrap|expect|panic|todo|unimplemented|unreachable"
    try {
        $diff = Get-StagedAddedLines -Pathspecs $productionRustFiles -PickaxePattern $pickaxePattern
    } catch {
        Fail "Rust panic patterns" $_.Exception.Message
        return
    }
    $currentFile = ""
    $nextNewLine = 0
    $candidates = [System.Collections.Generic.List[object]]::new()
    $violations = [System.Collections.Generic.List[string]]::new()

    foreach ($rawLine in $diff -split "`n") {
        $line = $rawLine.TrimEnd("`r")
        if ($line.StartsWith("+++ b/")) {
            $currentFile = $line.Substring(6)
            continue
        }
        if ($line -match "^@@ -\d+(?:,\d+)? \+(\d+)(?:,\d+)? @@") {
            $nextNewLine = [int]::Parse($Matches[1], [System.Globalization.CultureInfo]::InvariantCulture)
            continue
        }
        if (-not $line.StartsWith("+") -or $line.StartsWith("+++")) {
            if ($line.StartsWith(" ") -and $nextNewLine -gt 0) {
                $nextNewLine++
            }
            continue
        }
        if (-not (Test-ProductionRustSourcePath -Path $currentFile)) {
            continue
        }

        $addedLineNumber = $nextNewLine
        if ($nextNewLine -gt 0) {
            $nextNewLine++
        }

        $added = $line.Substring(1)
        $trimmed = $added.TrimStart()
        if ($trimmed.StartsWith("//")) {
            continue
        }

        if ($added -match $panicPattern) {
            [void]$candidates.Add([pscustomobject]@{
                    File = $currentFile
                    Line = $addedLineNumber
                    Text = $trimmed
                })
        }
    }

    if ($candidates.Count -eq 0) {
        Pass "Rust panic patterns"
        return
    }

    $candidateFiles = [string[]]@($candidates | ForEach-Object { $_.File } | Sort-Object -Unique)
    if ($candidateFiles.Count -gt 2) {
        try {
            Add-IndexTextCache -Pathspecs $candidateFiles
        } catch {
            Fail "Rust panic patterns" $_.Exception.Message
            return
        }
    }

    $orderedCandidates = @($candidates | Sort-Object -Property File, Line)
    $maxCandidateLineByFile = [System.Collections.Generic.Dictionary[string, int]]::new([System.StringComparer]::Ordinal)
    foreach ($candidate in $orderedCandidates) {
        if ((-not $maxCandidateLineByFile.ContainsKey($candidate.File)) -or
            $candidate.Line -gt $maxCandidateLineByFile[$candidate.File]) {
            $maxCandidateLineByFile[$candidate.File] = [int]$candidate.Line
        }
    }

    $testLinesByFile = [System.Collections.Generic.Dictionary[string, object]]::new([System.StringComparer]::Ordinal)
    foreach ($candidate in $orderedCandidates) {
        if (-not $testLinesByFile.ContainsKey($candidate.File)) {
            $content = Get-IndexText -Path $candidate.File
            $testLinesByFile[$candidate.File] = Get-RustTestLineSet -Content $content -MaxLine $maxCandidateLineByFile[$candidate.File]
        }

        $testLines = $testLinesByFile[$candidate.File]
        if ($testLines.Contains($candidate.Line)) {
            continue
        }

        [void]$violations.Add("$($candidate.File): $($candidate.Text)")
        break
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

function Repair-SkillsIndexIfNeeded {
    $triggered = @($script:StagedFiles | Where-Object {
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

if ($SourceOnly) {
    return
}

function Complete-PreCommit {
    $script:PreCommitTimer.Stop()
    Write-Host "[pre-commit] Completed in $($script:PreCommitTimer.ElapsedMilliseconds)ms"

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

$timer = [System.Diagnostics.Stopwatch]::StartNew()
$script:PreCommitTimer = $timer
$script:RepoRoot = (Invoke-Git -Arguments @("rev-parse", "--show-toplevel")).Stdout.Trim()
Set-Location $script:RepoRoot
$script:StagedFiles = @(Get-StagedFiles)

Write-Step "Running fast last-resort checks..."
$hasProductionRust = @($script:StagedFiles | Where-Object { Test-ProductionRustSourcePath -Path $_ }).Count -gt 0
if (-not $hasProductionRust) {
    if (-not (Invoke-Check "Hook speed policy" { Test-FastHookSource })) { Complete-PreCommit }
}
if (-not (Invoke-Check "Rust panic patterns" { Test-RustAddedPanicPatterns })) { Complete-PreCommit }
if (-not (Invoke-Check "Staged diff whitespace" { Test-Whitespace })) { Complete-PreCommit }

if ($hasProductionRust) {
    Complete-PreCommit
}

Add-StagedContentPreload
if ($null -ne $script:PreloadError) {
    Fail "Staged content preload" $script:PreloadError
    Complete-PreCommit
}

if (-not (Invoke-Check "Skills index freshness" { Repair-SkillsIndexIfNeeded })) { Complete-PreCommit }
if (-not (Invoke-Check "LLM file sizes" { Test-LlmFileSizes })) { Complete-PreCommit }
if (-not (Invoke-Check "README badge styles" { Test-ReadmeBadges })) { Complete-PreCommit }

Complete-PreCommit
