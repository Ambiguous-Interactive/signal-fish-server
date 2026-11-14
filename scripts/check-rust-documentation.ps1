#requires -Version 7.0
Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
. (Join-Path $PSScriptRoot "hooks/rust-documentation.ps1")
$contents = [Console]::In.ReadToEnd().Split([char]0)
if ($contents.Length -eq 2 -and (Test-RustDocumentationOnlyChange -OldContent $contents[0] -NewContent $contents[1])) {
    exit 0
}
exit 1
