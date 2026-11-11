param(
    [ValidatePattern('^\d+\.\d+\.\d+\.\d+$')]
    [string]$Version = "0.1.0.0",
    [ValidateSet("x64")]
    [string]$Architecture = "x64",
    # Store package: Free baseline; Premium unlocks at runtime via the Store add-on or a Polar key.
    [ValidateSet("free", "full")]
    [string]$Edition = "free",
    [switch]$SkipBuild,
    [switch]$SignForLocalTesting
)

$ErrorActionPreference = "Stop"
$repo = Split-Path $PSScriptRoot -Parent
$target = Join-Path $repo "target\msix"
$staging = Join-Path $target "staging"
$manifest = Join-Path $repo "packaging\windows\msix\Package.appxmanifest"
& python (Join-Path $repo "scripts\generate-third-party.py") --verify-release
if ($LASTEXITCODE -ne 0) { throw "Third-party license verification failed." }
$sourceVersion = ($Version.Split('.')[0..2] -join '.')
& python (Join-Path $repo "scripts\verify-source-release.py") --version $sourceVersion
if ($LASTEXITCODE -ne 0) { throw "Corresponding source snapshot verification failed." }
$icon = Join-Path $repo "packaging\icons\atlas.png"
$unplatedIcon = Join-Path $repo "packaging\icons\atlas-unplated.png"
if (-not (Test-Path $unplatedIcon)) {
    $unplatedIcon = $icon
}

# Packaging does not terminate unrelated Atlas/build processes.
if (-not $SkipBuild) {
    $env:ATLAS_RELEASE_VERSION = $sourceVersion
    $env:ATLAS_SOURCE_URL = "https://pub-89dd9e8d0b06444ca2f59e0dc245dbc9.r2.dev/source/atlas-$sourceVersion-source.zip"
    $featureArgs = @()
    if ($Edition -eq "free") {
        $featureArgs = @("--features", "free")
        # Public Polar organization id (not a secret); override with the env var for a different org.
        if (-not $env:ATLAS_POLAR_ORG_ID) { $env:ATLAS_POLAR_ORG_ID = "eec2eec8-cbfd-4967-ba65-ca98e824cce0" }
    }
    Push-Location $repo
    try {
        cargo build --release --locked @featureArgs -p atlas -p atlas-search-service -p atlas-cloud-extension -p atlas-icon-extension
        if ($LASTEXITCODE -ne 0) {
            throw "Cargo release build failed with exit code $LASTEXITCODE."
        }
        # Editions build different binaries for the same version: keep both symbol sets.
        # Symbol archiving is a maintainer step; the script is not part of the public tree.
        $archiveSymbols = Join-Path $repo "scripts\archive-symbols.ps1"
        if (Test-Path -LiteralPath $archiveSymbols) {
            & $archiveSymbols -Version "$sourceVersion-$Edition"
        } else {
            Write-Warning "archive-symbols.ps1 not found; skipping symbol archiving."
        }
    } finally {
        Pop-Location
    }
}

$exe = Join-Path $repo "target\release\atlas.exe"
if (-not (Test-Path $exe)) {
    throw "Missing $exe. Build Atlas first or omit -SkipBuild."
}

