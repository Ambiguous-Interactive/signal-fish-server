#requires -Version 7.0
param(
    [ValidateRange(1, 20)][int]$Repetitions = 5,
    [switch]$Fetch
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$repository = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot "../..")).Path
. (Join-Path $repository "scripts/hooks/native-process.ps1")

# Use the original #1486 workload and the hook source before its repair.
$refactor = "0141f270bb5d19b2c2b09e6e7b45679cc4f641a2"
$baseline = "753a665975a748a3a77d41be78c0c278d8d24c14"
if ($Fetch) {
    $shallow = (Invoke-Git @("-C", $repository, "rev-parse", "--is-shallow-repository")).Stdout.Trim() -eq "true"
    if ($shallow) {
        [void](Invoke-Git @("-C", $repository, "fetch", "--no-tags", "--depth=2", "origin", $refactor))
        [void](Invoke-Git @("-C", $repository, "fetch", "--no-tags", "--depth=1", "origin", $baseline))
    } else {
        [void](Invoke-Git @("-C", $repository, "fetch", "--no-tags", "origin", $refactor, $baseline))
    }
}
$candidate = (Invoke-Git @("-C", $repository, "rev-parse", "HEAD")).Stdout.Trim()
Write-Output "refactor=$refactor baseline=$baseline candidate=$candidate"

$temporary = Join-Path ([System.IO.Path]::GetTempPath()) ("exact-refactor-" + [guid]::NewGuid().ToString("N"))
[void](New-Item -ItemType Directory -Path $temporary)
$baselinePath = Join-Path $temporary "baseline"
$replay = Join-Path $temporary "replay"
$worktrees = [System.Collections.Generic.List[string]]::new()
try {
    [void](Invoke-Git @("-C", $repository, "worktree", "add", "--detach", $baselinePath, $baseline))
    $worktrees.Add($baselinePath)
    [void](Invoke-Git @("-C", $repository, "worktree", "add", "--detach", $replay, "$refactor^"))
    $worktrees.Add($replay)
    [void](Invoke-Git @("-C", $replay, "read-tree", "--reset", "-u", $refactor))
    $targetTree = (Invoke-Git @("-C", $replay, "rev-parse", "$refactor^{tree}")).Stdout.Trim()
    $stagedTree = (Invoke-Git @("-C", $replay, "write-tree")).Stdout.Trim()
    if ($stagedTree -cne $targetTree) { throw "Refactor index differs from the original commit" }

    $previousProfile = $env:SIGNAL_FISH_HOOK_PROFILE
    $env:SIGNAL_FISH_HOOK_PROFILE = "1"
    Push-Location -LiteralPath $replay
    try {
        foreach ($mode in @("staged", "worktree")) {
            $modeArguments = @()
            if ($mode -eq "worktree") { $modeArguments = @("-Worktree") }
            for ($run = 1; $run -le $Repetitions; $run++) {
                $order = if ($run % 2 -eq 0) { @("candidate", "baseline") } else { @("baseline", "candidate") }
                foreach ($version in $order) {
                    $root = if ($version -eq "baseline") { $baselinePath } else { $repository }
                    $hook = Join-Path $root "scripts/hooks/pre-commit.ps1"
                    Write-Output "=== exact refactor $mode run $run $version ==="
                    $wall = [System.Diagnostics.Stopwatch]::StartNew()
                    $output = & pwsh -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File $hook @modeArguments
                    $exitCode = $LASTEXITCODE
                    $wall.Stop()
                    $output | Write-Output
                    Write-Output "Process wall $($wall.ElapsedMilliseconds)ms"
                    if ($exitCode -ne 0 -or $output -cnotcontains "PASS: Changelog gate" -or
                        -not ($output -match "Completed in \d+ms")) {
                        throw "Refactor hook qualification failed for $mode run $run $version"
                    }
                }
            }
        }
    } finally {
        Pop-Location
        $env:SIGNAL_FISH_HOOK_PROFILE = $previousProfile
    }
} finally {
    foreach ($worktree in $worktrees) {
        [void](Invoke-Git @("-C", $repository, "worktree", "remove", "--force", $worktree))
    }
    Remove-Item -LiteralPath $temporary -Force
}
