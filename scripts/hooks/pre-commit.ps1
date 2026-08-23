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
$script:UseWorktreeTextForStagedFiles = $false
$script:RustPanicPolicyLoaded = $false
$script:StagedChangedFileSet = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::Ordinal)
$script:WorktreeChangedFileSet = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::Ordinal)
$script:WorktreeUntrackedFileSet = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::Ordinal)
$script:HookBudgetMs = 1000
$script:CheckTimings = [System.Collections.Generic.List[object]]::new()
$script:HookPolicyFiles = @(
    ".githooks/pre-commit",
    ".githooks/pre-push",
    "scripts/hooks/native-process.ps1",
    "scripts/hooks/pre-commit.ps1",
    "scripts/hooks/pre-commit-rust.ps1",
    "scripts/hooks/pre-push.ps1"
)
$script:SkillsIndexToolingFiles = @(
    "scripts/generate-skills-index.sh",
    ".llm/skills/manage-skills/scripts/generate_skills_index.py",
    ".llm/skills/manage-skills/scripts/validate_skills.py"
)
$script:HookPolicyChangedFiles = @()
$script:DocVersionSyncFiles = @("docs/library-usage.md", ".llm/context.md")
$script:WorktreePolicyPathspecs = @(
    "src",
    ".llm",
    "README.md",
    "scripts/generate-skills-index.sh",
    "Cargo.toml"
) + $script:DocVersionSyncFiles + $script:HookPolicyFiles
$script:PendingWorktreeStatus = $null
$script:PendingWorktreeRustDiff = $null
$script:WorktreeDiscoveryTimer = $null
$script:RustPanicGrepPattern = "((^|[^[:alnum:]_])(panic|todo|unimplemented|unreachable)[[:space:]]*(/\*[^*]*\*+([^/*][^*]*\*+)*/[[:space:]]*)*!)|(#\[[[:space:]]*(cfg[[:space:]]*\(|test|tokio::test|async_std::test|rstest))"

. (Join-Path $PSScriptRoot "native-process.ps1")

# Worktree discovery is dominated by refreshing the Git index on repository
# filesystems. Start it before PowerShell registers the policy functions below,
# so that unavoidable I/O overlaps script setup instead of extending the hook's
# critical path. The result is consumed exactly once by
# Get-WorktreeChangedFiles; staged hooks retain their cheaper index-only query.
if (-not $SourceOnly) {
    $script:RepoRoot = (Invoke-Git -Arguments @("rev-parse", "--show-toplevel")).Stdout.Trim()
    Set-Location $script:RepoRoot
    $script:InspectWorktree = [bool]$Worktree
    if ($script:InspectWorktree) {
        $statusArguments = @(
            "status", "--porcelain=v1", "-z", "--untracked-files=all", "--ignored=no", "--no-renames", "--"
        ) + $script:WorktreePolicyPathspecs
        $statusStartInfo = New-NativeProcessStartInfo -FileName "git" -Arguments $statusArguments
        $statusProcess = [System.Diagnostics.Process]::Start($statusStartInfo)
        $script:WorktreeDiscoveryTimer = [System.Diagnostics.Stopwatch]::StartNew()
        $script:PendingWorktreeStatus = [pscustomobject]@{
            Process = $statusProcess
            StdoutTask = $statusProcess.StandardOutput.ReadToEndAsync()
            StderrTask = $statusProcess.StandardError.ReadToEndAsync()
        }
        $rustDiffArguments = @(
            "diff", "HEAD", "--unified=0", "--no-color", "-G", $script:RustPanicGrepPattern, "--", "src"
        )
        $rustDiffStartInfo = New-NativeProcessStartInfo -FileName "git" -Arguments $rustDiffArguments
        $rustDiffProcess = [System.Diagnostics.Process]::Start($rustDiffStartInfo)
        $script:PendingWorktreeRustDiff = [pscustomobject]@{
            Process = $rustDiffProcess
            StdoutTask = $rustDiffProcess.StandardOutput.ReadToEndAsync()
            StderrTask = $rustDiffProcess.StandardError.ReadToEndAsync()
        }
    }
}

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

    # --diff-filter=ACDMR already includes C (Copied): the letters are
    # A(dded) C(opied) D(eleted) M(odified) R(enamed). It is not "missing" C.
    # We also do not pass -C/--find-copies (omitted for speed), so git never
    # emits C anyway — a copied file surfaces as A — making any extra "C" a no-op.
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
        [Parameter(Mandatory = $true)][AllowEmptyCollection()][System.Collections.Generic.HashSet[string]]$SeenPaths,
        [AllowNull()][System.Collections.Generic.HashSet[string]]$ChangedSet = $null
    )

    if ([string]::IsNullOrEmpty($NulDelimitedPaths)) {
        return
    }

    foreach ($path in $NulDelimitedPaths.Split([char]0, [System.StringSplitOptions]::RemoveEmptyEntries)) {
        if ($null -ne $ChangedSet) {
            [void]$ChangedSet.Add($path)
        }
        if ($SeenPaths.Add($path)) {
            [void]$OrderedPaths.Add($path)
        }
    }
}

