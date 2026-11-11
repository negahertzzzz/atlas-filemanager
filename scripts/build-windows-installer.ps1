param(
    [string]$Version = "0.1.0",
    [switch]$PerfLog
)

$ErrorActionPreference = "Stop"
$repo = Split-Path $PSScriptRoot -Parent
& python (Join-Path $repo "scripts\generate-third-party.py") --verify-release
if ($LASTEXITCODE -ne 0) { throw "Third-party license verification failed." }

if ($PerfLog) {
    cargo build --release --locked -p atlas -p atlas-search-service -p atlas-cloud-extension -p atlas-icon-extension --features perf-log
} else {
    cargo build --release --locked -p atlas -p atlas-search-service -p atlas-cloud-extension -p atlas-icon-extension
}
if ($LASTEXITCODE -ne 0) { throw "Windows build failed ($LASTEXITCODE)." }
# Symbol archiving is a maintainer step; the script is not part of the public tree.
$archiveSymbols = Join-Path $PSScriptRoot "archive-symbols.ps1"
if (Test-Path -LiteralPath $archiveSymbols) {
    & $archiveSymbols -Version "$Version-installer"
} else {
    Write-Warning "archive-symbols.ps1 not found; skipping symbol archiving."
}
# The Windows installer carries the same complete payload as the Cloud package.
& (Join-Path $PSScriptRoot "build-cloud-extension.ps1") -SkipBuild

$iscc = Get-Command ISCC.exe -ErrorAction SilentlyContinue
$isccPath = if ($iscc) { $iscc.Source } else { $null }
if (-not $isccPath) {
    $commonCandidates = @(
        "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe",
        "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
        "${env:ProgramFiles}\Inno Setup 6\ISCC.exe"
    )
    foreach ($cand in $commonCandidates) {
        if (Test-Path $cand) {
            $isccPath = $cand
            break
        }
    }
    if (-not $isccPath) {
        throw "Inno Setup compiler not found. Install Inno Setup, then run this again."
    }
}

& $isccPath "/DAppVersion=$Version" "packaging\windows\atlas.iss"

$installer = "target\installer\AtlasSetup-$Version.exe"
if (Test-Path $installer) {
    Get-FileHash $installer -Algorithm SHA256
}
