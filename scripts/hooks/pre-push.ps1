#requires -Version 7.0
param(
    [string]$RemoteName = "",
    [string]$RemoteUrl = "",
    [switch]$Worktree,
    [switch]$SourceOnly,
    [switch]$EnforceBudget
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$script:Passed = 0
$script:Failed = 0
$script:Skipped = 0
$script:WorktreePseudoCommit = "__WORKTREE__"
$script:HookBudgetMs = 1000
$script:MaxBatchedBlobBytes = 8 * 1024 * 1024
$script:CommitBlobTextCache = [System.Collections.Generic.Dictionary[string, string]]::new([System.StringComparer]::Ordinal)
$script:MissingCommitBlobCache = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::Ordinal)

. (Join-Path $PSScriptRoot "native-process.ps1")

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
    Write-Host "[pre-push] ERROR: $Message"
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

function Split-NulOutput {
    param([string]$Text)
    if ([string]::IsNullOrEmpty($Text)) {
        return @()
    }
    $Text.Split([char]0, [System.StringSplitOptions]::RemoveEmptyEntries)
}

function Add-ChangedFile {
    param(
        [System.Collections.Generic.Dictionary[string, System.Collections.Generic.HashSet[string]]]$Map,
        [string]$Commit,
        [string]$File
    )

    if (-not $Map.ContainsKey($File)) {
        $Map[$File] = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::Ordinal)
    }
    [void]$Map[$File].Add($Commit)
}

function Get-RevList {
    param(
        [Parameter(Mandatory = $true)][string]$LocalSha,
        [Parameter(Mandatory = $true)][string]$RemoteSha,
        [Parameter(Mandatory = $true)][string]$AllZeroSha,
        [AllowEmptyString()][string]$RemoteName = ""
    )

    if ($RemoteSha -eq $AllZeroSha) {
        $remoteArg = if ([string]::IsNullOrWhiteSpace($RemoteName)) { "--remotes" } else { "--remotes=$RemoteName" }
        $result = Invoke-Native -FileName "git" -Arguments @("rev-list", $LocalSha, "--not", $remoteArg)
    } else {
        # A force-push after rebasing can place commits from another remote branch
        # outside RemoteSha..LocalSha. Those commits are already present on the
        # target remote, so only inspect commits the push would newly introduce.
        $remoteArg = if ([string]::IsNullOrWhiteSpace($RemoteName)) { "--remotes" } else { "--remotes=$RemoteName" }
        $result = Invoke-Native -FileName "git" -Arguments @("rev-list", $LocalSha, "--not", $RemoteSha, $remoteArg)
    }

    if ($result.ExitCode -ne 0 -and $RemoteSha -ne $AllZeroSha) {
        $remoteArg = if ([string]::IsNullOrWhiteSpace($RemoteName)) { "--remotes" } else { "--remotes=$RemoteName" }
        $result = Invoke-Native -FileName "git" -Arguments @("rev-list", $LocalSha, "--not", $remoteArg)
    }

    if ($result.ExitCode -ne 0) {
        throw "git rev-list failed:`n$($result.Output)"
    }

    @($result.Stdout -split "`n" |
        ForEach-Object { $_.Trim() } |
        Where-Object { -not [string]::IsNullOrWhiteSpace($_) })
}

function Add-ChangedFilesFromCommits {
    param(
        [System.Collections.Generic.Dictionary[string, System.Collections.Generic.HashSet[string]]]$Map,
        [string[]]$Commits
    )

    if ($Commits.Count -eq 0) {
        return
    }

    $diffInput = ($Commits -join "`n") + "`n"
    $diff = Invoke-NativeWithInput -FileName "git" -Arguments @("diff-tree", "--stdin", "--root", "--raw", "-r", "-m", "-z") -InputText $diffInput
    if ($diff.ExitCode -ne 0) {
        throw "git diff-tree --stdin failed:`n$($diff.Output)"
    }

    $entries = @(Split-NulOutput -Text $diff.Stdout)
    $currentCommit = $null
    for ($index = 0; $index -lt $entries.Count; $index++) {
        $entry = $entries[$index]
        if ($entry -match "^[0-9a-fA-F]{40}$") {
            $currentCommit = $entry.ToLowerInvariant()
            continue
        }

        if ($null -eq $currentCommit -or -not $entry.StartsWith(":")) {
            continue
        }

        $fields = @($entry -split "\s+" | Where-Object { $_ -ne "" })
        if ($fields.Count -lt 5) {
            continue
        }

        $status = $fields[$fields.Count - 1]
        $statusCode = $status.Substring(0, 1)
        if ($statusCode -eq "R" -or $statusCode -eq "C") {
            $index++
            if ($index -ge $entries.Count) {
                break
            }
            $index++
            if ($index -ge $entries.Count) {
                break
            }
            Add-ChangedFile -Map $Map -Commit $currentCommit -File $entries[$index]
            continue
        }

        $index++
        if ($index -ge $entries.Count) {
            break
        }
        Add-ChangedFile -Map $Map -Commit $currentCommit -File $entries[$index]
    }
}

