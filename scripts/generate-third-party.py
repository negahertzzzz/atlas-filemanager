"""Collect notices from the locked Cargo graph and installed web dependencies.

Never substitutes a generic SPDX text for a dependency's own copyright notice.
Missing notices and copyleft source obligations are recorded in the audit.
Run from any directory. --check compares generated output without writing it.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import re
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
LEGAL = ROOT / "legal"
NOTICE_NAME = re.compile(r"^(licen[sc]e|copying|copyright|notice|unlicense)([._-]|$)", re.I)
SKIP = {".git", "target", "node_modules", "tests", "test", "examples", "benches"}


def read(path: Path) -> str:
    return path.read_bytes().decode("utf-8-sig", errors="replace").replace("\r\n", "\n").strip()


def text_hash_matches(path: Path, expected: str) -> bool:
    """Accept Git's LF/CRLF conversion, including legacy Windows receipts."""
    raw = path.read_bytes()
    lf = raw.replace(b"\r\n", b"\n")
    return any(hashlib.sha256(data).hexdigest() == expected
               for data in (raw, lf, lf.replace(b"\n", b"\r\n")))


def own_notices(directory: Path) -> list[Path]:
    files = []
    for path in sorted(directory.iterdir()):
        if path.is_file() and NOTICE_NAME.match(path.name):
            files.append(path)
        elif path.is_dir() and path.name.lower() in {"licenses", "licences", "license"}:
            files.extend(p for p in sorted(path.rglob("*")) if p.is_file() and p.suffix.lower() in {".txt", ".md", "", ".html"})
    return files


def cargo_notices(package: dict) -> list[Path]:
    directory = Path(package["manifest_path"]).parent
    files = own_notices(directory)
    if package.get("license_file"):
        explicit = directory / package["license_file"]
        if explicit.is_file() and explicit not in files:
            files.append(explicit)
    # Multi-crate git/workspace repos commonly keep the license at their root.
    if not files and ((package.get("source", "") or "").startswith("git+") or directory.is_relative_to(ROOT)):
        for parent in directory.parents:
            if parent == ROOT.parent or parent.name in {"checkouts", "src"}:
                break
            files = own_notices(parent)
            if files:
                break
    return files


def cargo_metadata(all_features: bool = False) -> dict:
    command = ["cargo", "metadata", "--locked", "--offline", "--format-version", "1", "--filter-platform", "x86_64-pc-windows-msvc"]
    if all_features:
        command.append("--all-features")
    result = subprocess.run(command, cwd=ROOT, check=True, capture_output=True, encoding="utf-8")
    return json.loads(result.stdout)


def closure(metadata: dict, roots: set[str], kinds: set[str | None]) -> set[str]:
    nodes = {node["id"]: node for node in metadata["resolve"]["nodes"]}
    visited = set()
    pending = list(roots)
    while pending:
        ident = pending.pop()
        if ident in visited:
            continue
        visited.add(ident)
        for dependency in nodes.get(ident, {}).get("deps", []):
            if any(kind["kind"] in kinds for kind in dependency["dep_kinds"]):
                pending.append(dependency["pkg"])
    return visited


def web_packages() -> list[tuple[str, Path, dict]]:
    result = []
    for site in ("website", "website-edge"):
        lock_path = ROOT / site / "package-lock.json"
        if not lock_path.is_file():
            continue
        lock = json.loads(read(lock_path))
        for location, package in sorted(lock.get("packages", {}).items()):
            if not location or not package.get("version"):
                continue
            directory = ROOT / site / location
            manifest = directory / "package.json"
            if not manifest.is_file():
                # Some optional packages are for other operating systems.
                if package.get("optional"):
                    continue
                raise RuntimeError(f"Install locked web dependencies first: {site}/{location}")
            data = json.loads(read(manifest))
            data["_build_only"] = bool(package.get("dev"))
            result.append((site, directory, data))
    return result


