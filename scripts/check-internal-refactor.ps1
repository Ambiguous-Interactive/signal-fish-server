#requires -Version 7.0
param(
    [ValidateSet("Staged", "Worktree", "Comparison")][string]$Mode = "Staged",
    [string]$DiffBase = ""
)
Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
. (Join-Path $PSScriptRoot "hooks/native-process.ps1")
. (Join-Path $PSScriptRoot "hooks/internal-refactor.ps1")
try {
    Get-InternalRefactorExemptions -Mode $Mode -DiffBase $DiffBase | ForEach-Object { $_ }
} catch {
    [Console]::Error.WriteLine($_.Exception.Message)
    exit 1
}
