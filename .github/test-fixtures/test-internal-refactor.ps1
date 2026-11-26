#requires -Version 7.0
param([Parameter(Mandatory = $true)][string]$RepositoryRoot)
Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$RepositoryRoot = (Resolve-Path -LiteralPath $RepositoryRoot).Path
. (Join-Path $RepositoryRoot "scripts/hooks/pre-commit.ps1") -SourceOnly

function Git {
    param([Parameter(ValueFromRemainingArguments = $true)][string[]]$GitArguments)
    $result = Invoke-Native -FileName "git" -Arguments $GitArguments
    if ($result.ExitCode -ne 0) { throw $result.Output }
    $result.Stdout.Trim()
}
function Save-Declaration {
    param([object]$Value)
    [System.IO.File]::WriteAllText(".github/internal-refactor.json", ($Value | ConvertTo-Json -Depth 5))
}
function Expect-Rejection {
    param([scriptblock]$Action, [string]$Name, [string]$ExpectedMessage = "")
    $rejected = $false
    try { & $Action | Out-Null } catch {
        if ($ExpectedMessage -and -not $_.Exception.Message.Contains($ExpectedMessage)) { throw }
        $rejected = $true
    }
    if (-not $rejected) { throw "Expected rejection: $Name" }
}
function Expect-Paths {
    param([string]$Mode, [string]$DiffBase = "")
    $paths = Get-InternalRefactorExemptions -Mode $Mode -DiffBase $DiffBase
    if (($paths | Sort-Object) -join "," -cne ".cargo/mutants.toml,src/.hidden space.rs,src/core.rs,src/deleted.rs,src/new.rs") {
        throw "Wrong exemption paths in $Mode"
    }
}