function Get-ChangedFilesForPush {
    $files = [System.Collections.Generic.Dictionary[string, System.Collections.Generic.HashSet[string]]]::new([System.StringComparer]::Ordinal)
    $stdin = [Console]::In.ReadToEnd()
    if ([string]::IsNullOrWhiteSpace($stdin)) {
        return $files
    }

    $allZeroSha = "0000000000000000000000000000000000000000"

    foreach ($rawLine in $stdin -split "`n") {
        $line = $rawLine.Trim()
        if ([string]::IsNullOrWhiteSpace($line)) {
            continue
        }

        $parts = $line -split "\s+"
        if ($parts.Count -lt 4) {
            continue
        }

        $localSha = $parts[1]
        $remoteSha = $parts[3]
        if ($localSha -eq $allZeroSha) {
            continue
        }

        $commits = @(Get-RevList -LocalSha $localSha -RemoteSha $remoteSha -AllZeroSha $allZeroSha -RemoteName $script:RemoteName)
        Add-ChangedFilesFromCommits -Map $files -Commits $commits
    }

    $files
}

function Add-UniqueWorktreeFiles {
    param(
        [System.Collections.Generic.Dictionary[string, System.Collections.Generic.HashSet[string]]]$Map,
        [AllowEmptyString()][string]$NulDelimitedPaths
    )

    if ([string]::IsNullOrEmpty($NulDelimitedPaths)) {
        return
    }

    foreach ($file in $NulDelimitedPaths.Split([char]0, [System.StringSplitOptions]::RemoveEmptyEntries)) {
        Add-ChangedFile -Map $Map -Commit $script:WorktreePseudoCommit -File $file
    }
}

function Get-ChangedFilesForWorktreePreflight {
    $files = [System.Collections.Generic.Dictionary[string, System.Collections.Generic.HashSet[string]]]::new([System.StringComparer]::Ordinal)
    $pathspecs = @(".github/workflows", ".githooks", "scripts/hooks")

    $cachedArgs = @("diff", "--cached", "--name-only", "-z", "--diff-filter=ACDMR", "--") + $pathspecs
    Add-UniqueWorktreeFiles -Map $files -NulDelimitedPaths (Invoke-Git -Arguments $cachedArgs).Stdout

    $worktreeArgs = @("diff", "--name-only", "-z", "--diff-filter=ACDMR", "--") + $pathspecs
    Add-UniqueWorktreeFiles -Map $files -NulDelimitedPaths (Invoke-Git -Arguments $worktreeArgs).Stdout

    $untrackedArgs = @("ls-files", "--others", "--exclude-standard", "-z", "--") + $pathspecs
    Add-UniqueWorktreeFiles -Map $files -NulDelimitedPaths (Invoke-Git -Arguments $untrackedArgs).Stdout

    $files
}

