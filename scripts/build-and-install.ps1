<#
.SYNOPSIS
    Builds Atlas from this checkout and updates the version installed on this PC.

.DESCRIPTION
    1. Loads the Visual Studio C++ build environment (vcvars64), which the
       WinFsp bindings need.
    2. Builds the release binaries (atlas, search service, cloud and icon
       extensions). With LTO this takes 15-20 minutes.
    3. Packages the Cloud extension (pinned, SHA-256-checked rclone).
    4. Compiles the Inno Setup installer into target\installer.
    5. Installs it silently over the current installation. Windows asks for
       administrator rights; Atlas is closed for the update and the tasks you
       chose at first install (shortcuts, search service, ...) are kept.
    6. Checks that the installed atlas.exe is the one just built.

    The Microsoft Store (MSIX) copy of Atlas is separate and is not touched.

.PARAMETER Version
    Version to stamp into the build. Default: the installed version, or the
    Cargo.toml version when Atlas is not installed yet.

.PARAMETER BumpPatch
    Increase the patch number of that version (0.1.5 -> 0.1.6).

.PARAMETER SkipBuild
    Reuse the binaries already in target\release.

.PARAMETER NoInstall
    Stop after creating the installer.

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File scripts\build-and-install.ps1

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File scripts\build-and-install.ps1 -BumpPatch
#>
param(
    [string]$Version,
    [switch]$BumpPatch,
    [switch]$SkipBuild,
    [switch]$NoInstall
)

$ErrorActionPreference = "Stop"
$repo = Split-Path $PSScriptRoot -Parent
Set-Location $repo

# Inno Setup AppId from packaging\windows\atlas.iss (uninstall key "<AppId>_is1").
$uninstallKeyName = "{A66B9F93-8F34-4D8A-8D6A-A71A50000001}_is1"

function Write-Step([string]$text) {
    Write-Host ""
    Write-Host "==> $text" -ForegroundColor Cyan
}

# Runs a native tool. Its stderr (cargo progress, warnings) must not count as a
# PowerShell error when the output is redirected; the exit code decides.
function Invoke-Native([string]$exe, [string[]]$arguments) {
    $previous = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    try { & $exe @arguments } finally { $ErrorActionPreference = $previous }
}

function Get-InstalledAtlas {
    foreach ($root in @("HKLM:", "HKCU:")) {
        foreach ($view in @("SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall", "SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall")) {
            $key = Join-Path $root (Join-Path $view $uninstallKeyName)
            if (Test-Path -LiteralPath $key) {
                $item = Get-ItemProperty -LiteralPath $key
                return [pscustomobject]@{
                    Version  = $item.DisplayVersion
                    Location = $item.InstallLocation
                }
            }
        }
    }
    return $null
}

function Get-CargoVersion {
    $match = Select-String -LiteralPath (Join-Path $repo "Cargo.toml") -Pattern '^version\s*=\s*"([^"]+)"' | Select-Object -First 1
    if (-not $match) { throw "Cannot read the version from Cargo.toml." }
    return $match.Matches[0].Groups[1].Value
}