$originalLocation = Get-Location
$originalDirectory = [System.Environment]::CurrentDirectory
$temporary = Join-Path ([System.IO.Path]::GetTempPath()) ("internal-refactor-" + [guid]::NewGuid().ToString("N"))
try {
    New-Item -ItemType Directory -Path $temporary | Out-Null
    Set-Location -LiteralPath $temporary
    [System.Environment]::CurrentDirectory = $temporary
    Git init -q | Out-Null
    Git config commit.gpgSign false | Out-Null
    Git config tag.gpgSign false | Out-Null
    Git config core.autocrlf false | Out-Null
    $emptyHooks = Join-Path $temporary ".git/empty-hooks"
    New-Item -ItemType Directory -Path $emptyHooks | Out-Null
    Git config core.hooksPath $emptyHooks | Out-Null
    Git config user.name "Fixture" | Out-Null
    Git config user.email "fixture@example.invalid" | Out-Null
    New-Item -ItemType Directory -Path src, .github, .cargo | Out-Null
    $beforeProbe = "fn ready() -> bool {`n    true`n}`n"
    $afterProbe = @'
fn ready() -> bool {
    #[cfg(feature = "allocation-tracking")]
    crate::allocation_benchmark::record_stage("ready");
    true
}
'@ + "`n"
    [System.IO.File]::WriteAllText("src/core.rs", $beforeProbe)
    [System.IO.File]::WriteAllText("src/.hidden space.rs", "old hidden`n")
    [System.IO.File]::WriteAllText("src/deleted.rs", "deleted core`n")
    [System.IO.File]::WriteAllText(".cargo/mutants.toml", "timeout_multiplier = 2.0`n")
    Git add . | Out-Null
    Git commit -qm seed | Out-Null
    $base = Git rev-parse HEAD
    $before = Git rev-parse HEAD:src/core.rs
    $deleted = Git rev-parse HEAD:src/deleted.rs
    $hidden = Git rev-parse "HEAD:src/.hidden space.rs"
    [System.IO.File]::WriteAllText("src/.hidden space.rs", "new hidden`n")
    [System.IO.File]::WriteAllText("src/core.rs", $afterProbe)
    [System.IO.File]::WriteAllText(".cargo/mutants.toml", "timeout_multiplier = 3.0`n")
    Git add src/core.rs | Out-Null
    [System.IO.File]::WriteAllText("src/new.rs", "new file`n")
    Remove-Item src/deleted.rs
    $undeclaredPaths = Get-InternalRefactorExemptions -Mode Worktree
    if ($undeclaredPaths -isnot [string[]] -or $undeclaredPaths.Length -ne 0) {
        throw "Expected an empty path list without a declaration"
    }
    $failedBefore = $script:Failed
    $guidance = Test-ChangelogGate -ChangedFiles @("src/core.rs") 6>&1 | Out-String
    if ($script:Failed -ne $failedBefore + 1 -or $guidance -cnotmatch '\.github/internal-refactor\.json') {
        throw "Undeclared allocation probe must fail with exact-blob guidance"
    }
    $declaration = @{
        base = $base
        reason = "Add a dev-only allocation probe and refresh private fixtures."
        files = @(
            @{path = "src/core.rs"; before = $before; after = (Git hash-object src/core.rs)},
            @{path = "src/new.rs"; before = $null; after = (Git hash-object src/new.rs)},
            @{path = "src/deleted.rs"; before = $deleted; after = $null},
            @{path = "src/.hidden space.rs"; before = $hidden; after = (Git hash-object "src/.hidden space.rs")},
            @{path = ".cargo/mutants.toml"; before = (Git rev-parse HEAD:.cargo/mutants.toml); after = (Git hash-object .cargo/mutants.toml)}
        )
    }
    Save-Declaration $declaration
    Expect-Paths Worktree
    Git add -A | Out-Null
    Expect-Paths Staged
    Expect-Paths Comparison $base
    [System.IO.File]::WriteAllText(".github/internal-refactor.json", "{")
    Expect-Paths Staged
    Expect-Rejection { Get-InternalRefactorExemptions -Mode Worktree } "unstaged declaration drift"
    Save-Declaration $declaration
    # A declaration can exempt a changelog entry, while the staged panic policy still applies.
    $panickingProbe = $afterProbe.Replace('    true', '    panic!("staged refactor probe")')
    [System.IO.File]::WriteAllText("src/core.rs", $panickingProbe)
    Git add src/core.rs | Out-Null
    $declaration.files[0].after = Git rev-parse :src/core.rs
    Save-Declaration $declaration
    Git add .github/internal-refactor.json | Out-Null
    [System.IO.File]::WriteAllText("src/core.rs", $afterProbe)
    $hook = Join-Path $RepositoryRoot "scripts/hooks/pre-commit.ps1"
    $rejection = & pwsh -NoLogo -NoProfile -NonInteractive -File $hook
    if ($LASTEXITCODE -eq 0 -or $rejection -cnotcontains "PASS: Changelog gate" -or
        $rejection -cnotcontains "FAIL: Rust panic patterns") {
        throw "An exact declaration hid a staged panic: $($rejection -join "`n")"
    }
    $declaration.files[0].after = Git hash-object src/core.rs
    Save-Declaration $declaration
    Git add src/core.rs .github/internal-refactor.json | Out-Null

    foreach ($worktreeMode in @($false, $true)) {
        $script:InspectWorktree = $worktreeMode
        $failedBefore = $script:Failed
        $declaredChanges = @("src/core.rs", ".cargo/mutants.toml", ".github/internal-refactor.json")
        Test-ChangelogGate -ChangedFiles $declaredChanges
        if ($script:Failed -ne $failedBefore) { throw "Hook rejected declared probe or mutation configuration" }
        $rejection = Test-ChangelogGate -ChangedFiles ($declaredChanges + ".cargo/config.toml") 6>&1 | Out-String
        if ($script:Failed -ne $failedBefore + 1 -or $rejection -cnotmatch '\.cargo/config\.toml') {
            throw "Mutation configuration declaration covered Cargo build configuration"
        }
        $failedBefore = $script:Failed

        [System.IO.File]::WriteAllText("src/core.rs", $afterProbe.Replace('    true', '    false'))
        if (-not $worktreeMode) { Git add src/core.rs | Out-Null }
        $rejection = Test-ChangelogGate -ChangedFiles $declaredChanges 6>&1 | Out-String
        if ($script:Failed -ne $failedBefore + 1 -or $rejection -cnotmatch 'after blob mismatch') {
            throw "Allocation probe declaration covered a changed return value"
        }
        [System.IO.File]::WriteAllText("src/core.rs", $afterProbe)
        Git add src/core.rs | Out-Null
    }
    $script:InspectWorktree = $false
    Expect-Rejection {
        Get-InternalRefactorExemptions -Mode Comparison -DiffBase $base -ChangeAlreadyObserved
    } "comparison discovery bypass"

    [System.IO.File]::WriteAllText("src/core.rs", "unstaged drift`n")
    Expect-Paths Staged
    Expect-Rejection { Get-InternalRefactorExemptions -Mode Worktree } "unstaged source drift"
    [System.IO.File]::WriteAllText("src/core.rs", $afterProbe)

    Remove-Item -LiteralPath "src/.hidden space.rs" -Force
    foreach ($mode in @("Worktree", "Comparison")) {
        Expect-Rejection { Get-InternalRefactorExemptions -Mode $mode -DiffBase $base } "missing hidden source in $mode" "after blob mismatch"
    }
    Expect-Paths Staged
    if (-not $IsWindows) {
        New-Item -ItemType SymbolicLink -Path "src/.hidden space.rs" -Target core.rs | Out-Null
        foreach ($mode in @("Worktree", "Comparison")) {
            Expect-Rejection { Get-InternalRefactorExemptions -Mode $mode -DiffBase $base } "hidden source symlink in $mode" "not a regular file"
        }
        Remove-Item -LiteralPath "src/.hidden space.rs" -Force
    }
    [System.IO.File]::WriteAllText("src/.hidden space.rs", "wrong hidden`n")
    foreach ($mode in @("Worktree", "Comparison")) {
        Expect-Rejection { Get-InternalRefactorExemptions -Mode $mode -DiffBase $base } "hidden source drift in $mode" "after blob mismatch"
    }
    [System.IO.File]::WriteAllText("src/.hidden space.rs", "new hidden`n")
    Expect-Paths Worktree
    Expect-Paths Comparison $base

    $originalJson = $declaration | ConvertTo-Json -Depth 5
    foreach ($mutation in @("before", "after", "reason", "traversal", "wildcard", "newline", "duplicate", "base", "extra")) {
        $invalid = ConvertFrom-Json -AsHashtable $originalJson
        switch ($mutation) {
            "before" { $invalid.files[0].before = "0" * 40 }
            "after" { $invalid.files[0].after = "0" * 40 }
            "reason" { $invalid.reason = " " }
            "traversal" { $invalid.files[0].path = "src/../core.rs" }
            "wildcard" { $invalid.files[0].path = "src/*.rs" }
            "newline" { $invalid.files[0].path = "src/core`n.rs" }
            "duplicate" { $invalid.files += @{path = "SRC/core.rs"; before = $before; after = $declaration.files[0].after} }
            "base" { $invalid.base = "0" * 40 }
            "extra" { $invalid["unexpected"] = $true }
        }
        Save-Declaration $invalid
        Expect-Rejection { Get-InternalRefactorExemptions -Mode Worktree } $mutation
    }
    [System.IO.File]::WriteAllText(".github/internal-refactor.json", "{")
    Expect-Rejection { Get-InternalRefactorExemptions -Mode Worktree } "malformed JSON"
    Save-Declaration $declaration

    $declarationBlob = Git -GitArguments @("hash-object", "-w", ".github/internal-refactor.json")
    Git update-index --cacheinfo 120000 $declarationBlob .github/internal-refactor.json | Out-Null
    Expect-Rejection { Get-InternalRefactorExemptions -Mode Staged } "staged declaration symlink"
    Git add .github/internal-refactor.json | Out-Null
    if (-not $IsWindows) {
        Copy-Item .github/internal-refactor.json .github/declared.json
        Remove-Item .github/internal-refactor.json
        New-Item -ItemType SymbolicLink -Path .github/internal-refactor.json -Target declared.json | Out-Null
        Expect-Rejection { Get-InternalRefactorExemptions -Mode Worktree } "worktree declaration symlink"
        Remove-Item .github/internal-refactor.json
        Save-Declaration $declaration
    }

    $script:InspectWorktree = $false
    $failedBefore = $script:Failed
    Test-ChangelogGate -ChangedFiles @("src/core.rs", "src/new.rs", ".github/internal-refactor.json")
    if ($script:Failed -ne $failedBefore) { throw "Hook rejected declared changes" }
    [System.IO.File]::WriteAllText("src/unlisted.rs", "unlisted`n")
    Git add src/unlisted.rs | Out-Null
    Test-ChangelogGate -ChangedFiles @("src/core.rs", "src/unlisted.rs", ".github/internal-refactor.json")
    if ($script:Failed -ne $failedBefore + 1) { throw "Hook accepted undeclared production change" }

    Git commit -qm refactor | Out-Null
    Expect-Paths Comparison "$base...HEAD"
    [System.IO.File]::WriteAllText("src/core.rs", "dirty comparison`n")
    Expect-Rejection { Get-InternalRefactorExemptions -Mode Comparison -DiffBase "$base...HEAD" } "dirty comparison source"
    $substitution = ConvertFrom-Json -AsHashtable $originalJson
    $substitution.files[0].after = Git hash-object src/core.rs
    Save-Declaration $substitution
    Expect-Rejection { Get-InternalRefactorExemptions -Mode Comparison -DiffBase "$base...HEAD" } "dirty declaration substitution"
    Save-Declaration $declaration
    [System.IO.File]::WriteAllText("src/core.rs", $afterProbe)
    $refactorBranch = Git symbolic-ref --short HEAD
    Git checkout -qb unrelated-base $base | Out-Null
    [System.IO.File]::WriteAllText("unrelated.txt", "unrelated base advance`n")
    Git add unrelated.txt | Out-Null
    Git commit -qm unrelated | Out-Null
    $advancedBase = Git rev-parse HEAD
    Git checkout $refactorBranch | Out-Null
    Git merge --no-edit unrelated-base | Out-Null
    Expect-Paths Comparison "$advancedBase...HEAD"

    Git checkout -qb changed-path-base $base | Out-Null
    [System.IO.File]::WriteAllText("src/core.rs", $afterProbe)
    Git add src/core.rs | Out-Null
    Git commit -qm changed-path | Out-Null
    $changedPathBase = Git rev-parse HEAD
    Git checkout $refactorBranch | Out-Null
    Git merge --no-edit changed-path-base | Out-Null
    Expect-Rejection { Get-InternalRefactorExemptions -Mode Comparison -DiffBase "$changedPathBase...HEAD" } "declared path changed at comparison base" "Refactor path changed at the comparison base"

    [System.IO.File]::WriteAllText("src/core.rs", "later change`n")
    Git add src/core.rs | Out-Null
    if ((Get-InternalRefactorExemptions -Mode Staged).Count -ne 0) { throw "Historical declaration exempted a later change" }
    if ((Get-InternalRefactorExemptions -Mode Worktree).Count -ne 0) { throw "Historical declaration exempted worktree drift" }
    Write-Output "PASS internal-refactor staged, worktree, comparison, and negative controls"
} finally {
    Set-Location -LiteralPath $originalLocation.Path
    [System.Environment]::CurrentDirectory = $originalDirectory
    if (Test-Path -LiteralPath $temporary) { Remove-Item -LiteralPath $temporary -Recurse -Force }
}
