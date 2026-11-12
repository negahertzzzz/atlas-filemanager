<#
.SYNOPSIS
    Builds Atlas from this checkout and updates the version installed on this PC.

.DESCRIPTION
    1. Loads the Visual Studio C++ build environment (vcvars64), which the
       WinFsp bindings need.
    2. Builds the release binaries (atlas, search service, cloud and icon
       extensions). With LTO this takes 15-20 minutes.
    3. Packages the Cloud extension (pinned, SHA-256-checked rclone).
    4. Stages the Visual C++ runtime that the installer ships next to
       atlas.exe (from Visual Studio, else from the copy installed in Windows).
    5. Compiles the Inno Setup installer into target\installer.
    6. Installs it silently over the current installation. Windows asks for
       administrator rights; Atlas is closed for the update and the tasks you
       chose at first install (shortcuts, search service, ...) are kept.
    7. Checks that the installed binaries are the ones just built.

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

# Fallback for stage-windows-runtime.ps1 when the Build Tools lack the
# "C++ Redistributable" component: the same Microsoft-signed x64 DLLs that the
# installed Visual C++ Redistributable put in System32, in the same layout.
function Copy-InstalledRuntime {
    $system = if ([Environment]::Is64BitProcess) { "System32" } else { "Sysnative" }
    $source = Join-Path $env:SystemRoot $system
    $output = Join-Path $repo "target\windows-runtime"
    $names = @("vcruntime140.dll", "vcruntime140_1.dll", "vcruntime140_threads.dll", "msvcp140.dll",
        "msvcp140_1.dll", "msvcp140_2.dll", "msvcp140_atomic_wait.dll", "msvcp140_codecvt_ids.dll",
        "concrt140.dll", "vccorlib140.dll")
    foreach ($required in @("vcruntime140.dll", "vcruntime140_1.dll", "msvcp140.dll")) {
        if (-not (Test-Path -LiteralPath (Join-Path $source $required))) {
            throw "The Visual C++ runtime is not installed ($required missing). Install the 'C++ Redistributable' component in Visual Studio Installer."
        }
    }
    if (Test-Path -LiteralPath $output) { Remove-Item -LiteralPath $output -Recurse -Force }
    New-Item -ItemType Directory -Force -Path $output | Out-Null
    $entries = @()
    foreach ($name in $names) {
        $dll = Join-Path $source $name
        if (-not (Test-Path -LiteralPath $dll)) { continue }
        $signature = Get-AuthenticodeSignature -LiteralPath $dll
        if ($signature.Status -ne "Valid" -or $signature.SignerCertificate.Subject -notmatch "O=Microsoft Corporation") {
            throw "$dll is not signed by Microsoft; refusing to bundle it."
        }
        Copy-Item -LiteralPath $dll -Destination $output
        $entries += [ordered]@{
            file    = $name
            version = (Get-Item -LiteralPath $dll).VersionInfo.FileVersion
            sha256  = (Get-FileHash -LiteralPath $dll -Algorithm SHA256).Hash.ToLowerInvariant()
        }
    }
    $entries | ConvertTo-Json -Depth 3 | Set-Content -LiteralPath (Join-Path $output "MICROSOFT-RUNTIME.json") -Encoding UTF8
    @(
        "Microsoft Visual C++ Runtime (x64 desktop redistributable files)",
        "Copyright Microsoft Corporation. These files retain Microsoft's license terms;",
        "they are not relicensed under Atlas's AGPL license.",
        "Copied unmodified from the Visual C++ Redistributable installed in Windows.",
        "File versions and SHA-256 values are recorded in MICROSOFT-RUNTIME.json.",
        "Redistribution and servicing information:",
        "https://learn.microsoft.com/cpp/windows/redistributing-visual-cpp-files"
    ) | Set-Content -LiteralPath (Join-Path $output "MICROSOFT-RUNTIME-NOTICE.txt") -Encoding UTF8
    Write-Host "Staged $($entries.Count) Microsoft runtime DLLs in $output"
}

function Get-ExplorerRedirectAgent {
    @(Get-CimInstance Win32_Process -Filter "Name = 'atlas.exe'" -ErrorAction SilentlyContinue |
        Where-Object { $_.CommandLine -match '--explorer-redirect' })
}

# Returns $true when an agent was running (and so should be restarted).
function Stop-ExplorerRedirectAgent {
    $running = Get-ExplorerRedirectAgent
    if (-not $running) { return $false }
    try {
        $stop = [System.Threading.EventWaitHandle]::OpenExisting("Local\AtlasExplorerRedirectStop")
        [void]$stop.Set()
        $stop.Dispose()
    } catch {}
    for ($i = 0; $i -lt 30 -and (Get-ExplorerRedirectAgent); $i++) { Start-Sleep -Milliseconds 100 }
    Get-ExplorerRedirectAgent | ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }
    Write-Host "  Stopped the Explorer redirect agent for the install."
    return $true
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

Write-Step "Staging the Visual C++ runtime"
# atlas.iss ships the x64 CRT DLLs from target\windows-runtime next to atlas.exe.
try {
    & (Join-Path $PSScriptRoot "stage-windows-runtime.ps1")
} catch {
    Write-Host "Visual Studio's redistributable folder is missing ($($_.Exception.Message))."
    Write-Host "Using the Visual C++ runtime installed in Windows instead."
    Copy-InstalledRuntime
}

Write-Step "Compiling the installer"
# build-windows-installer.ps1 also runs the release-only third-party license
# verification and symbol archiving; a local update does not need them.
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
# The Explorer redirect agent (atlas.exe --explorer-redirect) keeps atlas.exe
# locked; ask it to exit through its stop event and restart it afterwards.
$redirectAgent = Stop-ExplorerRedirectAgent

$log = Join-Path $repo "target\installer\AtlasSetup-$Version-install.log"
$arguments = @("/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/CLOSEAPPLICATIONS", "/LOG=`"$log`"")
# A silent install repeats the tasks chosen last time. When the search service
# is registered but its task is remembered as off, setup would replace the
# binary without ever re-registering or starting the service: keep it in sync.
if (Get-Service -Name AtlasSearchService -ErrorAction SilentlyContinue) {
    $arguments += "/MERGETASKS=`"searchservice`""
}
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
$searchService = Get-Service -Name AtlasSearchService -ErrorAction SilentlyContinue
if ($searchService) {
    try { $searchService.WaitForStatus('Running', [TimeSpan]::FromSeconds(15)) } catch {}
    $searchService.Refresh()
    if ($searchService.Status -eq 'Running') {
        Write-Host "  ok    Atlas Search Service running"
    } else {
        $image = (Get-CimInstance Win32_Service -Filter "Name='AtlasSearchService'").PathName
        Write-Host "  WARN  Atlas Search Service is $($searchService.Status) (image path: [$image])" -ForegroundColor Yellow
    }
    $searchService.Dispose()
}
if ($redirectAgent) {
    # Started with the same command Windows runs at sign-in.
    $exe = Join-Path $installed.Location "atlas.exe"
    Start-Process -FilePath $exe -ArgumentList "--explorer-redirect"
    Write-Host "  ok    Explorer redirect agent restarted"
}
Write-Host ""
Write-Host "Atlas $($installed.Version) installed in $($installed.Location)." -ForegroundColor Green