def generate() -> tuple[dict[Path, str], dict]:
    default = cargo_metadata()
    audit = cargo_metadata(all_features=True)
    release_roots = {p["id"] for p in default["packages"] if p["name"] in {"atlas", "atlas-search-service", "atlas-cloud-extension", "atlas-icon-extension"}}
    runtime = closure(default, release_roots, {None})
    build = closure(default, release_roots, {None, "build"})
    documents: dict[str, str] = {}
    entries = []
    missing = []
    overrides_path = LEGAL / "notice-overrides.json"
    overrides = json.loads(read(overrides_path)) if overrides_path.is_file() else {}

    def record(name: str, version: str, ecosystem: str, license_expression: str | None,
               origin: str | None, scope: str, files: list[Path], review: str = "") -> None:
        override = overrides.get(f"{ecosystem}:{name}@{version}", {})
        for item in override.get("notices", []):
            path = ROOT / item["path"]
            if item.get("sha256") and hashlib.sha256(path.read_bytes()).hexdigest() != item["sha256"]:
                raise RuntimeError(f"Upstream evidence changed: {item['path']}")
        files = files + [ROOT / item["path"] for item in override.get("notices", [])]
        if override.get("license"):
            license_expression = override["license"]
        refs = []
        for path in files:
            content = read(path)
            if not content:
                continue
            digest = hashlib.sha256(content.encode()).hexdigest()
            documents[digest] = content
            refs.append({"file": path.name, "text": digest})
        entry = {"name": name, "version": version, "ecosystem": ecosystem,
                 "declaredLicense": license_expression, "source": origin,
                 "scope": scope, "notices": refs}
        if review:
            entry["review"] = review
        if override:
            entry["upstreamEvidence"] = override
        if not refs:
            missing.append(f"{ecosystem}: {name}@{version}")
        entries.append(entry)

    for package in sorted(audit["packages"], key=lambda p: (p["name"], p["version"], p["id"])):
        ident = package["id"]
        scope = "windows-release" if ident in runtime else "windows-build" if ident in build else "development-or-optional"
        expression = package.get("license")
        files = cargo_notices(package)
        review = ""
        if not expression:
            review = "No SPDX expression in crate metadata; inspect the upstream workspace license."
        if expression and any(lic in expression for lic in ("GPL", "MPL")):
            review = "Copyleft: notices alone do not satisfy corresponding-source obligations. See legal/RELEASE-CHECKLIST.md."
        origin = package.get("repository") or package.get("source")
        record(package["name"], package["version"], "cargo", expression, origin, scope, files, review)

    for site, directory, package in web_packages():
        license_value = package.get("license")
        if isinstance(license_value, dict):
            license_value = license_value.get("type")
        repository = package.get("repository")
        if isinstance(repository, dict):
            repository = repository.get("url")
        record(package.get("name", directory.name), package["version"], site, license_value,
               repository, "web-build" if package["_build_only"] else "web-dependency", own_notices(directory))

    assets = json.loads(read(LEGAL / "assets.json"))
    for asset in assets:
        files = [ROOT / path for path in asset["notices"]]
        if any(not path.is_file() for path in files):
            raise RuntimeError(f"Missing resource notice for {asset['name']}")
        for item in asset.get("files", []):
            if hashlib.sha256((ROOT / item["path"]).read_bytes()).hexdigest() != item["sha256"]:
                raise RuntimeError(f"Bundled resource changed: {item['path']}; review its license and regenerate the asset inventory.")
        record(asset["name"], asset["version"], "resource", asset["license"], asset["source"],
               asset["scope"], files, asset.get("review", ""))

    entries.sort(key=lambda p: (p["ecosystem"], p["name"], p["version"]))
    report = {"schemaVersion": 1, "target": "x86_64-pc-windows-msvc",
              "cargoLockSha256": hashlib.sha256((ROOT / "Cargo.lock").read_bytes()).hexdigest(),
              "coverage": "All-feature workspace metadata, default Windows release graph, installed locked web packages, and explicit bundled resources. Build/development entries may not be shipped.",
              "packages": entries, "missingNotices": missing,
              "textCount": len(documents)}
    report["inputHashes"] = {path.relative_to(ROOT).as_posix(): hashlib.sha256(path.read_bytes()).hexdigest()
                            for path in [ROOT / "Cargo.lock", ROOT / "website/package-lock.json", LEGAL / "assets.json", LEGAL / "notice-overrides.json"] if path.is_file()}
    lines = ["ATLAS FILE MANAGER — THIRD-PARTY NOTICES", "",
             "This file preserves notices supplied by upstream authors. Each component remains",
             "under its own license. The inventory includes build tools and optional dependencies;",
             "their presence does not imply that every component is included in every binary.",
             "Atlas workspace code is identified as AGPL-3.0-or-later in its manifests.",
             "These notices do not replace source-delivery obligations under copyleft licenses.",
             "Contact: support@modhyt.org", "", "COMPONENT INDEX", ""]
    for entry in entries:
        lines.extend([f"{entry['name']} {entry['version']} [{entry['ecosystem']}; {entry['scope']}]",
                      f"Declared license: {entry['declaredLicense'] or 'See upstream notice'}",
                      f"Source: {entry['source'] or 'Atlas source distribution'}"])
        if entry.get("review"):
            lines.append(entry["review"])
        lines.extend(f"Notice: {notice['file']} — {notice['text']}" for notice in entry["notices"])
        lines.append("")
    lines.extend(["LICENSE AND COPYRIGHT TEXTS", ""])
    for digest, content in sorted(documents.items()):
        lines.extend(["=" * 78, f"TEXT {digest}", "=" * 78, content, ""])
    notices = "\n".join(lines) + "\n"
    outputs = {LEGAL / "THIRD-PARTY-NOTICES.txt": notices,
               LEGAL / "dependency-inventory.json": json.dumps(report, ensure_ascii=False, indent=2) + "\n",
               ROOT / "packaging/windows/msix/THIRD-PARTY-NOTICES.txt": notices,
               ROOT / "website/public/legal/THIRD-PARTY-NOTICES.txt": notices,
               ROOT / "website-edge/public/legal/THIRD-PARTY-NOTICES.txt": notices}
    return outputs, report


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true")
    parser.add_argument("--allow-missing", action="store_true", help="Audit only; never use for release packaging.")
    parser.add_argument("--verify-release", action="store_true", help="Verify committed inventory inputs and distributed copies without requiring installed npm packages.")
    parser.add_argument("--scope", choices=("all", "desktop"), default="all",
                        help="Desktop packaging verifies desktop inputs and copies; web publication still uses all.")
    args = parser.parse_args()
    if args.verify_release:
        report = json.loads(read(LEGAL / "dependency-inventory.json"))
        for file, digest in report["inputHashes"].items():
            if args.scope == "desktop" and file.startswith(("website/", "website-edge/")):
                continue
            if not text_hash_matches(ROOT / file, digest):
                raise SystemExit(f"License inventory is stale: {file}. Regenerate before packaging.")
        for override in json.loads(read(LEGAL / "notice-overrides.json")).values():
            for item in override.get("notices", []):
                if item.get("sha256") and not text_hash_matches(ROOT / item["path"], item["sha256"]):
                    raise SystemExit(f"License evidence changed: {item['path']}")
        for asset in json.loads(read(LEGAL / "assets.json")):
            for item in asset.get("files", []):
                if args.scope == "desktop" and item["path"].startswith(("website/", "website-edge/")):
                    continue
                resource = ROOT / item["path"]
                matches = (text_hash_matches(resource, item["sha256"]) if resource.suffix.lower() == ".svg"
                           else hashlib.sha256(resource.read_bytes()).hexdigest() == item["sha256"])
                if not matches:
                    raise SystemExit(f"Resource changed: {item['path']}")
        expected = (LEGAL / "THIRD-PARTY-NOTICES.txt").read_bytes()
        copies = ["packaging/windows/msix/THIRD-PARTY-NOTICES.txt"]
        if args.scope == "all":
            copies += ["website/public/legal/THIRD-PARTY-NOTICES.txt", "website-edge/public/legal/THIRD-PARTY-NOTICES.txt"]
        for file in copies:
            if (ROOT / file).read_bytes().replace(b"\r\n", b"\n") != expected.replace(b"\r\n", b"\n"):
                raise SystemExit(f"Distributed notices are stale: {file}")
        if report["missingNotices"]:
            raise SystemExit("Missing license notices block packaging.")
        print(f"Release notice checks passed for {len(report['packages'])} components.")
        return
    outputs, report = generate()
    stale = []
    for path, content in outputs.items():
        if args.check:
            if not path.is_file() or path.read_text(encoding="utf-8") != content:
                stale.append(str(path.relative_to(ROOT)))
        else:
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(content, encoding="utf-8", newline="\n")
    print(f"Audited {len(report['packages'])} components; {report['textCount']} unique notice texts; {len(report['missingNotices'])} missing notices.")
    for missing in report["missingNotices"]:
        print(f"MISSING {missing}")
    for path in stale:
        print(f"STALE {path}")
    if stale or (report["missingNotices"] and not args.allow_missing):
        raise SystemExit(1)


if __name__ == "__main__":
    main()
