# Exercise WSL byte decoding without requiring a Windows host or WSL install.
$ErrorActionPreference = 'Stop'
$scriptPath = Join-Path $PSScriptRoot 'dev-storage-host.ps1'
$parseErrors = $null
$parseTokens = $null
$ast = [Management.Automation.Language.Parser]::ParseFile($scriptPath, [ref]$parseTokens, [ref]$parseErrors)
if (@($parseErrors).Count) { throw ($parseErrors | Out-String) }
$functions = @($ast.FindAll({ param($node) $node -is [Management.Automation.Language.FunctionDefinitionAst] }, $false))
foreach ($function in $functions) {
    . ([scriptblock]::Create($function.Extent.Text))
}

$expected = "Ubuntu-魚🐟`r`nDocker Desktop`r`n"
foreach ($encoding in @([Text.Encoding]::Unicode, [Text.Encoding]::UTF8, [Text.Encoding]::BigEndianUnicode)) {
    foreach ($withBom in @($true, $false)) {
        # BOM-less UTF-16BE is not emitted by WSL.
        if (-not $withBom -and $encoding.CodePage -eq 1201) { continue }
        [byte[]]$bytes = $encoding.GetBytes($expected)
        if ($withBom) { $bytes = $encoding.GetPreamble() + $bytes }
        $actual = ConvertFrom-WslBytes -Bytes $bytes
        if ($actual -cne $expected) { throw "WSL decoding failed for $($encoding.WebName), BOM=$withBom" }
    }
}
if ((ConvertFrom-WslBytes -Bytes @()) -cne '') { throw 'Empty WSL output must remain empty' }

$python = (Get-Command python3 -ErrorAction Stop).Source
$stdoutHex = [Convert]::ToHexString([Text.Encoding]::Unicode.GetBytes("Ubuntu-魚🐟`r`n"))
$stderrHex = [Convert]::ToHexString([Text.Encoding]::UTF8.GetBytes("fixture error`n"))
$code = "import sys; sys.stdout.buffer.write(bytes.fromhex('$stdoutHex')); sys.stderr.buffer.write(bytes.fromhex('$stderrHex'))"
$lines = @(Invoke-WslText -Executable $python -ArgumentList @('-c', $code))
if ($lines.Count -ne 2 -or $lines[0] -cne 'Ubuntu-魚🐟' -or $lines[1] -cne 'fixture error') {
    throw 'WSL raw stream capture changed stdout or stderr'
}
Write-Output 'WSL byte decoding and raw stream capture passed.'