# All companions are staged in the package, never into the developer's profile.
# Stage only inside this repository's target/msix directory.
$resolvedStaging = [System.IO.Path]::GetFullPath($staging)
$allowedStaging = [System.IO.Path]::GetFullPath((Join-Path $repo "target\msix")) + [System.IO.Path]::DirectorySeparatorChar
if (-not $resolvedStaging.StartsWith($allowedStaging, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw "Refusing to clean a staging path outside target/msix: $resolvedStaging"
}
& (Join-Path $PSScriptRoot "build-cloud-extension.ps1") -SkipBuild
Remove-Item -LiteralPath $staging -Recurse -Force -ErrorAction SilentlyContinue
$assets = New-Item -ItemType Directory -Force -Path (Join-Path $staging "Assets")
Copy-Item -LiteralPath $exe -Destination (Join-Path $staging "atlas.exe")
$serviceExe = Join-Path $repo "target\release\atlas-search-service.exe"
if (Test-Path $serviceExe) {
    Copy-Item -LiteralPath $serviceExe -Destination (Join-Path $staging "atlas-search-service.exe")
}
Copy-Item -LiteralPath (Join-Path $repo "crates\atlas-core\vendor\unrar-bin\UnRAR.exe") `
    -Destination (Join-Path $staging "UnRAR.exe")
Copy-Item -LiteralPath (Join-Path $repo "packaging\windows\msix\THIRD-PARTY-NOTICES.txt") `
    -Destination $staging
Copy-Item -LiteralPath (Join-Path $repo "LICENSE") -Destination (Join-Path $staging "LICENSE.txt")
$sourceNotice = @"
Atlas $sourceVersion corresponding source and build instructions:
https://atlasfm.modhyt.org/source-code
https://pub-89dd9e8d0b06444ca2f59e0dc245dbc9.r2.dev/source/atlas-$sourceVersion-source.zip
No additional charge or private repository access is required.
Atlas: AGPL-3.0-or-later. Third-party components retain their own licenses.
"@
Set-Content -LiteralPath (Join-Path $staging "SOURCE-CODE.txt") -Value $sourceNotice -Encoding UTF8
foreach ($companion in @("atlas-cloud-extension.exe", "atlas-icon-extension.exe")) {
    $companionPath = Join-Path $repo "target\release\$companion"
    if (-not (Test-Path -LiteralPath $companionPath)) { throw "Missing companion: $companionPath" }
    Copy-Item -LiteralPath $companionPath -Destination $staging
}
Copy-Item -LiteralPath (Join-Path $repo "target\extensions\cloud\rclone.exe") -Destination $staging
Copy-Item -LiteralPath (Join-Path $repo "extensions\cloud\THIRD-PARTY-NOTICES.txt") `
    -Destination (Join-Path $staging "CLOUD-THIRD-PARTY-NOTICES.txt")
$goInventory = Get-Content -Raw -LiteralPath (Join-Path $repo "legal\rclone-dependency-inventory.json") | ConvertFrom-Json
$goReceipt = Get-Content -Raw -LiteralPath (Join-Path $repo "legal\rclone-source-release.json") | ConvertFrom-Json
$goNotices = Join-Path $repo "packaging\windows\msix\RCLONE-THIRD-PARTY-NOTICES.txt"
if ((Get-FileHash -LiteralPath (Join-Path $staging "rclone.exe") -Algorithm SHA256).Hash.ToLowerInvariant() -ne $goInventory.binarySha256) {
    throw "The bundled rclone binary changed. Audit its exact modules before packaging."
}
if ((Get-FileHash -LiteralPath $goNotices -Algorithm SHA256).Hash.ToLowerInvariant() -ne $goReceipt.noticeSha256) {
    throw "The rclone dependency notices changed. Regenerate their source receipt."
}
Copy-Item -LiteralPath $goNotices -Destination $staging


[xml]$xml = Get-Content -Raw $manifest
$identity = $xml.Package.Identity
$identity.Version = $Version
$identity.ProcessorArchitecture = $Architecture
$xml.Save((Join-Path $staging "AppxManifest.xml"))

Add-Type -AssemblyName System.Drawing

function Save-ResizedAsset {
    param(
        [System.Drawing.Image]$SourceImage,
        [int]$TargetWidth,
        [int]$TargetHeight,
        [string]$DestinationPath,
        [switch]$Contain
    )
    $bitmap = [System.Drawing.Bitmap]::new($TargetWidth, $TargetHeight)
    try {
        $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
        try {
            $graphics.Clear([System.Drawing.Color]::Transparent)
            $graphics.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
            $graphics.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::HighQuality
            $graphics.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
            $graphics.CompositingQuality = [System.Drawing.Drawing2D.CompositingQuality]::HighQuality

            if ($Contain) {
                $scale = [Math]::Min($TargetWidth / $SourceImage.Width, $TargetHeight / $SourceImage.Height)
                $destW = [int]($SourceImage.Width * $scale)
                $destH = [int]($SourceImage.Height * $scale)
                $destX = [int](($TargetWidth - $destW) / 2)
                $destY = [int](($TargetHeight - $destH) / 2)
                $graphics.DrawImage($SourceImage, $destX, $destY, $destW, $destH)
            } else {
                $graphics.DrawImage($SourceImage, 0, 0, $TargetWidth, $TargetHeight)
            }
        } finally {
            $graphics.Dispose()
        }
        $bitmap.Save($DestinationPath, [System.Drawing.Imaging.ImageFormat]::Png)
    } finally {
        $bitmap.Dispose()
    }
}

$source = [System.Drawing.Image]::FromFile($icon)
$sourceUnplated = [System.Drawing.Image]::FromFile($unplatedIcon)
try {
    # 1. Square150x150Logo (Medium Tile)
    $sq150Scales = @{
        "Square150x150Logo.png"           = @(150, 150)
        "Square150x150Logo.scale-100.png" = @(150, 150)
        "Square150x150Logo.scale-125.png" = @(188, 188)
        "Square150x150Logo.scale-150.png" = @(225, 225)
        "Square150x150Logo.scale-200.png" = @(300, 300)
        "Square150x150Logo.scale-400.png" = @(600, 600)
    }
    foreach ($k in $sq150Scales.Keys) {
        $w, $h = $sq150Scales[$k]
        Save-ResizedAsset -SourceImage $source -TargetWidth $w -TargetHeight $h -DestinationPath (Join-Path $assets.FullName $k)
    }

    # 2. StoreLogo
    $storeScales = @{
        "StoreLogo.png"           = @(50, 50)
        "StoreLogo.scale-100.png" = @(50, 50)
        "StoreLogo.scale-125.png" = @(63, 63)
        "StoreLogo.scale-150.png" = @(75, 75)
        "StoreLogo.scale-200.png" = @(100, 100)
        "StoreLogo.scale-400.png" = @(200, 200)
    }
    foreach ($k in $storeScales.Keys) {
        $w, $h = $storeScales[$k]
        Save-ResizedAsset -SourceImage $source -TargetWidth $w -TargetHeight $h -DestinationPath (Join-Path $assets.FullName $k)
    }

    # 3. Wide310x150Logo
    $wideScales = @{
        "Wide310x150Logo.png"           = @(310, 150)
        "Wide310x150Logo.scale-100.png" = @(310, 150)
        "Wide310x150Logo.scale-125.png" = @(388, 188)
        "Wide310x150Logo.scale-150.png" = @(465, 225)
        "Wide310x150Logo.scale-200.png" = @(620, 300)
        "Wide310x150Logo.scale-400.png" = @(1240, 600)
    }
    foreach ($k in $wideScales.Keys) {
        $w, $h = $wideScales[$k]
        Save-ResizedAsset -SourceImage $source -TargetWidth $w -TargetHeight $h -DestinationPath (Join-Path $assets.FullName $k) -Contain
    }

    # 4. Square44x44Logo (App List & System scales)
    $sq44Scales = @{
        "Square44x44Logo.png"               = @(44, 44)
        "Square44x44Logo.scale-100.png"     = @(44, 44)
        "Square44x44Logo.scale-125.png"     = @(55, 55)
        "Square44x44Logo.scale-150.png"     = @(66, 66)
        "Square44x44Logo.scale-200.png"     = @(88, 88)
        "Square44x44Logo.scale-400.png"     = @(176, 176)
        "Square44x44Logo.targetsize-16.png"  = @(16, 16)
        "Square44x44Logo.targetsize-24.png"  = @(24, 24)
        "Square44x44Logo.targetsize-32.png"  = @(32, 32)
        "Square44x44Logo.targetsize-48.png"  = @(48, 48)
        "Square44x44Logo.targetsize-256.png" = @(256, 256)
    }
    foreach ($k in $sq44Scales.Keys) {
        $w, $h = $sq44Scales[$k]
        Save-ResizedAsset -SourceImage $source -TargetWidth $w -TargetHeight $h -DestinationPath (Join-Path $assets.FullName $k)
    }

    # 5. Taskbar Unplated variants (Crucial for modern Windows 10/11 taskbar!)
    $unplatedSizes = @(16, 24, 32, 48, 256)
    foreach ($sz in $unplatedSizes) {
        Save-ResizedAsset -SourceImage $sourceUnplated -TargetWidth $sz -TargetHeight $sz `
            -DestinationPath (Join-Path $assets.FullName "Square44x44Logo.altform-unplated_targetsize-$sz.png")
        Save-ResizedAsset -SourceImage $sourceUnplated -TargetWidth $sz -TargetHeight $sz `
            -DestinationPath (Join-Path $assets.FullName "Square44x44Logo.altform-lightunplated_targetsize-$sz.png")
    }
} finally {
    $source.Dispose()
    $sourceUnplated.Dispose()
}

$makeAppx = Get-ChildItem "${env:ProgramFiles(x86)}\Windows Kits\10\bin" `
    -Filter makeappx.exe -Recurse |
    Where-Object FullName -Match "\\x64\\makeappx\.exe$" |
    Sort-Object FullName -Descending |
    Select-Object -First 1 -ExpandProperty FullName
if (-not $makeAppx) {
    throw "MakeAppx.exe not found. Install the Windows 10/11 SDK."
}

$makepri = Join-Path (Split-Path $makeAppx -Parent) "makepri.exe"
if (Test-Path $makepri) {
    $priconfig = Join-Path $target "priconfig.xml"
    & $makepri createconfig /cf $priconfig /dq lang-en-US /pv 10.0.0 /o | Out-Null
    & $makepri new /pr $staging /cf $priconfig /of (Join-Path $staging "resources.pri") /o | Out-Null
    Remove-Item -LiteralPath $priconfig -Force -ErrorAction SilentlyContinue
}

$package = Join-Path $target "Atlas_${Version}_${Architecture}.msix"
& $makeAppx pack /o /d $staging /p $package
if ($LASTEXITCODE -ne 0) {
    throw "MakeAppx failed with exit code $LASTEXITCODE."
}

$signtool = Join-Path (Split-Path $makeAppx -Parent) "signtool.exe"
if ($SignForLocalTesting -and (Test-Path $signtool)) {
    try {
        $cert = Get-ChildItem Cert:\CurrentUser\My | Where-Object { $_.Subject -eq "CN=49A3C9B5-859D-4613-AE50-D104622E4EBC" } | Select-Object -First 1
        if (-not $cert) {
            $cert = New-SelfSignedCertificate -Type Custom -Subject "CN=49A3C9B5-859D-4613-AE50-D104622E4EBC" `
                -KeyUsage DigitalSignature -FriendlyName "Atlas Dev Cert" -CertStoreLocation "Cert:\CurrentUser\My" `
                -TextExtension @("2.5.29.37={text}1.3.6.1.5.5.7.3.3", "2.5.29.19={text}")
        }
        & $signtool sign /fd SHA256 /sha1 $cert.Thumbprint /q $package
        Write-Host "Successfully signed $package with dev cert ($($cert.Thumbprint))." -ForegroundColor Green
        $cerPath = Join-Path $target "AtlasDev.cer"
        Export-Certificate -Cert $cert -FilePath $cerPath -Force | Out-Null
        Write-Host "Exported public certificate to $cerPath" -ForegroundColor Green
    } catch {
        Write-Warning "Failed to sign MSIX package: $_"
    }
}

Get-Item $package
Get-FileHash $package -Algorithm SHA256