function Get-CommitBlobCacheKey {
    param(
        [Parameter(Mandatory = $true)][string]$Commit,
        [Parameter(Mandatory = $true)][string]$File
    )

    "${Commit}`n${File}"
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

function Initialize-CommitBlobTextCache {
    param([System.Collections.Generic.Dictionary[string, System.Collections.Generic.HashSet[string]]]$ChangedFiles)

    $requests = [System.Collections.Generic.List[object]]::new()
    foreach ($file in $ChangedFiles.Keys) {
        $isHookPolicyFile = $file -in @(
            ".githooks/pre-commit",
            ".githooks/pre-push",
            "scripts/hooks/native-process.ps1",
            "scripts/hooks/pre-commit.ps1",
            "scripts/hooks/pre-commit-rust.ps1",
            "scripts/hooks/pre-push.ps1"
        )
        if (-not $isHookPolicyFile -and $file -notmatch "^\.github/workflows/.*\.ya?ml$") {
            continue
        }

        foreach ($commit in $ChangedFiles[$file]) {
            if ($commit -eq $script:WorktreePseudoCommit) {
                continue
            }
            [void]$requests.Add([pscustomobject]@{
                    Expression = "${commit}:${file}"
                    Key = Get-CommitBlobCacheKey -Commit $commit -File $file
                })
        }
    }

    if ($requests.Count -eq 0) {
        return
    }

    $batchInput = (($requests | ForEach-Object { $_.Expression }) -join "`n") + "`n"
    $result = Invoke-NativeBytesWithInput -FileName "git" -Arguments @("cat-file", "--batch") -InputText $batchInput
    if ($result.ExitCode -ne 0) {
        throw "git cat-file --batch failed:`n$($result.Output)"
    }

    $offset = 0
    $totalByteCount = [int64]0
    $utf8NoBom = [System.Text.UTF8Encoding]::new($false)
    foreach ($request in $requests) {
        $header = Read-AsciiLineFromBytes -Bytes $result.StdoutBytes -Offset ([ref]$offset)
        if ($null -eq $header) {
            throw "git cat-file --batch ended before $($request.Expression)"
        }
        if ($header.EndsWith(" missing", [System.StringComparison]::Ordinal)) {
            [void]$script:MissingCommitBlobCache.Add($request.Key)
            continue
        }

        $headerFields = @($header.Split(" ", [System.StringSplitOptions]::RemoveEmptyEntries))
        if ($headerFields.Count -ne 3 -or $headerFields[1] -ne "blob") {
            throw "Unexpected git cat-file header for $($request.Expression): $header"
        }

        $byteCount = [int64]::Parse($headerFields[2], [System.Globalization.CultureInfo]::InvariantCulture)
        $totalByteCount += $byteCount
        if ($totalByteCount -gt $script:MaxBatchedBlobBytes) {
            throw "Pushed hook/workflow blob batch is $totalByteCount bytes, above the $script:MaxBatchedBlobBytes byte pre-push limit."
        }
        if ($byteCount -gt [int]::MaxValue -or $offset + $byteCount -gt $result.StdoutBytes.Length) {
            throw "git cat-file --batch returned truncated or oversized content for $($request.Expression)"
        }

        $script:CommitBlobTextCache[$request.Key] = $utf8NoBom.GetString($result.StdoutBytes, $offset, [int]$byteCount)
        $offset += [int]$byteCount
        if ($offset -lt $result.StdoutBytes.Length -and $result.StdoutBytes[$offset] -eq 10) {
            $offset++
        }
    }
}

function Get-CommitBlobText {
    param(
        [Parameter(Mandatory = $true)][string]$Commit,
        [Parameter(Mandatory = $true)][string]$File
    )

    if ($Commit -eq $script:WorktreePseudoCommit) {
        $path = Join-Path $script:RepoRoot $File
        if (-not (Test-Path -LiteralPath $path)) {
            return $null
        }
        return [System.IO.File]::ReadAllText($path)
    }

    $cacheKey = Get-CommitBlobCacheKey -Commit $Commit -File $File
    if ($script:CommitBlobTextCache.ContainsKey($cacheKey)) {
        return $script:CommitBlobTextCache[$cacheKey]
    }
    if ($script:MissingCommitBlobCache.Contains($cacheKey)) {
        return $null
    }

    $result = Invoke-Native -FileName "git" -Arguments @("show", "${Commit}:$File")
    if ($result.ExitCode -ne 0) {
        return $null
    }
    $result.Stdout
}

function Test-FastHookSource {
    param([System.Collections.Generic.Dictionary[string, System.Collections.Generic.HashSet[string]]]$ChangedFiles)

    $files = @(
        ".githooks/pre-commit",
        ".githooks/pre-push",
        "scripts/hooks/native-process.ps1",
        "scripts/hooks/pre-commit.ps1",
        "scripts/hooks/pre-commit-rust.ps1",
        "scripts/hooks/pre-push.ps1"
    )
    $slowCommandPattern = "^\s*(&\s*)?[""']?cargo[""']?\s+(fmt|clippy|test|doc|check|build|install)\b|^\s*(&\s*)?[""']?npm[""']?\s+(install|ci)\b|^\s*(&\s*)?[""']?npx[""']?\b|Invoke-Native\s+-FileName\s+[""'](cargo|npm|npx)[""']|Start-Process\s+[""']?(cargo|npm|npx)[""']?"
    $violations = [System.Collections.Generic.List[string]]::new()
    $checked = 0

    foreach ($file in $files) {
        if (-not $ChangedFiles.ContainsKey($file)) {
            continue
        }

        foreach ($commit in $ChangedFiles[$file]) {
            $content = Get-CommitBlobText -Commit $commit -File $file
            if ($null -eq $content) {
                continue
            }
            $checked++

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
                    [void]$violations.Add("${file}@${commit}:${lineNumber}: $trimmed")
                }
            }
        }
    }

    if ($violations.Count -gt 0) {
        Fail "Hook speed policy" "Git hooks must not run slow semantic or install commands.`n$($violations -join "`n")"
    } elseif ($checked -eq 0) {
        Skip "Hook speed policy" "no hook files pushed"
    } else {
        Pass "Hook speed policy"
    }
}

function Get-Indent {
    param([string]$Line)
    $Line.Length - $Line.TrimStart().Length
}

function Normalize-LocalScriptToken {
    param([string]$Token)

    $clean = $Token.Trim('"', "'", '`', '(', ')', '[', ']', ',', '{', '}')
    $clean = $clean.TrimEnd(';', ')', ',', ']', '}')
    $path = $clean
    foreach ($prefix in @('${{ github.workspace }}/', '$GITHUB_WORKSPACE/', '${GITHUB_WORKSPACE}/', '$PWD/', '${PWD}/')) {
        if ($path.StartsWith($prefix)) {
            $path = $path.Substring($prefix.Length)
            break
        }
    }
    if ($path.StartsWith("./")) {
        $path = $path.Substring(2)
    }
    $isLocalScript = $path.StartsWith("scripts/") -or $path.StartsWith(".github/scripts/")
    $isScriptFile = $path.EndsWith(".sh") -or
        $path.EndsWith(".awk") -or
        $path.EndsWith(".py") -or
        $path.EndsWith(".ps1") -or
        $path.EndsWith(".bash") -or
        $path.EndsWith(".js")

    if ($isLocalScript -and $isScriptFile) {
        return $path
    }

    $null
}

