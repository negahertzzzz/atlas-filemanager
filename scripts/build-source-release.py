"""Prepare a reviewed, secret-free source archive for the Windows release.

First run cargo vendor --locked --versioned-dirs with the directory reported
by --vendor-dir, saving Cargo's config output as target/source-vendor-config.toml.
This does not upload anything; publish the ZIP and JSON receipt only after review.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import re
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SKIP = {".git", ".github", "target", "node_modules", "__pycache__", ".wrangler", ".playwright-cli", "artifacts", "perf-logs"}
ROOT_FILES = ["Cargo.toml", "Cargo.lock", "LICENSE", "README.md", "ARCHITECTURE.md", "CONTRIBUTING.md", "rust-toolchain.toml", "rustfmt.toml", "clippy.toml"]
SCRIPTS = ["winfsp-build.rs", "stage-windows-runtime.ps1", "archive-symbols.ps1", "build-windows-msix.ps1", "build-windows-installer.ps1", "build-cloud-extension.ps1", "recover-licensed-artwork.py", "generate-third-party.py", "build-source-release.py", "verify-source-release.py", "audit-rclone-notices.py", "linux_release.py", "linux_repository.py", "linux-smoke.sh", "linux-container-test.sh", "linux-store-packages.sh", "install-linux.sh", "test-linux-search-service.py", "verify-windows-search-service.ps1", "restore-windows-binaries.py"]


def eligible(path: Path, parent: Path) -> bool:
    relative = path.relative_to(parent)
    if any(part in SKIP for part in relative.parts):
        return False
    name = path.name.lower()
    if name.startswith(".env") or name in {"credentials", "credentials.toml", ".dev.vars", "source-release.json"}:
        return False
    if path.suffix.lower() in {".pfx", ".p12", ".key", ".log", ".dmp", ".zip", ".msix"}:
        return False
    if "liquid-glass" in relative.parts and path.suffix.lower() == ".png":
        return False
    if path.is_symlink() and not path.resolve().is_relative_to(ROOT.resolve()):
        raise ValueError(f"Source symlink escapes workspace: {relative}")
    return path.is_file()


def build(version: str, source_url: str | None = None):
    from linux_release import validate_version
    validate_version(version)
    msix_version = version.split("-", 1)[0] + ".0"
    if source_url is not None and (not source_url.startswith("https://") or any(c in source_url for c in "\r\n")):
        raise ValueError("Corresponding source URL must be HTTPS")
    staging = ROOT / f"target/source-release-{version}"
    vendor = staging / "vendor-registry"
    if not vendor.is_dir() or not any(vendor.iterdir()):
        raise ValueError(f"Run cargo vendor into {vendor} first")
    config_bytes = (ROOT / "target/source-vendor-config.toml").read_bytes()
    config = config_bytes.decode("utf-16" if config_bytes.startswith((b"\xff\xfe", b"\xfe\xff")) else "utf-8-sig")
    if "[source.vendored-sources]" not in config:
        raise ValueError("Missing Cargo vendor source-replacement config")
    config = re.sub(r'directory = ".*"', 'directory = "vendor-registry"', config)
    inputs = {}
    for name in ROOT_FILES:
        inputs[name] = ROOT / name
    for directory in ["src", "crates", "extensions", "vendor/gpui-ce", "vendor/gpui-component-fork", "legal", "packaging/windows", "packaging/linux", "packaging/icons"]:
        parent = ROOT / directory
        for path in sorted(parent.rglob("*")):
            if eligible(path, parent):
                inputs[path.relative_to(ROOT).as_posix()] = path
    for name in SCRIPTS:
        path = ROOT / "scripts" / name
        if path.is_file():
            inputs[f"scripts/{name}"] = path
    # Cargo vendor has already selected published source files and checksum data.
    # Preserve its fixtures too; filtering them would invalidate Cargo checksums.
    registry = {"vendor-registry/" + p.relative_to(vendor).as_posix(): p for p in vendor.rglob("*") if p.is_file()}
    # Explicit known-safe build config only: never include Cargo credentials.
    local_config = (ROOT / ".cargo/config.toml").read_text(encoding="utf-8")
    generated = {".cargo/config.toml": (local_config + "\n" + config).encode()}
    source_url = source_url or f"https://pub-89dd9e8d0b06444ca2f59e0dc245dbc9.r2.dev/source/atlas-{version}-source.zip"
    generated["SOURCE-README.txt"] = f"""Atlas {version} — corresponding Windows release source

