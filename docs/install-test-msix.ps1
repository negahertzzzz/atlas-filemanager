# Explicit developer/test installation. Imports only the supplied public cert.
[CmdletBinding()]
param()
$ErrorActionPreference = 'Stop'
$package = @(Get-ChildItem -LiteralPath $PSScriptRoot -Filter '*_test-signed.msix')
if ($package.Count -ne 1) { throw 'Expected exactly one test-signed MSIX beside this script.' }
$certificate = Join-Path $PSScriptRoot 'AtlasDev.cer'
if (-not (Test-Path -LiteralPath $certificate)) { throw 'Missing AtlasDev.cer.' }
Write-Host 'This is a self-signed Atlas TEST package, not a Microsoft Store installation.'
Write-Host 'It requires an elevated PowerShell to trust its public certificate.'
Import-Certificate -FilePath $certificate -CertStoreLocation Cert:\LocalMachine\TrustedPeople | Out-Null
Add-AppxPackage -Path $package[0].FullName
