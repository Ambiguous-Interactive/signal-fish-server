# Read-only Windows host measurements. Run outside the devcontainer.
[CmdletBinding()]
param(
    [string]$DockerWslPath = ''
)

$ErrorActionPreference = 'Stop'
if (-not $IsWindows) {
    throw 'Run this collector in PowerShell 7 on the Windows host.'
}
if (-not $DockerWslPath) {
    $DockerWslPath = Join-Path $env:LOCALAPPDATA 'Docker\wsl'
}

# File length and allocated disk bytes differ for sparse VHD files.
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class SignalFishAllocatedFileSize {
    [DllImport("kernel32.dll", EntryPoint="GetCompressedFileSizeW", CharSet=CharSet.Unicode, SetLastError=true)]
    public static extern uint GetSize(string name, out uint high);
}
'@

$disks = @(Get-PSDrive -PSProvider FileSystem | ForEach-Object {
    [ordered]@{ Name = $_.Name; FreeBytes = $_.Free; UsedBytes = $_.Used }
})
$files = @()
if (Test-Path -LiteralPath $DockerWslPath) {
    $files = @(Get-ChildItem -LiteralPath $DockerWslPath -Filter '*.vhdx' -Recurse -File | ForEach-Object {
        [uint32]$high = 0
        $low = [SignalFishAllocatedFileSize]::GetSize($_.FullName, [ref]$high)
        $nativeError = [Runtime.InteropServices.Marshal]::GetLastWin32Error()
        if ($low -eq [uint32]::MaxValue -and $nativeError -ne 0) {
            throw "Cannot measure allocated bytes for $($_.FullName): Win32 error $nativeError"
        }
        [ordered]@{
            Path = $_.FullName
            FileLengthBytes = $_.Length
            AllocatedBytes = ([uint64]$high * 4294967296 + [uint64]$low)
        }
    })
}
[ordered]@{
    CollectedAt = [DateTime]::UtcNow.ToString('o')
    Drives = $disks
    DockerWslPath = $DockerWslPath
    VhdFiles = $files
    WslVersion = @(& wsl.exe --version 2>&1 | ForEach-Object { "$_" })
    WslDistributions = @(& wsl.exe --list --verbose 2>&1 | ForEach-Object { "$_" })
    Notes = @('Use -DockerWslPath if Docker Desktop stores its disk elsewhere.',
              'An empty VhdFiles array means the Docker disk was not found; do not infer zero usage.')
} | ConvertTo-Json -Depth 5