This snapshot includes uncommitted/local changes used for this release. Atlas
code remains AGPL-3.0-or-later; dependencies retain their licenses. See LICENSE
and legal/THIRD-PARTY-NOTICES.txt. No repository access, NDA or extra purchase is
required to obtain this archive. Recipients retain their license rights.

BUILD ON WINDOWS x64
Install Rust 1.97.1, the Microsoft C++ build tools and Windows SDK. Keep this
directory outside WindowsApps. The locked Rust dependency sources are included
under vendor-registry and .cargo/config.toml enables offline Cargo resolution.

Icons8 PNGs are separately licensed artwork, not standalone downloads in this
source archive. Recover the exact inputs from your matching Atlas executable
or MSIX before building:
  python scripts/recover-licensed-artwork.py PATH_TO_MATCHING_ATLAS_EXE_OR_MSIX
The helper verifies every PNG against legal/assets.json. The files keep their
Icons8 terms and attribution. All program logic and the helper's own code are
provided in preferred editable source form.

In PowerShell, set the release identifiers and build:
  $env:ATLAS_RELEASE_VERSION = '{version}'
  $env:ATLAS_SOURCE_URL = '{source_url}'
  cargo build --release --locked --offline -p atlas -p atlas-search-service -p atlas-cloud-extension -p atlas-icon-extension

The MSIX manifest version for this release is {msix_version}. Packaging uses the
official rclone archive pinned by scripts/build-cloud-extension.ps1. That tool
is an independent program and is not relicensed as Atlas. UnRAR.exe is likewise
an unmodified independent tool with its own notices.

The archive's source-file hashes are recorded in SOURCE-FILES.json. Do not add
credentials, private configuration, profiling data or unrelated reference repos.
Source download: {source_url}
Contact: support@modhyt.org
""".encode("utf-8")
    hashes = {name: hashlib.sha256(path.read_bytes()).hexdigest() for name, path in sorted(inputs.items())}
    generated["SOURCE-FILES.json"] = (json.dumps({"version":version,"files":hashes},indent=2)+"\n").encode()
    output = ROOT / "target/source"
    output.mkdir(parents=True, exist_ok=True)
    archive_path = output / f"atlas-{version}-source.zip"
    all_inputs = dict(sorted({**inputs, **registry}.items()))
    with zipfile.ZipFile(archive_path,"w",compression=zipfile.ZIP_DEFLATED,compresslevel=6) as archive:
        for name, path in all_inputs.items():
            content = path.read_bytes()
            # Refuse obvious private-key material; report only the file path.
            if name in inputs and path.suffix.lower() in {".rs", ".toml", ".json", ".txt", ".pem"} and re.search(rb"-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----\r?\n[A-Za-z0-9+/=\r\n]{80,}",content):
                raise ValueError(f"Private-key material found in source input: {name}")
            info = zipfile.ZipInfo(name,(2026,10,31,0,0,0))
            info.compress_type=zipfile.ZIP_DEFLATED
            info.external_attr=(0o100644 << 16)
            archive.writestr(info,content)
        for name, content in sorted(generated.items()):
            info=zipfile.ZipInfo(name,(2026,10,31,0,0,0));info.compress_type=zipfile.ZIP_DEFLATED;info.external_attr=(0o100644 << 16)
            archive.writestr(info,content)
    receipt={"version":version,"msixVersion":msix_version,"url":source_url,"sha256":hashlib.sha256(archive_path.read_bytes()).hexdigest(),"bytes":archive_path.stat().st_size,"workspaceFiles":len(inputs),"vendoredFiles":len(registry),"workspaceHashes":hashes,"excluded":"Private configuration, git metadata, build outputs and standalone Icons8 PNGs. The included recovery helper reconstructs exact artwork inputs from the recipient's binary."}
    receipt_path=output/f"atlas-{version}-source.json"
    receipt_path.write_text(json.dumps(receipt,indent=2)+"\n",encoding="utf-8")
    print(f"Prepared {archive_path.name}: {receipt['bytes']} bytes; {len(inputs)} workspace files and {len(registry)} vendor files.")
    print(f"SHA256 {receipt['sha256']}")


if __name__ == "__main__":
    parser=argparse.ArgumentParser(description=__doc__);parser.add_argument("--version",required=True)
    parser.add_argument("--source-url")
    args = parser.parse_args()
    build(args.version, args.source_url)
