#!/usr/bin/env python3
"""Build Linux artifacts from one version and one changelog (Python 3.10+)."""
import argparse
import base64
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tarfile
import urllib.request
import xml.etree.ElementTree as ET

ROOT = Path(__file__).resolve().parents[1]
APP_ID = "io.github.neondeex.atlas-filemanager"
VERSION_RE = re.compile(r"(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-([0-9A-Za-z.-]+))?")


def validate_version(version):
    match = VERSION_RE.fullmatch(version)
    if not match:
        raise ValueError("Use a version such as 0.2.0 or 0.2.0-rc.1, without v")
    if match[4]:
        for part in match[4].split("."):
            if not part or (part.isdigit() and len(part) > 1 and part.startswith("0")):
                raise ValueError("Invalid prerelease identifier")
    return version


def repository(value):
    if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", value):
        raise ValueError("Repository must be OWNER/REPO")
    return value


def release_notes(changelog, version):
    headings = list(re.finditer(r"^## \[([^\]]+)\](?: - (\d{4}-\d{2}-\d{2}))?\s*$", changelog, re.M))
    found = [i for i, match in enumerate(headings) if match[1] == version]
    if len(found) != 1:
        raise ValueError(f"CHANGELOG.md needs exactly one heading for [{version}]")
    i = found[0]
    heading = headings[i]
    if not heading[2]:
        raise ValueError("Release heading needs a YYYY-MM-DD date")
    from datetime import date
    date.fromisoformat(heading[2])
    end = headings[i + 1].start() if i + 1 < len(headings) else len(changelog)
    notes = changelog[heading.end():end].strip()
    if not notes:
        raise ValueError("Release notes cannot be empty")
    return heading[2], notes


def sha256(path):
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def run(*args, **kwargs):
    subprocess.run([str(arg) for arg in args], check=True, **kwargs)


def metadata(version, repo, output):
    validate_version(version)
    repository(repo)
    output.mkdir(parents=True, exist_ok=True)
    date, notes = release_notes((ROOT / "CHANGELOG.md").read_text(), version)
    release_url = f"https://github.com/{repo}/releases/tag/v{version}"
    data = dict(version=version, date=date, notes=notes, repository=repo,
                release_url=release_url, prerelease="-" in version)
    (output / "release.json").write_text(json.dumps(data, indent=2) + "\n")
    (output / "release-notes.md").write_text(notes + "\n")
    component = ET.Element("component", type="desktop-application")
    for name, text in [("id", APP_ID), ("name", "Atlas"),
                       ("summary", "Manage files with multiple panes and fast keyboard navigation"),
                       ("metadata_license", "CC0-1.0"), ("project_license", "AGPL-3.0-or-later")]:
        ET.SubElement(component, name).text = text
    description = ET.SubElement(component, "description")
    ET.SubElement(description, "p").text = "Atlas is a native file manager built with Rust and GPUI. Browse files in multiple panes, preview content, and organize your workspace."
    ET.SubElement(component, "launchable", type="desktop-id").text = "atlas.desktop"
    ET.SubElement(component, "url", type="homepage").text = "https://atlasnative.com"
    ET.SubElement(component, "url", type="bugtracker").text = f"https://github.com/{repo}/issues"
    ET.SubElement(ET.SubElement(component, "developer", id="io.github.neondeex"), "name").text = "Atlas contributors"
    ET.SubElement(component, "content_rating", type="oars-1.1")
    releases = ET.SubElement(component, "releases")
    release = ET.SubElement(releases, "release", version=version, date=date,
                            type="development" if data["prerelease"] else "stable")
    ET.SubElement(release, "url", type="details").text = release_url
    details = ET.SubElement(release, "description")
    items = ET.SubElement(details, "ul")
    bullets = [line[2:] for line in notes.splitlines() if line.startswith("- ")]
    for bullet in bullets or [notes]:
        ET.SubElement(items, "li").text = bullet
    ET.indent(component)
    ET.ElementTree(component).write(output / f"{APP_ID}.metainfo.xml", encoding="utf-8", xml_declaration=True)
    return data


def tools(destination):
    destination.mkdir(parents=True, exist_ok=True)
    lock = json.loads((ROOT / "packaging/linux/tools.json").read_text())
    for name, item in lock.items():
        download = destination / (name + ".download")
        request = urllib.request.Request(item["url"], headers={"User-Agent": "Atlas-packaging"})
        with urllib.request.urlopen(request, timeout=120) as response, download.open("wb") as stream:
            shutil.copyfileobj(response, stream)
        if sha256(download) != item["sha256"]:
            raise ValueError(f"Checksum mismatch for {name}; update tools.json intentionally")
        if "archive_member" in item:
            with tarfile.open(download) as archive:
                member = archive.extractfile(item["archive_member"])
                if member is None:
                    raise ValueError(f"Missing {name} in archive")
                with (destination / name).open("wb") as stream:
                    shutil.copyfileobj(member, stream)
        else:
            download.replace(destination / name)
        (destination / name).chmod(0o755)
        download.unlink(missing_ok=True)


