# Read-only Windows host measurements. Run outside the devcontainer.
[CmdletBinding()]
param(
    [string]$DockerWslPath = ''
)

function ConvertFrom-WslBytes {
    param([byte[]]$Bytes)
    if ($Bytes.Length -eq 0) { return '' }
    if ($Bytes.Length -ge 2 -and $Bytes[0] -eq 0xfe -and $Bytes[1] -eq 0xff) {
        $encoding = [Text.Encoding]::BigEndianUnicode
    } elseif (($Bytes.Length -ge 2 -and $Bytes[0] -eq 0xff -and $Bytes[1] -eq 0xfe) -or
              [Array]::IndexOf($Bytes, [byte]0) -ge 0) {
        # wsl --list can emit UTF-16LE without a BOM. Newer commands use UTF-8.
        $encoding = [Text.Encoding]::Unicode
    } else {
        $encoding = [Text.Encoding]::UTF8
    }
    return $encoding.GetString($Bytes).TrimStart([char]0xfeff)
}

function Invoke-WslText {
    param([string[]]$ArgumentList, [string]$Executable = 'wsl.exe')
    $process = [Diagnostics.Process]::new()
    $process.StartInfo.FileName = $Executable
    $process.StartInfo.UseShellExecute = $false
    $process.StartInfo.RedirectStandardOutput = $true
    $process.StartInfo.RedirectStandardError = $true
    foreach ($argument in $ArgumentList) { $process.StartInfo.ArgumentList.Add($argument) }
    $stdout = [IO.MemoryStream]::new()
    $stderr = [IO.MemoryStream]::new()
    try {
        [void]$process.Start()
        $outTask = $process.StandardOutput.BaseStream.CopyToAsync($stdout)
        $errTask = $process.StandardError.BaseStream.CopyToAsync($stderr)
        if (-not $process.WaitForExit(15000)) {
            $process.Kill($true)
            throw 'WSL query exceeded 15 seconds.'
        }
        [Threading.Tasks.Task]::WhenAll([Threading.Tasks.Task[]]@($outTask, $errTask)).GetAwaiter().GetResult()
        $outputText = ConvertFrom-WslBytes -Bytes $stdout.ToArray()
        $errorText = ConvertFrom-WslBytes -Bytes $stderr.ToArray()
        ($outputText + "`n" + $errorText) -split '\r?\n' | Where-Object { $_.Length -gt 0 }
    } finally {
        $process.Dispose()
        $stdout.Dispose()
        $stderr.Dispose()
    }
}

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
    WslVersion = @(Invoke-WslText -ArgumentList @('--version'))
    WslDistributions = @(Invoke-WslText -ArgumentList @('--list', '--verbose'))
    Notes = @('Use -DockerWslPath if Docker Desktop stores its disk elsewhere.',
              'An empty VhdFiles array means the Docker disk was not found; do not infer zero usage.')
} | ConvertTo-Json -Depth 5
