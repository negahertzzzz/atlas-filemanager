# Contribution checks and maintainer package tests

Microsoft Store is the Windows distribution and update channel. GitHub CI checks
contributions; it does not distribute an alternative Windows installation.

For maintainer package tests, open **Actions**, choose the manual packaging run,
then download its **Artifacts**. GitHub requires sign-in for artifact downloads.
Extract the downloaded ZIP before inspecting its contents.

## Contributor CI

Pull requests and pushes to `main` run formatting, compilation, CLI startup and
tests on Windows and Linux, with cached Rust dependencies. Check their results
and logs in Actions. Routine CI does not archive binaries or upload installers.
Linux package/install tests are started explicitly by maintainers rather than
automatically rebuilding packages for every pull request.

## Manual package validation for maintainers

These workflows prepare controlled test artifacts and Store submission files.
Do not advertise their EXE, portable or self-signed MSIX artifacts as a Windows
download channel: those installations are outside Store-managed updates.

Run **Actions → Packages → Run workflow** with a version from `CHANGELOG.md`.
Select both platforms, or just Windows/Linux when checking a platform-specific fix.
To repeat Linux installation tests without compiling again, run **Validate
existing Linux packages** with the original Packages run ID and version.
For a Windows search-service or packaging fix, `windows-binaries-run` can reuse a
previous manual candidate from `main`. The workflow checks unchanged application
sources, edition, version and executable hashes, then rebuilds the search service.
It refuses reuse when application inputs changed. `BINARY-PROVENANCE.json` records
the source run; normal candidates and release tags build every executable.
**Repackage existing Linux binaries** rebuilds the package metadata around an
earlier manual candidate without invoking Rust. It keeps that candidate's source
ZIP, adds a separate packaging patch, and records both revisions. Native packages
declare OpenSSL 3 for the cloud/icon programs; AppImage bundles the libraries.
This creates downloadable candidate packages without publishing a GitHub release
or submitting anything to Microsoft Store, Flathub or Snap Store.

- `atlas-windows-installer-x86_64`: `AtlasSetup-<version>.exe` for controlled
  maintainer testing outside Microsoft Store. This includes the companion
  programs and rclone. The workflow verifies installation, startup and uninstall.
- `atlas-windows-msix-store-unsigned-x86_64`: MSIX for Store submission or signing
  with your own trusted production certificate. **It cannot be installed as-is.**
  Microsoft signs MSIX submitted through the Store. For the Store Free baseline,
  select `free` when running Packages; the selected edition applies to both
  Windows formats.
- `test-msix-windows-x86_64`: self-signed MSIX plus `AtlasDev.cer` (public key
  only). For deliberate local testing, review and run `Install-TestPackage.ps1`
  in an elevated PowerShell. This trusts the supplied certificate in Trusted
  People, then installs the package. It is not a production-trusted signature.
  MSIX includes the search executable and starts it in the user's session; it
  does not register the privileged Windows system service. The test EXE's
  Search Service task can be used to validate NTFS system-service integration.
- `atlas-linux-x86_64`: DEB, RPM, Arch package, tarball, AppImage, AUR recipe,
  metadata and checksums, including the search daemon, cloud/icon programs and
  third-party notices. Builds target glibc 2.35 or newer. The Linux cloud backend
  locates rclone on PATH or downloads a checksum-verified copy on first use.
- `atlas-linux-flatpak` / `atlas-linux-snap`: locally installable store-format
  packages; they have not been submitted to their stores.
- `atlas-source`: matching source ZIP with locked dependency sources and a hash
  receipt. Keep it with the binaries when sharing a candidate.

Verify `SHA256SUMS` (Linux: `sha256sum --check SHA256SUMS`; Windows:
`Get-FileHash <file> -Algorithm SHA256`). Candidate artifacts expire according to
GitHub retention. GitHub Releases publish Linux packages and corresponding
sources, while Windows users obtain the application through Microsoft Store.

Release tags validate Windows packages in the Store Free baseline and run the
Linux packaging checks before publication. Windows EXE/MSIX files remain test
artifacts in Actions and are excluded from public GitHub release assets. Store
submission is a separate maintainer action. Existing published releases are never
overwritten. Private signing keys, test certificates and PDBs are not published.

## Update channels

Windows delegates updates to Microsoft Store. The app does not query the legacy
R2 installer feed or display standalone EXE update notices. Locally compiled
contributor builds and manual test packages do not establish an update channel.
This workflow does not create a direct MSIX App Installer feed or submit a Store
package. Maintainer package tests cover installation, startup and uninstall.

Downloaded DEB/RPM packages also need a configured, published package repository
for package-manager updates. The generated Flatpak/Snap files are local test
packages and are not connected to Flathub or Snap Store update channels.
