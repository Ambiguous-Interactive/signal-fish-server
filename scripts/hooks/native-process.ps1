#requires -Version 7.0
Set-StrictMode -Version Latest

function New-NativeProcessStartInfo {
    param(
        [Parameter(Mandatory = $true)][string]$FileName,
        [Parameter(Mandatory = $true)][string[]]$Arguments,
        [switch]$RedirectStandardInput
    )

    $psi = [System.Diagnostics.ProcessStartInfo]::new()
    $psi.FileName = $FileName
    foreach ($argument in $Arguments) {
        [void]$psi.ArgumentList.Add($argument)
    }
    $psi.RedirectStandardInput = $RedirectStandardInput
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    $psi.UseShellExecute = $false
    $utf8NoBom = [System.Text.UTF8Encoding]::new($false)
    $psi.StandardOutputEncoding = $utf8NoBom
    $psi.StandardErrorEncoding = $utf8NoBom
    $psi
}

function Invoke-Native {
    param(
        [Parameter(Mandatory = $true)][string]$FileName,
        [Parameter(Mandatory = $true)][string[]]$Arguments
    )

    $psi = New-NativeProcessStartInfo -FileName $FileName -Arguments $Arguments
    $process = [System.Diagnostics.Process]::Start($psi)
    try {
        $stdoutTask = $process.StandardOutput.ReadToEndAsync()
        $stderrTask = $process.StandardError.ReadToEndAsync()
        $process.WaitForExit()
        $stdout = $stdoutTask.GetAwaiter().GetResult()
        $stderr = $stderrTask.GetAwaiter().GetResult()

        [pscustomobject]@{
            ExitCode = $process.ExitCode
            Stdout = $stdout
            Stderr = $stderr
            Output = $stdout + $stderr
        }
    } finally {
        $process.Dispose()
    }
}

function Invoke-NativeWithInput {
    param(
        [Parameter(Mandatory = $true)][string]$FileName,
        [Parameter(Mandatory = $true)][string[]]$Arguments,
        [Parameter(Mandatory = $true)][string]$InputText
    )

    $psi = New-NativeProcessStartInfo -FileName $FileName -Arguments $Arguments -RedirectStandardInput
    $process = [System.Diagnostics.Process]::Start($psi)
    try {
        $stdoutTask = $process.StandardOutput.ReadToEndAsync()
        $stderrTask = $process.StandardError.ReadToEndAsync()
        $process.StandardInput.Write($InputText)
        $process.StandardInput.Close()
        $process.WaitForExit()
        $stdout = $stdoutTask.GetAwaiter().GetResult()
        $stderr = $stderrTask.GetAwaiter().GetResult()

        [pscustomobject]@{
            ExitCode = $process.ExitCode
            Stdout = $stdout
            Stderr = $stderr
            Output = $stdout + $stderr
        }
    } finally {
        $process.Dispose()
    }
}

function Invoke-NativeBytesWithInput {
    param(
        [Parameter(Mandatory = $true)][string]$FileName,
        [Parameter(Mandatory = $true)][string[]]$Arguments,
        [Parameter(Mandatory = $true)][string]$InputText
    )

    $psi = New-NativeProcessStartInfo -FileName $FileName -Arguments $Arguments -RedirectStandardInput
    $process = [System.Diagnostics.Process]::Start($psi)
    $stdoutStream = [System.IO.MemoryStream]::new()
    try {
        $stdoutTask = $process.StandardOutput.BaseStream.CopyToAsync($stdoutStream)
        $stderrTask = $process.StandardError.ReadToEndAsync()
        $process.StandardInput.Write($InputText)
        $process.StandardInput.Close()
        $process.WaitForExit()
        [void]$stdoutTask.GetAwaiter().GetResult()
        $stderr = $stderrTask.GetAwaiter().GetResult()

        [pscustomobject]@{
            ExitCode = $process.ExitCode
            StdoutBytes = $stdoutStream.ToArray()
            Stderr = $stderr
            Output = $stderr
        }
    } finally {
        $stdoutStream.Dispose()
        $process.Dispose()
    }
}

function Invoke-Git {
    param([Parameter(Mandatory = $true)][string[]]$Arguments)
    $result = Invoke-Native -FileName "git" -Arguments $Arguments
    if ($result.ExitCode -ne 0) {
        throw "git $($Arguments -join ' ') failed:`n$($result.Output)"
    }
    $result
}