def write_checksums(output):
    files = sorted(p for p in output.iterdir() if p.is_file() and not p.name.endswith((".sha256", ".asc")) and p.name != "SHA256SUMS")
    (output / "SHA256SUMS").write_text("".join(f"{sha256(p)}  {p.name}\n" for p in files))
    for path in files:
        (output / (path.name + ".sha256")).write_text(f"{sha256(path)}  {path.name}\n")


def packages(version, repo, binary, output, tool_dir, data_dir):
    if output.exists() and any(output.iterdir()):
        raise ValueError("Package output must be empty; use a fresh --output directory")
    info = metadata(version, repo, data_dir)
    icon = data_dir / "atlas.png"
    # Normalize the official Windows raster to a supported hicolor/AppImage
    # size without using the original SVG's unsupported shadow filters.
    encoded = base64.b64encode((ROOT / "packaging/icons/atlas.png").read_bytes()).decode('ascii')
    wrapper = data_dir / 'atlas-icon.svg'
    wrapper.write_text('<svg xmlns="http://www.w3.org/2000/svg" '
                       'xmlns:xlink="http://www.w3.org/1999/xlink" viewBox="0 0 1 1">'
                       f'<image width="1" height="1" xlink:href="data:image/png;base64,{encoded}"/>'
                       '</svg>', encoding='utf-8')
    run('rsvg-convert', '--width', '512', '--height', '512', '--output', icon, wrapper)
    output.mkdir(parents=True, exist_ok=True)
    companions = [binary.parent / name for name in ("atlas-search-service", "atlas-cloud-extension", "atlas-icon-extension")]
    for companion in companions:
        if not companion.is_file():
            raise FileNotFoundError(f"Missing built companion: {companion}")
    env = dict(os.environ, ATLAS_VERSION=version, ATLAS_BINARY=str(binary),
               ATLAS_BINARY_DIR=str(binary.parent), ATLAS_PACKAGE_DATA=str(data_dir))
    # nFPM expands some metadata fields, but not content source paths.
    config = (ROOT / "packaging/linux/nfpm.yaml").read_text()
    for name in ["ATLAS_VERSION", "ATLAS_BINARY", "ATLAS_BINARY_DIR", "ATLAS_PACKAGE_DATA"]:
        config = config.replace("${" + name + "}", env[name])
    config = re.sub(r"^(\s*- src: )(.+)$", lambda match: match[1] + json.dumps(match[2]), config, flags=re.M)
    resolved_config = data_dir / "nfpm.yaml"
    resolved_config.write_text(config)
    for kind, filename in [("deb", f"atlas_{version}_amd64.deb"),
                            ("rpm", f"atlas-{version}.x86_64.rpm"),
                            ("archlinux", f"atlas-{version}-x86_64.pkg.tar.zst")]:
        run(tool_dir / "nfpm", "package", "--config", resolved_config,
            "--packager", kind, "--target", output / filename, env=env, cwd=ROOT)
    name = f"atlas-{version}-linux-x86_64"
    stage = data_dir / name
    if stage.exists():
        shutil.rmtree(stage)
    destinations = {
        binary: "bin/atlas", ROOT / "packaging/linux/atlas.desktop": "share/applications/atlas.desktop",
        icon: "share/icons/hicolor/512x512/apps/atlas.png",
        data_dir / f"{APP_ID}.metainfo.xml": f"share/metainfo/{APP_ID}.metainfo.xml",
        ROOT / "CHANGELOG.md": "share/doc/atlas/CHANGELOG.md",
        ROOT / "LICENSE": "share/licenses/atlas/LICENSE",
        ROOT / "legal/THIRD-PARTY-NOTICES.txt": "share/doc/atlas/THIRD-PARTY-NOTICES.txt",
    }
    destinations.update({companion: f"bin/{companion.name}" for companion in companions})
    for source, relative in destinations.items():
        destination = stage / relative
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(source, destination)
    with tarfile.open(output / (name + ".tar.gz"), "w:gz") as archive:
        archive.add(stage, arcname=name)
    appdir = data_dir / "AppDir"
    if appdir.exists():
        shutil.rmtree(appdir)
    shutil.copytree(stage, appdir / "usr")
    env.update(APPIMAGE_EXTRACT_AND_RUN="1", ARCH="x86_64", OUTPUT=f"Atlas-{version}-x86_64.AppImage")
    owner, project = repo.split("/")
    env["UPDATE_INFORMATION"] = f"gh-releases-zsync|{owner}|{project}|latest|Atlas-*-x86_64.AppImage.zsync"
    # Dependencies are collected by linuxdeploy; it intentionally excludes glibc.
    run(tool_dir / "linuxdeploy", "--appdir", appdir, "--executable", binary,
        "--desktop-file", ROOT / "packaging/linux/atlas.desktop", "--icon-file", icon,
        "--output", "appimage", env=env, cwd=output)
    tarball = output / (name + ".tar.gz")
    aur = data_dir / "aur"
    aur.mkdir(exist_ok=True)
    pkgver = version.replace("-", "_")
    dependencies = ["glibc", "gcc-libs", "openssl", "libxkbcommon", "libxkbcommon-x11", "libxcb", "fontconfig", "wayland", "vulkan-icd-loader", "libglvnd", "ttf-dejavu"]
    # This recipe only unpacks a pinned, checksummed upstream artifact.
    recipe = f'''# Maintained upstream; generated for Atlas {version}.
pkgname=atlas-bin
pkgver={pkgver}
pkgrel=1
pkgdesc='Modern file manager built with Rust and GPUI'
arch=('x86_64')
url='https://atlasnative.com'
license=('AGPL-3.0-or-later')
depends=({' '.join(repr(d) for d in dependencies)})
provides=('atlas')
conflicts=('atlas')
options=('!strip')
source=('{info["release_url"].replace("/tag/", "/download/")}/{tarball.name}')
sha256sums=('{sha256(tarball)}')
package() {{
    for binary in atlas atlas-search-service atlas-cloud-extension atlas-icon-extension; do
        install -Dm755 "$srcdir/{name}/bin/$binary" "$pkgdir/usr/bin/$binary"
    done
    mkdir -p "$pkgdir/usr/share"
    cp -a "$srcdir/{name}/share/." "$pkgdir/usr/share/"
}}
'''
    (aur / "PKGBUILD").write_text(recipe)
    srcinfo = ["pkgbase = atlas-bin", f"\tpkgdesc = Modern file manager built with Rust and GPUI", f"\tpkgver = {pkgver}", "\tpkgrel = 1", "\turl = https://atlasnative.com", "\tarch = x86_64", "\tlicense = AGPL-3.0-or-later"]
    srcinfo.extend(f"\tdepends = {d}" for d in dependencies)
    srcinfo.extend(["\tprovides = atlas", "\tconflicts = atlas", "\toptions = !strip", f"\tsource = {info['release_url'].replace('/tag/', '/download/')}/{tarball.name}", f"\tsha256sums = {sha256(tarball)}", "", "pkgname = atlas-bin"])
    (aur / ".SRCINFO").write_text("\n".join(srcinfo) + "\n")
    with tarfile.open(output / f"atlas-{version}-aur.tar.gz", "w:gz") as archive:
        archive.add(aur, arcname="atlas-bin")
    shutil.copy2(data_dir / "release-notes.md", output / "release-notes.md")
    manifest = dict(info, arch="x86_64", assets={p.name: dict(sha256=sha256(p), size=p.stat().st_size) for p in output.iterdir() if p.is_file()})
    (output / "atlas-linux.json").write_text(json.dumps(manifest, indent=2) + "\n")
    shutil.copy2(ROOT / "scripts/install-linux.sh", output / "install-linux.sh")
    write_checksums(output)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    tool_parser = commands.add_parser("tools")
    tool_parser.add_argument("--output", type=Path, default=ROOT / "target/linux-tools")
    for command in ["metadata", "packages"]:
        sub = commands.add_parser(command)
        sub.add_argument("--version", required=True)
        sub.add_argument("--repository", default=os.environ.get("GITHUB_REPOSITORY", "neondeex/atlas-filemanager"))
        sub.add_argument("--data-dir", type=Path, default=ROOT / "target/linux-release")
        if command == "packages":
            sub.add_argument("--binary", type=Path, default=ROOT / "target/release/atlas")
            sub.add_argument("--output", type=Path, default=ROOT / "target/linux-dist")
            sub.add_argument("--tool-dir", type=Path, default=ROOT / "target/linux-tools")
    args = parser.parse_args()
    if args.command == "tools":
        tools(args.output.resolve())
    elif args.command == "metadata":
        metadata(args.version, args.repository, args.data_dir.resolve())
    else:
        packages(args.version, args.repository, args.binary.resolve(), args.output.resolve(), args.tool_dir.resolve(), args.data_dir.resolve())


if __name__ == "__main__":
    main()
