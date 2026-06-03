#requires -Version 7.0
param(
    [switch]$Repair
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$script:Errors = 0
$script:Warnings = 0

. (Join-Path $PSScriptRoot "hooks/native-process.ps1")

function Info {
    param([string]$Message)
    Write-Host "[hook-ready] $Message"
}

function Ok {
    param([string]$Message)
    Write-Host "[OK] $Message"
}

function Warn {
    param([string]$Message)
    Write-Host "[WARN] $Message"
    $script:Warnings++
}

function ErrorItem {
    param([string]$Message)
    Write-Host "[ERROR] $Message"
    $script:Errors++
}

function Command-Exists {
    param([string]$Name)
    $null -ne (Get-Command $Name -ErrorAction SilentlyContinue)
}

function Git {
    param([string[]]$Arguments)
    $result = Invoke-Native -FileName "git" -Arguments $Arguments
    if ($result.ExitCode -ne 0) {
        throw "git $($Arguments -join ' ') failed:`n$($result.Output)"
    }
    $result.Stdout
}

if (-not (Command-Exists "git")) {
    ErrorItem "git is required to install and run hooks."
    exit 1
}

$repoRoot = (Git @("rev-parse", "--show-toplevel")).Trim()
Set-Location $repoRoot

Info "Repository: $repoRoot"
Ok "PowerShell $($PSVersionTable.PSVersion) is available."

$hooksPath = (Invoke-Native -FileName "git" -Arguments @("config", "--local", "--get", "core.hooksPath")).Stdout.Trim()
if ($hooksPath -eq ".githooks") {
    Ok "core.hooksPath is .githooks."
} elseif ($Repair) {
    [void](Git @("config", "--local", "core.hooksPath", ".githooks"))
    Ok "Configured core.hooksPath to .githooks."
} else {
    ErrorItem "core.hooksPath is '$hooksPath' (expected .githooks). Run scripts/check-hook-readiness.ps1 -Repair."
}

foreach ($hook in @(".githooks/pre-commit", ".githooks/pre-push")) {
    if (-not (Test-Path -LiteralPath $hook)) {
        ErrorItem "Missing hook: $hook"
        continue
    }

    $mode = (Git @("ls-files", "--stage", "--", $hook)).Split(" ", [System.StringSplitOptions]::RemoveEmptyEntries)[0]
    if ($mode -eq "100755") {
        Ok "$hook is executable in the Git index."
    } elseif ($Repair) {
        [void](Git @("update-index", "--chmod=+x", "--", $hook))
        Ok "Repaired executable bit for $hook."
    } else {
        ErrorItem "$hook has Git index mode $mode (expected 100755). Run scripts/check-hook-readiness.ps1 -Repair."
    }

    if (-not $IsWindows) {
        $fsCheck = Invoke-Native -FileName "sh" -Arguments @("-c", "test -x ""`$1""", "sh", $hook)
        if ($fsCheck.ExitCode -eq 0) {
            Ok "$hook is executable on this filesystem."
        } elseif ($Repair) {
            chmod +x -- $hook
            Ok "Repaired filesystem executable bit for $hook."
        } else {
            ErrorItem "$hook is not executable on this filesystem. Run scripts/check-hook-readiness.ps1 -Repair."
        }
    }
}

$requiredTools = @("git", "pwsh")
foreach ($tool in $requiredTools) {
    if (Command-Exists $tool) {
        Ok "Required tool available: $tool"
    } else {
        ErrorItem "Required tool missing: $tool"
    }
}

$optionalTools = @("cargo", "node", "npm", "shellcheck", "yamllint", "lychee", "jq", "yq", "taplo")
foreach ($tool in $optionalTools) {
    if (Command-Exists $tool) {
        Ok "Optional workflow tool available: $tool"
    } else {
        Warn "Optional workflow tool missing: $tool"
    }
}

if (Test-Path -LiteralPath ".markdownlint-version") {
    $requiredMarkdownlint = (Get-Content -LiteralPath ".markdownlint-version" -Raw).Trim()
    $localMarkdownlint = Join-Path $repoRoot "node_modules/.bin/markdownlint-cli2"
    if ($IsWindows) {
        $localMarkdownlint = Join-Path $repoRoot "node_modules/.bin/markdownlint-cli2.cmd"
    }

    if (Test-Path -LiteralPath $localMarkdownlint) {
        $version = (& $localMarkdownlint --version 2>$null) -join "`n"
        if ($version -match [regex]::Escape($requiredMarkdownlint)) {
            Ok "Pinned markdownlint-cli2 $requiredMarkdownlint is installed locally."
        } else {
            Warn "Local markdownlint-cli2 version does not match .markdownlint-version ($requiredMarkdownlint)."
        }
    } else {
        Warn "Pinned markdownlint-cli2 is not installed locally. Run npm ci before local CI checks."
    }
} else {
    Warn ".markdownlint-version is missing."
}

if ($script:Errors -gt 0) {
    Info "$script:Errors error(s), $script:Warnings warning(s)."
    exit 1
}

Info "Hook readiness passed with $script:Warnings warning(s)."
exit 0