function Test-InterpreterToken {
    param([string]$Token)

    $clean = $Token.Trim('"', "'", '`', '(', ')', ';')
    $clean -in @("bash", "sh", "awk", "perl", "python", "python3", "shellcheck") -or
        $clean -in @("pwsh", "powershell", "node", "source", ".") -or
        $clean.EndsWith("/bash") -or
        $clean.EndsWith("/sh")
}

function Test-ShellCommandStringOption {
    param([string]$Token)

    $clean = $Token.Trim('"', "'", '`', '(', ')', ';')
    $clean.StartsWith("-") -and -not $clean.StartsWith("--") -and $clean.Contains("c")
}

function Test-ShellAssignmentValueConsumesRest {
    param([string]$Value)

    $valueText = $Value.TrimStart()
    if ($valueText.Length -eq 0) {
        return $true
    }

    $firstChar = $valueText[0]
    if ($firstChar -ne '"' -and $firstChar -ne "'") {
        return -not ($valueText -match "\s")
    }

    $escaped = $false
    for ($index = 1; $index -lt $valueText.Length; $index++) {
        $char = $valueText[$index]
        if ($firstChar -eq '"' -and $char -eq "\" -and -not $escaped) {
            $escaped = $true
            continue
        }

        if ($char -eq $firstChar -and -not $escaped) {
            return $valueText.Substring($index + 1).Trim().Length -eq 0
        }

        $escaped = $false
    }

    $false
}

