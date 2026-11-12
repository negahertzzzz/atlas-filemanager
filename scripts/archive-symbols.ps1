# Archive the release binaries with their PDBs so user crash stacks can be
# symbolized later (scripts/symbolize-crash.py). PDBs are never packaged.
#
#   scripts/archive-symbols.ps1 -Version 0.2.0 [-TargetDir target\release]
#
# Writes symbols\<version>\ (git-ignored). Each binary keeps its exact PDB:
# rebuilding the same version replaces them, and manifest.json records the
# exe hashes so a mismatched archive is detectable. Copy this folder somewhere
# durable (it is the only way to read release crash stacks).
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$Version,
    [string]$TargetDir
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
if (-not $TargetDir) { $TargetDir = Join-Path $root "target\release" }
if ($Version -notmatch '^[0-9A-Za-z.+-]+$') { throw "Unsafe version string: $Version" }

$destination = Join-Path $root "symbols\$Version"
New-Item -ItemType Directory -Force -Path $destination | Out-Null

$entries = @()
foreach ($name in @("atlas", "atlas-search-service", "atlas-cloud-extension", "atlas-icon-extension")) {
    $exe = Join-Path $TargetDir "$name.exe"
    if (-not (Test-Path -LiteralPath $exe)) { continue }
    # Cargo names the PDB after the crate (dashes become underscores).
    $pdb = Join-Path $TargetDir ("{0}.pdb" -f ($name -replace '-', '_'))
    if (-not (Test-Path -LiteralPath $pdb)) {
        Write-Warning "No PDB for $name.exe; its crash stacks will stay unsymbolized."
        continue
    }
    Copy-Item -LiteralPath $exe -Destination $destination -Force
    Copy-Item -LiteralPath $pdb -Destination $destination -Force
    $entries += [ordered]@{
        exe    = "$name.exe"
        pdb    = (Split-Path -Leaf $pdb)
        sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $exe).Hash.ToLowerInvariant()
    }
}
if ($entries.Count -eq 0) { throw "No release binaries with PDBs found in $TargetDir." }

[ordered]@{
    version  = $Version
    archived = (Get-Date).ToUniversalTime().ToString("o")
    binaries = $entries
} | ConvertTo-Json -Depth 4 | Set-Content -Encoding UTF8 -LiteralPath (Join-Path $destination "manifest.json")

Write-Host "Symbols:  $destination ($($entries.Count) binaries)"
