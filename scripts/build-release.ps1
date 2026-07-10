param(
    [switch]$Clean,
    [switch]$CliOnly,
    [switch]$RunTests,
    [switch]$VerboseCargo,
    [string]$Target = ""
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$cargoCommand = Get-Command cargo -ErrorAction SilentlyContinue
if ($null -ne $cargoCommand) {
    $script:CargoExecutable = $cargoCommand.Source
} else {
    $cargoFallback = Join-Path $env:USERPROFILE ".cargo\bin\cargo.exe"
    if (-not (Test-Path -LiteralPath $cargoFallback)) {
        throw "cargo was not found in PATH or $cargoFallback. Install Rust from https://rustup.rs/."
    }
    $script:CargoExecutable = $cargoFallback
}

function Invoke-Cargo {
    param(
        [Parameter(Mandatory = $true)]
        [string[]]$CargoArgs
    )

    Write-Host ""
    Write-Host "cargo $($CargoArgs -join ' ')" -ForegroundColor Cyan
    & $script:CargoExecutable @CargoArgs
    if ($LASTEXITCODE -ne 0) {
        throw "cargo $($CargoArgs -join ' ') failed with exit code $LASTEXITCODE"
    }
}

$repoRoot = Resolve-Path (Join-Path $PSScriptRoot "..")

Push-Location $repoRoot
try {
    if ($Clean) {
        Invoke-Cargo @("clean")
    }

    if ($RunTests) {
        Invoke-Cargo @("test", "--all-targets")
    }

    $buildArgs = @("build", "--release")

    if ($CliOnly) {
        $buildArgs += @("-p", "rsmpeg-cli")
    } else {
        $buildArgs += "--workspace"
    }

    if ($Target.Trim().Length -gt 0) {
        $buildArgs += @("--target", $Target)
    }

    if ($VerboseCargo) {
        $buildArgs += "--verbose"
    }

    Invoke-Cargo $buildArgs

    $releaseDir = if ($Target.Trim().Length -gt 0) {
        Join-Path $repoRoot "target\$Target\release"
    } else {
        Join-Path $repoRoot "target\release"
    }

    $exePath = Join-Path $releaseDir "rsmpeg.exe"
    if (-not (Test-Path -LiteralPath $exePath)) {
        $exePath = Join-Path $releaseDir "rsmpeg"
    }

    Write-Host ""
    Write-Host "Release build complete." -ForegroundColor Green
    if (Test-Path -LiteralPath $exePath) {
        Write-Host "Binary: $exePath" -ForegroundColor Green
    } else {
        Write-Host "Release artifacts: $releaseDir" -ForegroundColor Yellow
    }
} finally {
    Pop-Location
}