function Test-ShellAssignmentOnly {
    param([string]$CommandText)

    $trimmed = $CommandText.Trim()
    $match = [regex]::Match($trimmed, "^[A-Za-z_][A-Za-z0-9_]*(\[[^\]]+\])?=")
    if (-not $match.Success) {
        return $false
    }

    Test-ShellAssignmentValueConsumesRest -Value $trimmed.Substring($match.Length)
}

function Remove-UnquotedShellComment {
    param([string]$Text)

    $inSingle = $false
    $inDouble = $false
    $escaped = $false
    for ($index = 0; $index -lt $Text.Length; $index++) {
        $char = $Text[$index]
        if ($escaped) {
            $escaped = $false
            continue
        }
        if ($char -eq "\" -and -not $inSingle) {
            $escaped = $true
            continue
        }
        if ($char -eq "'" -and -not $inDouble) {
            $inSingle = -not $inSingle
            continue
        }
        if ($char -eq '"' -and -not $inSingle) {
            $inDouble = -not $inDouble
            continue
        }
        if ($char -eq "#" -and -not $inSingle -and -not $inDouble) {
            if ($index -eq 0 -or [char]::IsWhiteSpace($Text[$index - 1])) {
                return $Text.Substring(0, $index).TrimEnd()
            }
        }
    }

    $Text
}

function Normalize-CommandText {
    param([string]$Text)

    $Text.Replace('${{ github.workspace }}', '$GITHUB_WORKSPACE').
        Replace('${{github.workspace}}', '$GITHUB_WORKSPACE')
}

function Test-CommandTextForDirectScript {
    param(
        [Parameter(Mandatory = $true)][string]$CommandText,
        [Parameter(Mandatory = $true)][string]$File,
        [Parameter(Mandatory = $true)][int]$LineNumber,
        [Parameter(Mandatory = $true)]
        [AllowEmptyCollection()]
        [System.Collections.Generic.List[string]]$Violations
    )

    # Most lines in multiline run blocks cannot reference a repository script.
    # Avoid the quote-aware shell tokenization path unless its only supported
    # path prefixes are present.
    if (-not $CommandText.Contains("scripts/")) {
        return
    }

    $trimmed = Normalize-CommandText -Text ((Remove-UnquotedShellComment -Text $CommandText).Trim())
    if ([string]::IsNullOrWhiteSpace($trimmed) -or $trimmed.StartsWith("#")) {
        return
    }
    if (Test-ShellAssignmentOnly -CommandText $trimmed) {
        return
    }

    $tokenText = $trimmed -replace "&&|\|\||[;&|()]", " "
    $tokens = @($tokenText -split "\s+" | Where-Object { $_ -ne "" })
    for ($tokenIndex = 0; $tokenIndex -lt $tokens.Count; $tokenIndex++) {
        $scriptPath = Normalize-LocalScriptToken -Token $tokens[$tokenIndex]
        if ($null -eq $scriptPath) {
            continue
        }

        $interpreted = $false
        $start = [Math]::Max(0, $tokenIndex - 8)
        for ($previousIndex = $tokenIndex - 1; $previousIndex -ge $start; $previousIndex--) {
            if (Test-InterpreterToken -Token $tokens[$previousIndex]) {
                $usesCommandString = $false
                for ($middleIndex = $previousIndex + 1; $middleIndex -lt $tokenIndex; $middleIndex++) {
                    if (Test-ShellCommandStringOption -Token $tokens[$middleIndex]) {
                        $usesCommandString = $true
                        break
                    }
                }
                $interpreted = -not $usesCommandString
                break
            }
        }
        if ($interpreted) {
            continue
        }

        [void]$Violations.Add("${File}:${LineNumber}: direct script invocation must use an interpreter: $scriptPath")
    }
}

