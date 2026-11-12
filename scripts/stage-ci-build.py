"""Archive explicitly selected development binaries from a successful CI job."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import tarfile
import tempfile
import shutil
import zipfile

ROOT = Path(__file__).resolve().parents[1]


def stage(profile, output):
    windows = os.name == "nt"
    system = "windows" if windows else "linux"
    suffix = ".exe" if windows else ""
    revision = os.environ.get("GITHUB_SHA", "local")
    name = f"atlas-{system}-{platform.machine().lower()}-{profile}-{revision[:12]}"
    output.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory() as temporary:
        payload = Path(temporary) / name
        payload.mkdir()
        for binary in ("atlas", "atlas-search-service", "atlas-cloud-extension", "atlas-icon-extension"):
            source = ROOT / "target" / profile / (binary + suffix)
            if not source.is_file():
                raise FileNotFoundError(f"Missing built companion: {source}")
            shutil.copy2(source, payload / source.name)
        for source, target in [(ROOT / "LICENSE", "LICENSE.txt"),
                               (ROOT / "legal/THIRD-PARTY-NOTICES.txt", "THIRD-PARTY-NOTICES.txt")]:
            shutil.copy2(source, payload / target)
        if windows:
            shutil.copy2(ROOT / "crates/atlas-core/vendor/unrar-bin/UnRAR.exe", payload / "UnRAR.exe")
            runtime = ROOT / 'target/windows-runtime'
            if not (runtime / 'vcruntime140.dll').is_file():
                raise FileNotFoundError('Stage the Windows redistributable runtime before archiving')
            for path in runtime.iterdir():
                if path.is_file():
                    shutil.copy2(path, payload / path.name)
        repo = os.environ.get("GITHUB_REPOSITORY", "neondeex/atlas-filemanager")
        (payload / "BUILD.json").write_text(json.dumps(dict(profile=profile, revision=revision,
            platform=system, architecture=platform.machine()), indent=2) + "\n", encoding="utf-8")
        (payload / "README.txt").write_text(f"""Atlas development build ({profile})
Commit: {revision}
Source: https://github.com/{repo}/tree/{revision}

Extract the entire archive and run {'atlas.exe' if windows else './atlas'}.
This build is intended for testing. The optional cloud backend also needs
rclone; installable packages with the bundled backend are produced by the
Packages workflow. Optimized installers are separate from these CI binaries.
""", encoding="utf-8")
        archive = output / (name + (".zip" if windows else ".tar.gz"))
        if windows:
            with zipfile.ZipFile(archive, "w", compression=zipfile.ZIP_DEFLATED) as bundle:
                for path in sorted(payload.iterdir()):
                    bundle.write(path, f"{name}/{path.name}")
        else:
            with tarfile.open(archive, "w:gz") as bundle:
                bundle.add(payload, arcname=name)
        digest = hashlib.sha256()
        with archive.open("rb") as stream:
            for chunk in iter(lambda: stream.read(1024 * 1024), b""):
                digest.update(chunk)
        (output / (archive.name + ".sha256")).write_text(f"{digest.hexdigest()}  {archive.name}\n", encoding="utf-8")
        print(archive)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--profile", choices=("debug", "release"), default="debug")
    parser.add_argument("--output", type=Path, default=ROOT / "target/ci-dist")
    args = parser.parse_args()
    stage(args.profile, args.output.resolve())