function Get-WorktreeChangedFiles {
    param([string[]]$Pathspecs = @())

    $orderedPaths = [System.Collections.Generic.List[string]]::new()
    $seenPaths = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::Ordinal)
    $script:StagedChangedFileSet.Clear()
    $script:WorktreeChangedFileSet.Clear()
    $script:WorktreeUntrackedFileSet.Clear()

    # One porcelain query replaces separate staged, unstaged, and untracked
    # discovery processes. `-z` makes paths literal and NUL-delimited; rename
    # records carry a second NUL-delimited source path, which is skipped because
    # policy evaluates the current destination.
    # Policy checks only need the current paths, so skip rename similarity
    # detection. Git will report those changes as delete/add records instead.
    $arguments = @("status", "--porcelain=v1", "-z", "--untracked-files=all", "--ignored=no", "--no-renames", "--") + $Pathspecs
    if ($null -eq $script:PendingWorktreeStatus) {
        $statusResult = Invoke-Git -Arguments $arguments
    } else {
        $pending = $script:PendingWorktreeStatus
        $script:PendingWorktreeStatus = $null
        try {
            $pending.Process.WaitForExit()
            $stdout = $pending.StdoutTask.GetAwaiter().GetResult()
            $stderr = $pending.StderrTask.GetAwaiter().GetResult()
            $statusResult = [pscustomobject]@{
                ExitCode = $pending.Process.ExitCode
                Stdout = $stdout
                Stderr = $stderr
                Output = $stdout + $stderr
            }
        } finally {
            $pending.Process.Dispose()
        }
        if ($statusResult.ExitCode -ne 0) {
            throw "git $($arguments -join ' ') failed:`n$($statusResult.Output)"
        }
    }
    $records = $statusResult.Stdout.Split([char]0, [System.StringSplitOptions]::RemoveEmptyEntries)
    $skipRenameSource = $false
    foreach ($record in $records) {
        if ($skipRenameSource) {
            $skipRenameSource = $false
            continue
        }
        if ($record.Length -lt 4 -or $record[2] -ne " ") {
            throw "Unable to parse git status porcelain record: $record"
        }

        $indexStatus = [string]$record[0]
        $worktreeStatus = [string]$record[1]
        $path = $record.Substring(3)
        if ($seenPaths.Add($path)) {
            [void]$orderedPaths.Add($path)
        }

        if ($indexStatus -eq "?" -and $worktreeStatus -eq "?") {
            [void]$script:WorktreeChangedFileSet.Add($path)
            [void]$script:WorktreeUntrackedFileSet.Add($path)
        } else {
            if ($indexStatus -ne " ") {
                [void]$script:StagedChangedFileSet.Add($path)
            }
            if ($worktreeStatus -ne " ") {
                [void]$script:WorktreeChangedFileSet.Add($path)
            }
        }

        $skipRenameSource = $indexStatus -in @("R", "C") -or $worktreeStatus -in @("R", "C")
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
    $result = Invoke-Git -Arguments @("ls-files", "-s", "-z", "--", ":(glob).llm/skills/*/SKILL.md")
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

function ConvertTo-LfText {
    param([AllowNull()][string]$Text)

    if ($null -eq $Text) {
        return $null
    }

    $Text -replace "`r`n", "`n" -replace "`r", "`n"
}

function Test-WorktreeTextMatchesIndexText {
    param(
        [Parameter(Mandatory = $true)][string]$Path,
        [Parameter(Mandatory = $true)][AllowEmptyString()][string]$IndexContent
    )

    $worktreeText = Get-WorktreeText -Path $Path
    if ($null -eq $worktreeText) {
        return $false
    }

    (ConvertTo-LfText -Text $worktreeText) -eq (ConvertTo-LfText -Text $IndexContent)
}

function Set-WorktreeText {
    param(
        [Parameter(Mandatory = $true)][string]$Path,
        [Parameter(Mandatory = $true)][AllowEmptyString()][string]$Content
    )

    $absolutePath = Join-Path $script:RepoRoot $Path
    [System.IO.File]::WriteAllText($absolutePath, $Content, [System.Text.UTF8Encoding]::new($false))
    $script:WorktreeTextCache[$Path] = $Content
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
    $useWorktreeText = $script:InspectWorktree -or $script:UseWorktreeTextForStagedFiles

    foreach ($file in $stagedHookFiles) {
        $content = if ($useWorktreeText) {
            Get-WorktreeText -Path $file
        } else {
            Get-PolicyText -Path $file
        }
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
            if ($trimmed.IndexOf("cargo", [System.StringComparison]::OrdinalIgnoreCase) -lt 0 -and
                $trimmed.IndexOf("npm", [System.StringComparison]::OrdinalIgnoreCase) -lt 0 -and
                $trimmed.IndexOf("npx", [System.StringComparison]::OrdinalIgnoreCase) -lt 0) {
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
            $script:SkillsIndexToolingFiles -contains $_ -or
            ($_.StartsWith(".llm/skills/") -and $_.EndsWith(".md"))
        }).Count -gt 0
    $preloadPaths = @($script:StagedFiles | Where-Object {
            ($script:HookPolicyFiles -contains $_) -or
            ($_.StartsWith(".llm/") -and $_.EndsWith(".md") -and
                -not ($skillsIndexTriggered -and $_.StartsWith(".llm/skills/"))) -or
            $_ -eq "README.md"
        })

    $preloadTimer = [System.Diagnostics.Stopwatch]::StartNew()
    try {
        if ($preloadPaths.Count -gt $script:PreloadBatchThreshold) {
            Add-IndexTextCache -Pathspecs $preloadPaths
        }
    } catch {
        $script:PreloadError = $_.Exception.Message
    } finally {
        $preloadTimer.Stop()
        [void]$script:CheckTimings.Add([pscustomobject]@{
                Name = "Staged content preload"
                ElapsedMilliseconds = [long]$preloadTimer.ElapsedMilliseconds
            })
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

function Test-RustPanicPolicyRelevantDiff {
    param([Parameter(Mandatory = $true)][AllowEmptyCollection()][string[]]$Pathspecs)

    if ($Pathspecs.Count -eq 0) {
        return $false
    }

    $hasUntrackedProductionRust = $script:InspectWorktree -and @($Pathspecs | Where-Object {
                $script:WorktreeUntrackedFileSet.Contains($_)
            }).Count -gt 0

    $diffBase = if ($script:InspectWorktree) { "HEAD" } else { "--cached" }
    if ($null -eq $script:PendingWorktreeRustDiff) {
        $result = Invoke-Native -FileName "git" -Arguments (@(
                "diff", $diffBase, "--unified=0", "--no-color", "-G", $script:RustPanicGrepPattern, "--"
            ) + $Pathspecs)
    } else {
        $pending = $script:PendingWorktreeRustDiff
        $script:PendingWorktreeRustDiff = $null
        try {
            $pending.Process.WaitForExit()
            $stdout = $pending.StdoutTask.GetAwaiter().GetResult()
            $stderr = $pending.StderrTask.GetAwaiter().GetResult()
            $result = [pscustomobject]@{
                ExitCode = $pending.Process.ExitCode
                Stdout = $stdout
                Stderr = $stderr
                Output = $stdout + $stderr
            }
        } finally {
            $pending.Process.Dispose()
        }
    }
    # `git diff HEAD` cannot see untracked files, so keep their full scanner
    # fail-closed after consuming the already-started query above.
    if ($hasUntrackedProductionRust) {
        return $true
    }
    if ($result.ExitCode -ne 0 -and $script:InspectWorktree -and $result.Output -match "unknown revision|bad revision|ambiguous argument 'HEAD'|Not a valid object name HEAD") {
        return $true
    }
    if ($result.ExitCode -ne 0) {
        throw "git diff Rust panic policy precheck failed:`n$($result.Output)"
    }

    # The full, line-aware Rust scanner is needed only when a panic macro is
    # added or a test-context guard is removed. A new #[test]/#[cfg(test)] can
    # only narrow production exposure, while removed panic macros cannot create
    # a new production panic. Parsing zero-context matching hunks avoids loading
    # the large scanner for those safe, common test-only additions.
    $panicLinePattern = '(^|[^A-Za-z0-9_])(panic|todo|unimplemented|unreachable)\s*(/\*.*\*/\s*)*!'
    $testContextLinePattern = '#\[\s*(cfg\s*\(|test|tokio::test|async_std::test|rstest)'
    foreach ($rawLine in $result.Stdout -split "`n") {
        $line = $rawLine.TrimEnd("`r")
        if ($line.StartsWith("+++") -or $line.StartsWith("---")) {
            continue
        }
        if ($line.StartsWith("+") -and $line.Substring(1) -match $panicLinePattern) {
            return $true
        }
        if ($line.StartsWith("-") -and $line.Substring(1) -match $testContextLinePattern) {
            return $true
        }
    }

    return $false
}

function Test-WorktreeIndexPolicyConsistency {
    if (-not $script:InspectWorktree) {
        return
    }

    $conflicts = [System.Collections.Generic.List[string]]::new()
    foreach ($path in $script:StagedChangedFileSet) {
        if ($script:WorktreeChangedFileSet.Contains($path)) {
            [void]$conflicts.Add($path)
        }
    }

    if ($conflicts.Count -eq 0) {
        Skip "Worktree/index policy consistency" "no staged policy paths with unstaged edits"
        return
    }

    $conflictList = ([string[]]@($conflicts | Sort-Object)) -join "`n"
    Fail "Worktree/index policy consistency" "These policy paths have staged content that differs from the worktree. The real git hook validates the staged snapshot, so -Worktree cannot prove the commit is safe until the index and worktree agree.`n$conflictList"
}


function Get-SkillMetadataFromText {
    param([AllowNull()][string]$Content)

    if ([string]::IsNullOrEmpty($Content)) {
        return $null
    }

    $name = ""
    $title = ""
    $descriptionLines = [System.Collections.Generic.List[string]]::new()
    $readingDescription = $false
    $frontmatterClosed = $false
    foreach ($line in $Content -split "`r?`n") {
        if (-not $frontmatterClosed) {
            if ($line.StartsWith("name:")) {
                $name = $line.Substring("name:".Length).Trim()
                continue
            }
            if ($line.StartsWith("description:")) {
                $inlineDescription = $line.Substring("description:".Length).Trim()
                if ($inlineDescription -notin @(">", ">-", "|", "|-")) {
                    [void]$descriptionLines.Add($inlineDescription.Trim('"', "'"))
                }
                $readingDescription = $true
                continue
            }
            if ($line -eq "---" -and -not [string]::IsNullOrWhiteSpace($name)) {
                $frontmatterClosed = $true
                $readingDescription = $false
                continue
            }
            if ($readingDescription -and $line.StartsWith("  ")) {
                [void]$descriptionLines.Add($line.Trim())
            }
            continue
        }

        if ($line.StartsWith("# ")) {
            $title = $line.Substring(2).Trim()
            break
        }
    }

    $description = ([string[]]@($descriptionLines)) -join " "
    if ([string]::IsNullOrWhiteSpace($name) -or [string]::IsNullOrWhiteSpace($description) -or [string]::IsNullOrWhiteSpace($title)) {
        return $null
    }

    [pscustomobject]@{ Name = $name; Description = $description; Title = $title }
}

function New-SkillsIndexContent {
    $lines = [System.Collections.Generic.List[string]]::new()
    [void]$lines.Add("# Skills Index")
    [void]$lines.Add("")
    [void]$lines.Add("> WARNING: Auto-generated by")
    [void]$lines.Add("> ``.llm/skills/manage-skills/scripts/generate_skills_index.py``. Do not edit manually.")
    [void]$lines.Add("")
    [void]$lines.Add("Agents should select a skill from its description, then load only that ``SKILL.md`` and the")
    [void]$lines.Add("resources it directs them to read or run.")
    [void]$lines.Add("")
    [void]$lines.Add("## Available Skills")
    [void]$lines.Add("")
    [void]$lines.Add("<!-- markdown" + "lint-disable MD013 -->")
    [void]$lines.Add("")

    $objectIdsByPath = Get-IndexSkillFileObjectIds
    $paths = [string[]]@($objectIdsByPath.Keys)
    [array]::Sort($paths, [System.StringComparer]::Ordinal)
    $blobTextsByObjectId = Get-IndexBlobTexts -ObjectIds ([string[]]@($paths | ForEach-Object { $objectIdsByPath[$_] }))

    foreach ($path in $paths) {
        $objectId = $objectIdsByPath[$path]
        if (-not $blobTextsByObjectId.ContainsKey($objectId)) {
            continue
        }
        $content = $blobTextsByObjectId[$objectId]
        $script:IndexTextCache[$path] = $content
        $metadata = Get-SkillMetadataFromText -Content $content
        if ($null -ne $metadata) {
            $relativePath = $path.Substring(".llm/skills/".Length)
            [void]$lines.Add("- [$($metadata.Title)]($relativePath) (``$($metadata.Name)``) — $($metadata.Description)")
        }
    }

    [void]$lines.Add("")
    [void]$lines.Add("<!-- markdown" + "lint-enable MD013 -->")

    ($lines -join "`n") + "`n"
}

function ConvertTo-GitRelativePath {
    param([Parameter(Mandatory = $true)][string]$Path)

    $relative = [System.IO.Path]::GetRelativePath($script:RepoRoot, $Path)
    $relative.Replace([System.IO.Path]::DirectorySeparatorChar, "/").Replace([System.IO.Path]::AltDirectorySeparatorChar, "/")
}

function New-WorktreeSkillsIndexContent {
    $lines = [System.Collections.Generic.List[string]]::new()
    [void]$lines.Add("# Skills Index")
    [void]$lines.Add("")
    [void]$lines.Add("> WARNING: Auto-generated by")
    [void]$lines.Add("> ``.llm/skills/manage-skills/scripts/generate_skills_index.py``. Do not edit manually.")
    [void]$lines.Add("")
    [void]$lines.Add("Agents should select a skill from its description, then load only that ``SKILL.md`` and the")
    [void]$lines.Add("resources it directs them to read or run.")
    [void]$lines.Add("")
    [void]$lines.Add("## Available Skills")
    [void]$lines.Add("")
    [void]$lines.Add("<!-- markdown" + "lint-disable MD013 -->")
    [void]$lines.Add("")

    $skillsRoot = Join-Path $script:RepoRoot ".llm/skills"
    if (-not (Test-Path -LiteralPath $skillsRoot)) {
        return ($lines -join "`n") + "`n"
    }

    $paths = [string[]]@(
        [System.IO.Directory]::EnumerateFiles($skillsRoot, "SKILL.md", [System.IO.SearchOption]::AllDirectories) |
            ForEach-Object { ConvertTo-GitRelativePath -Path $_ }
    )
    [array]::Sort($paths, [System.StringComparer]::Ordinal)

    foreach ($path in $paths) {
        $absolutePath = Join-Path $script:RepoRoot $path
        try {
            $content = [System.IO.File]::ReadAllText($absolutePath)
        } catch {
            continue
        }
        $metadata = Get-SkillMetadataFromText -Content $content
        if ($null -ne $metadata) {
            $relativePath = $path.Substring(".llm/skills/".Length)
            [void]$lines.Add("- [$($metadata.Title)]($relativePath) (``$($metadata.Name)``) — $($metadata.Description)")
        }
    }

    [void]$lines.Add("")
    [void]$lines.Add("<!-- markdown" + "lint-enable MD013 -->")

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

function Test-StagedSkillsIndexDefinitelyFresh {
    param([Parameter(Mandatory = $true)][AllowEmptyCollection()][string[]]$ChangedSkillFiles)

    if ($ChangedSkillFiles.Count -eq 0) {
        return $false
    }

    if ($ChangedSkillFiles -contains ".llm/skills/index.md") {
        return $false
    }

    $fileListChanges = Invoke-Native -FileName "git" -Arguments (@("diff", "--cached", "--name-only", "--diff-filter=ACDRTUXB", "HEAD", "--") + $ChangedSkillFiles)
    if ($fileListChanges.ExitCode -ne 0) {
        if ($fileListChanges.Output -match "unknown revision|bad revision|ambiguous argument 'HEAD'|Not a valid object name HEAD") {
            return $false
        }
        throw "git diff staged skill file-list check failed:`n$($fileListChanges.Output)"
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
        throw "git grep staged skill title check failed:`n$($headTitleResult.Output)"
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

        $stagedTitle = ""
        $stagedText = Get-IndexText -Path $file
        if ($null -eq $stagedText) {
            return $false
        }
        foreach ($line in $stagedText -split "`r?`n") {
            if ($line.StartsWith("# Skill:")) {
                $stagedTitle = $line.Substring("# Skill:".Length).Trim()
                break
            }
        }
        if (-not [System.StringComparer]::Ordinal.Equals($headTitles[$file], $stagedTitle)) {
            return $false
        }
    }

    $true
}

function Get-SkillTitleFromText {
    param([AllowNull()][string]$Content)

    if ([string]::IsNullOrEmpty($Content)) {
        return ""
    }

    foreach ($line in $Content -split "`r?`n") {
        if ($line.StartsWith("# Skill:")) {
            return $line.Substring("# Skill:".Length).Trim()
        }
    }

    ""
}

function Test-SkillsIndexCoversChangedSkills {
    param(
        [Parameter(Mandatory = $true)][AllowEmptyCollection()][string[]]$ChangedSkillFiles,
        [switch]$UseWorktree
    )

    $indexText = if ($UseWorktree) {
        Get-WorktreeText -Path ".llm/skills/index.md"
    } else {
        Get-PolicyText -Path ".llm/skills/index.md"
    }
    if ([string]::IsNullOrEmpty($indexText)) {
        return $false
    }

    $indexLines = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::Ordinal)
    foreach ($line in $indexText -split "`r?`n") {
        [void]$indexLines.Add($line)
    }

    $checked = 0
    foreach ($file in $ChangedSkillFiles) {
        if ($file -eq ".llm/skills/index.md") {
            continue
        }

        $relativePath = $file.Substring(".llm/skills/".Length)
        $content = if ($UseWorktree) {
            Get-WorktreeText -Path $file
        } else {
            Get-PolicyText -Path $file
        }
        $entryMarker = "]($relativePath) ("
        $matchingIndexEntries = @($indexLines | Where-Object { $_.Contains($entryMarker, [System.StringComparison]::Ordinal) })
        $checked++

        if ($null -eq $content) {
            if ($matchingIndexEntries.Count -gt 0) {
                return $false
            }
            continue
        }

        $metadata = Get-SkillMetadataFromText -Content $content
        if ($null -eq $metadata) {
            return $false
        }
        $expected = "- [$($metadata.Title)]($relativePath) (``$($metadata.Name)``) — $($metadata.Description)"

        if (-not $indexLines.Contains($expected)) {
            return $false
        }
    }

    $checked -gt 0
}

function Test-WorktreeMatchesIndexForPaths {
    param([Parameter(Mandatory = $true)][AllowEmptyCollection()][string[]]$Paths)

    if ($Paths.Count -eq 0) {
        return $true
    }

    $result = Invoke-Native -FileName "git" -Arguments (@("diff", "--quiet", "--") + $Paths)
    if ($result.ExitCode -eq 0) {
        return $true
    }
    if ($result.ExitCode -eq 1) {
        return $false
    }

    throw "git diff worktree/index comparison failed:`n$($result.Output)"
}

function Repair-SkillsIndexIfNeeded {
    $changedSkillFiles = [string[]]@($script:StagedFiles | Where-Object {
            $_ -eq ".llm/skills/index.md" -or
            ($_.StartsWith(".llm/skills/") -and $_.EndsWith("/SKILL.md"))
        })
    $skillsToolingChanged = @($script:StagedFiles | Where-Object {
            $script:SkillsIndexToolingFiles -contains $_
        }).Count -gt 0
    $triggered = $skillsToolingChanged -or @($script:StagedFiles | Where-Object {
            $_ -eq ".llm/skills/index.md" -or
            ($_.StartsWith(".llm/skills/") -and $_.EndsWith("/SKILL.md"))
        }).Count -gt 0
    if (-not $triggered) {
        Skip "Skills index freshness" "no skills index inputs staged"
        return
    }

    $indexCoversChangedSkills = $false
    if (
        (-not $skillsToolingChanged) -and
        ($script:StagedFiles -contains ".llm/skills/index.md")
    ) {
        if ($script:InspectWorktree) {
            $indexCoversChangedSkills = Test-SkillsIndexCoversChangedSkills -ChangedSkillFiles $changedSkillFiles
        } elseif ($script:UseWorktreeTextForStagedFiles) {
            $indexCoversChangedSkills = Test-SkillsIndexCoversChangedSkills -ChangedSkillFiles $changedSkillFiles -UseWorktree
        } else {
            Add-IndexTextCache -Pathspecs ([string[]]@((".llm/skills/index.md") + $changedSkillFiles))
            $indexCoversChangedSkills = Test-SkillsIndexCoversChangedSkills -ChangedSkillFiles $changedSkillFiles
        }
    }

    if ($indexCoversChangedSkills) {
        Pass "Skills index freshness"
        return
    }

    if ($script:InspectWorktree) {
        if (
            (-not $skillsToolingChanged) -and
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

    if (
        (-not $skillsToolingChanged) -and
        (Test-StagedSkillsIndexDefinitelyFresh -ChangedSkillFiles $changedSkillFiles)
    ) {
        Pass "Skills index freshness"
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

    $canUpdateWorktree = Test-WorktreeTextMatchesIndexText -Path ".llm/skills/index.md" -IndexContent $actual
    if (-not $canUpdateWorktree) {
        Fail "Skills index freshness" "Unable to auto-repair .llm/skills/index.md because the working tree has unstaged edits. Run scripts/generate-skills-index.sh before committing."
        return
    }

    $expectedHash = Set-IndexText -Path ".llm/skills/index.md" -Content $expected
    $updatedHash = Get-IndexObjectId -Path ".llm/skills/index.md"

    if ($updatedHash -eq $expectedHash) {
        Set-WorktreeText -Path ".llm/skills/index.md" -Content $expected
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

        $canUpdateWorktree = Test-WorktreeTextMatchesIndexText -Path $path -IndexContent $actual
        if (-not $canUpdateWorktree) {
            Fail "Doc version sync" "Unable to auto-sync $path because the working tree has unstaged edits. Update the file before committing."
            return
        }

        $expectedHash = Set-IndexText -Path $path -Content $expected
        $updatedHash = Get-IndexObjectId -Path $path
        if ($updatedHash -ne $expectedHash) {
            Fail "Doc version sync" "Unable to re-stage $path with crate version $version."
            return
        }
        Set-WorktreeText -Path $path -Content $expected
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

    $llmFiles = @($script:StagedFiles | Where-Object {
            $_.StartsWith(".llm/") -and
            $_.EndsWith(".md") -and
            $_ -ne ".llm/skills/index.md"
        })
    if ($llmFiles.Count -eq 0) {
        Skip "LLM file sizes" "no checked .llm/*.md files staged"
        return
    }

    $violations = [System.Collections.Generic.List[string]]::new()
    $useWorktreeText = $script:InspectWorktree -or $script:UseWorktreeTextForStagedFiles

    foreach ($file in $llmFiles) {
        $content = if ($useWorktreeText) {
            Get-WorktreeText -Path $file
        } else {
            Get-PolicyText -Path $file
        }
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
    . (Join-Path $PSScriptRoot "pre-commit-rust.ps1")
    $script:RustPanicPolicyLoaded = $true
    return
}

function Complete-PreCommit {
    if ($null -ne $script:PendingWorktreeRustDiff) {
        $pending = $script:PendingWorktreeRustDiff
        $script:PendingWorktreeRustDiff = $null
        try {
            $pending.Process.WaitForExit()
            [void]$pending.StdoutTask.GetAwaiter().GetResult()
            [void]$pending.StderrTask.GetAwaiter().GetResult()
        } finally {
            $pending.Process.Dispose()
        }
    }
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

$changedFilesTimer = if ($script:InspectWorktree) {
    $script:WorktreeDiscoveryTimer
} else {
    [System.Diagnostics.Stopwatch]::StartNew()
}
$allChangedFiles = [string[]]@(if ($script:InspectWorktree) {
        Get-WorktreeChangedFiles -Pathspecs $script:WorktreePolicyPathspecs
    } else {
        Get-StagedFiles
    })
$changedFilesTimer.Stop()
Write-Profile -Name "Changed file discovery" -ElapsedMilliseconds $changedFilesTimer.ElapsedMilliseconds
[void]$script:CheckTimings.Add([pscustomobject]@{
        Name = "Changed file discovery"
        ElapsedMilliseconds = [long]$changedFilesTimer.ElapsedMilliseconds
    })
$script:HookPolicyChangedFiles = [string[]]@($allChangedFiles | Where-Object { $script:HookPolicyFiles -contains $_ })
$script:StagedFiles = $allChangedFiles
if ((-not $script:InspectWorktree) -and $allChangedFiles.Count -gt 0) {
    $script:UseWorktreeTextForStagedFiles = Test-WorktreeMatchesIndexForPaths -Paths $allChangedFiles
}

if ($script:InspectWorktree) {
    Write-Step "Running fast worktree preflight checks..."
} else {
    Write-Step "Running fast last-resort checks..."
}

if ($script:InspectWorktree) {
    if (-not (Invoke-Check "Worktree/index policy consistency" { Test-WorktreeIndexPolicyConsistency })) { Complete-PreCommit }
}

# Keep the crate version and the docs that quote it in lockstep for every
# commit shape (runs before the Rust early-exit below so a Cargo.toml bump that
# also touches Rust still auto-syncs).
if (-not (Invoke-Check "Doc version sync" { Repair-DocVersionsIfNeeded -ChangedFiles $allChangedFiles })) { Complete-PreCommit }

if (-not (Invoke-Check "Hook speed policy" { Test-FastHookSource })) { Complete-PreCommit }
$changedProductionRustFiles = [string[]]@($allChangedFiles | Where-Object { Test-ProductionRustSourcePath -Path $_ })
if ($changedProductionRustFiles.Count -eq 0) {
    if (-not (Invoke-Check "Rust panic patterns" { Skip "Rust panic patterns" "no production Rust files staged" })) { Complete-PreCommit }
} elseif (-not (Test-RustPanicPolicyRelevantDiff -Pathspecs $changedProductionRustFiles)) {
    if (-not (Invoke-Check "Rust panic patterns" { Pass "Rust panic patterns" })) { Complete-PreCommit }
} else {
    if (-not (Invoke-Check "Rust panic patterns" {
                if (-not $script:RustPanicPolicyLoaded) {
                    . (Join-Path $PSScriptRoot "pre-commit-rust.ps1")
                    $script:RustPanicPolicyLoaded = $true
                }
                Test-RustAddedPanicPatterns
            })) { Complete-PreCommit }
}

$metadataPolicyTriggered = @($allChangedFiles | Where-Object {
        $script:SkillsIndexToolingFiles -contains $_ -or
        ($_.StartsWith(".llm/") -and $_.EndsWith(".md")) -or
        $_ -eq "README.md"
    }).Count -gt 0
if (-not $metadataPolicyTriggered) {
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