function Import-VsDevEnvironment {
    if (Get-Command cl.exe -ErrorAction SilentlyContinue) { return }
    $vcvars = $null
    $vswhere = Join-Path ${env:ProgramFiles(x86)} "Microsoft Visual Studio\Installer\vswhere.exe"
    if (Test-Path -LiteralPath $vswhere) {
        $installPath = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
        if ($installPath) {
            $candidate = Join-Path $installPath "VC\Auxiliary\Build\vcvars64.bat"
            if (Test-Path -LiteralPath $candidate) { $vcvars = $candidate }
        }
    }
    if (-not $vcvars) {
        $fallback = Join-Path ${env:ProgramFiles(x86)} "Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat"
        if (Test-Path -LiteralPath $fallback) { $vcvars = $fallback }
    }
    if (-not $vcvars) {
        throw "Visual Studio C++ Build Tools not found (vcvars64.bat). Install 'Desktop development with C++'."
    }
    $environment = & cmd.exe /d /s /c "`"$vcvars`" >nul 2>&1 && set"
    if ($LASTEXITCODE -ne 0) { throw "vcvars64.bat failed ($LASTEXITCODE)." }
    foreach ($line in $environment) {
        if ($line -match '^([^=]+)=(.*)$') {
            [Environment]::SetEnvironmentVariable($Matches[1], $Matches[2], "Process")
        }
    }
}

function Find-Iscc {
    $command = Get-Command ISCC.exe -ErrorAction SilentlyContinue
    if ($command) { return $command.Source }
    foreach ($candidate in @(
            (Join-Path $env:LOCALAPPDATA "Programs\Inno Setup 6\ISCC.exe"),
            (Join-Path ${env:ProgramFiles(x86)} "Inno Setup 6\ISCC.exe"),
            (Join-Path $env:ProgramFiles "Inno Setup 6\ISCC.exe"))) {
        if (Test-Path -LiteralPath $candidate) { return $candidate }
    }
    throw "Inno Setup 6 (ISCC.exe) not found. Install it from https://jrsoftware.org/isinfo.php"
}

# -- Version ----------------------------------------------------------------
$installed = Get-InstalledAtlas
if (-not $Version) {
    if ($installed -and $installed.Version) { $Version = $installed.Version } else { $Version = Get-CargoVersion }
}
if ($Version -notmatch '^\d+\.\d+\.\d+$') { throw "Version must look like 1.2.3 (got '$Version')." }
if ($BumpPatch) {
    $parts = $Version.Split('.')
    $Version = "{0}.{1}.{2}" -f $parts[0], $parts[1], ([int]$parts[2] + 1)
}
if ($installed) {
    Write-Host "Installed: Atlas $($installed.Version) in $($installed.Location)"
} else {
    Write-Host "Atlas (installer version) is not installed yet."
}
Write-Host "Building:  Atlas $Version"

# -- Build ------------------------------------------------------------------
$binaries = @("atlas", "atlas-search-service", "atlas-cloud-extension", "atlas-icon-extension")
if (-not $SkipBuild) {
    Write-Step "Loading the Visual Studio build environment"
    Import-VsDevEnvironment

    Write-Step "Building release binaries (15-20 minutes with LTO)"
    $env:ATLAS_RELEASE_VERSION = $Version
    $started = Get-Date
    $packages = $binaries | ForEach-Object { @("-p", $_) }
    Invoke-Native "cargo" (@("build", "--release", "--locked") + $packages)
    if ($LASTEXITCODE -ne 0) { throw "Release build failed ($LASTEXITCODE)." }
    Write-Host ("Build finished in {0:N0} min." -f ((Get-Date) - $started).TotalMinutes)
}
foreach ($name in $binaries) {
    $path = Join-Path $repo "target\release\$name.exe"
    if (-not (Test-Path -LiteralPath $path)) { throw "Missing $path. Run without -SkipBuild." }
}

# -- Package ----------------------------------------------------------------
Write-Step "Packaging the Cloud extension"
& (Join-Path $PSScriptRoot "build-cloud-extension.ps1") -SkipBuild | Out-Null

Write-Step "Compiling the installer"
# build-windows-installer.ps1 also runs the release-only third-party license
# verification, which needs files outside the public tree; a local update
# does not need it.
$iscc = Find-Iscc
Invoke-Native $iscc @("/Q", "/DAppVersion=$Version", (Join-Path $repo "packaging\windows\atlas.iss"))
if ($LASTEXITCODE -ne 0) { throw "Inno Setup failed ($LASTEXITCODE)." }
$installer = Join-Path $repo "target\installer\AtlasSetup-$Version.exe"
if (-not (Test-Path -LiteralPath $installer)) { throw "Installer not found: $installer" }
$hash = (Get-FileHash -LiteralPath $installer -Algorithm SHA256).Hash
Write-Host "Installer: $installer"
Write-Host "SHA-256:   $hash"

if ($NoInstall) {
    Write-Host ""
    Write-Host "Installer ready (not installed, -NoInstall)." -ForegroundColor Green
    return
}

# -- Install ----------------------------------------------------------------
Write-Step "Installing (Windows will ask for administrator rights)"
# A search service started by hand with --console would keep its exe locked.
Get-CimInstance Win32_Process -Filter "Name = 'atlas-search-service.exe'" -ErrorAction SilentlyContinue |
    Where-Object { $_.CommandLine -match '--console' } |
    ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }

$log = Join-Path $repo "target\installer\AtlasSetup-$Version-install.log"
$arguments = @("/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/CLOSEAPPLICATIONS", "/LOG=`"$log`"")
$process = Start-Process -FilePath $installer -ArgumentList $arguments -Wait -PassThru
if ($process.ExitCode -ne 0) {
    throw "Setup exited with code $($process.ExitCode). Log: $log"
}

# -- Verify -----------------------------------------------------------------
Write-Step "Verifying"
$installed = Get-InstalledAtlas
if (-not $installed -or -not $installed.Location) { throw "Setup finished but Atlas is not registered as installed." }
$failed = $false
foreach ($name in $binaries) {
    $built = (Get-FileHash -LiteralPath (Join-Path $repo "target\release\$name.exe") -Algorithm SHA256).Hash
    $target = Join-Path $installed.Location "$name.exe"
    $deployed = if (Test-Path -LiteralPath $target) { (Get-FileHash -LiteralPath $target -Algorithm SHA256).Hash } else { "" }
    if ($built -eq $deployed) {
        Write-Host "  ok    $name.exe"
    } else {
        Write-Host "  DIFF  $name.exe (still locked or not copied)" -ForegroundColor Yellow
        $failed = $true
    }
}
if ($failed) {
    throw "Some files were not replaced. Close Atlas (and stop 'Atlas Search Service' if needed), then run again with -SkipBuild. Log: $log"
}
Write-Host ""
Write-Host "Atlas $($installed.Version) installed in $($installed.Location)." -ForegroundColor Green