function Test-WorkflowContentForDirectScripts {
    param(
        [Parameter(Mandatory = $true)][string]$File,
        [Parameter(Mandatory = $true)][string]$Content,
        [Parameter(Mandatory = $true)]
        [AllowEmptyCollection()]
        [System.Collections.Generic.List[string]]$Violations
    )

    $inRunBlock = $false
    $runBlockIndent = -1
    $lineNumber = 0
    foreach ($line in $Content -split "`r?`n") {
        $lineNumber++
        $trimmed = $line.Trim()
        $indent = Get-Indent -Line $line

        if ($inRunBlock) {
            if ([string]::IsNullOrWhiteSpace($trimmed)) {
                continue
            }
            if ($indent -gt $runBlockIndent) {
                Test-CommandTextForDirectScript -CommandText $trimmed -File $File -LineNumber $lineNumber -Violations $Violations
                continue
            }
            $inRunBlock = $false
        }

        if ($trimmed.StartsWith("#")) {
            continue
        }

        if ($line -match "^\s*(?:-\s*)?run:\s*([|>])[-+]?\s*$") {
            $inRunBlock = $true
            $runBlockIndent = $indent
            continue
        }

        if ($line -match "^\s*(?:-\s*)?run:\s*(.+)$") {
            Test-CommandTextForDirectScript -CommandText $Matches[1] -File $File -LineNumber $lineNumber -Violations $Violations
        }
    }
}

function Test-WorkflowDirectScriptInvocations {
    param([System.Collections.Generic.Dictionary[string, System.Collections.Generic.HashSet[string]]]$ChangedFiles)

    $workflowFiles = @($ChangedFiles.Keys | Where-Object { $_ -match "^\.github/workflows/.*\.ya?ml$" })
    if ($workflowFiles.Count -eq 0) {
        Skip "Workflow script invocation policy" "no workflow files pushed"
        return
    }

    $violations = [System.Collections.Generic.List[string]]::new()
    foreach ($file in $workflowFiles) {
        foreach ($commit in $ChangedFiles[$file]) {
            $content = Get-CommitBlobText -Commit $commit -File $file
            if ($null -eq $content) {
                continue
            }
            Test-WorkflowContentForDirectScripts -File $file -Content $content -Violations $violations
        }
    }

    if ($violations.Count -gt 0) {
        Fail "Workflow script invocation policy" "Invoke local scripts through an interpreter so executable-bit drift cannot break CI.`n$($violations -join "`n")"
    } else {
        Pass "Workflow script invocation policy"
    }
}

if ($SourceOnly) {
    return
}

$timer = [System.Diagnostics.Stopwatch]::StartNew()
$script:RepoRoot = (Invoke-Git -Arguments @("rev-parse", "--show-toplevel")).Stdout.Trim()
Set-Location $script:RepoRoot

if ($Worktree) {
    Write-Host "[pre-push] Running fast worktree push-policy preflight checks..."
    $changedFiles = Get-ChangedFilesForWorktreePreflight
} else {
    Write-Host "[pre-push] Running fast push checks..."
    $changedFiles = Get-ChangedFilesForPush
}

if ($changedFiles.Count -eq 0) {
    Skip "Changed file discovery" "no pushed file changes detected"
} else {
    Pass "Changed file discovery"
}

Initialize-CommitBlobTextCache -ChangedFiles $changedFiles
Test-FastHookSource -ChangedFiles $changedFiles
Test-WorkflowDirectScriptInvocations -ChangedFiles $changedFiles

$timer.Stop()
Write-Host "[pre-push] Completed in $($timer.ElapsedMilliseconds)ms"
if ($timer.ElapsedMilliseconds -gt $script:HookBudgetMs) {
    Write-Host "[pre-push] WARN: runtime $($timer.ElapsedMilliseconds)ms exceeded ${script:HookBudgetMs}ms target."
    if ($EnforceBudget) {
        Fail "Hook runtime budget" "Runtime $($timer.ElapsedMilliseconds)ms exceeded ${script:HookBudgetMs}ms target."
    }
}

if ($script:Failed -gt 0) {
    Write-Host "[pre-push] $script:Failed failed, $script:Passed passed, $script:Skipped skipped."
    Write-Host "[pre-push] Slow semantic checks stay outside git hooks. Run:"
    Write-Host "  cargo fmt --check"
    Write-Host "  cargo clippy --locked --all-targets --all-features -- -D warnings"
    Write-Host "  cargo test --locked --all-features"
    Write-Host "  ./scripts/run-local-ci.sh"
    exit 1
}

Write-Host "[pre-push] All checks passed ($script:Passed passed, $script:Skipped skipped)."
exit 0
