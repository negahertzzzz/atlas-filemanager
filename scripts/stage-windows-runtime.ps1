# App-local deployment from Visual Studio's redistributable desktop CRT folder.
[CmdletBinding()]
param()
$ErrorActionPreference = 'Stop'
$repo = Split-Path $PSScriptRoot -Parent
$output = [System.IO.Path]::GetFullPath((Join-Path $repo 'target\windows-runtime'))
$allowed = [System.IO.Path]::GetFullPath((Join-Path $repo 'target')) + [System.IO.Path]::DirectorySeparatorChar
if (-not $output.StartsWith($allowed, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw "Runtime staging path escaped target: $output"
}
$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
if (-not (Test-Path -LiteralPath $vswhere)) { throw 'Visual Studio Installer / vswhere.exe is required.' }
$candidates = @(& $vswhere -latest -products '*' -find 'VC/Redist/MSVC/**/x64/Microsoft.VC*.CRT/vcruntime140.dll')
$runtime = $candidates | Where-Object { $_ -notmatch '\\onecore\\' -and (Test-Path -LiteralPath $_) } | Sort-Object -Descending | Select-Object -First 1
if (-not $runtime) { throw 'The x64 desktop Visual C++ redistributable files were not found.' }
if (Test-Path -LiteralPath $output) { Remove-Item -LiteralPath $output -Recurse -Force }
New-Item -ItemType Directory -Force -Path $output | Out-Null
$entries = @()
foreach ($dll in Get-ChildItem -LiteralPath (Split-Path $runtime -Parent) -Filter '*.dll') {
    Copy-Item -LiteralPath $dll.FullName -Destination $output
    $hasher = [System.Security.Cryptography.SHA256]::Create()
    try {
        $digest = [BitConverter]::ToString($hasher.ComputeHash([System.IO.File]::ReadAllBytes($dll.FullName))).Replace('-', '').ToLowerInvariant()
    } finally { $hasher.Dispose() }
    $entries += [ordered]@{
        file = $dll.Name
        version = $dll.VersionInfo.FileVersion
        sha256 = $digest
    }
}
$entries | ConvertTo-Json -Depth 3 | Set-Content -LiteralPath (Join-Path $output 'MICROSOFT-RUNTIME.json') -Encoding UTF8
@'
Microsoft Visual C++ Runtime (x64 desktop redistributable files)
Copyright Microsoft Corporation. These files retain Microsoft's license terms;
they are not relicensed under Atlas's AGPL license.
Copied unmodified from Visual Studio's VC/Redist/MSVC/x64 desktop CRT directory.
File versions and SHA-256 values are recorded in MICROSOFT-RUNTIME.json.
Redistribution and servicing information:
https://learn.microsoft.com/cpp/windows/redistributing-visual-cpp-files
Visual Studio license terms:
https://visualstudio.microsoft.com/license-terms/
'@ | Set-Content -LiteralPath (Join-Path $output 'MICROSOFT-RUNTIME-NOTICE.txt') -Encoding UTF8
Write-Host "Staged $($entries.Count) Microsoft runtime DLLs in $output"
