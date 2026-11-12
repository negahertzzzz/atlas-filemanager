import json
import importlib.util
import io
import os
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import unittest
from PIL import Image
import xml.etree.ElementTree as ET
from unittest.mock import patch

SCRIPTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS))
import linux_release as release
import linux_repository as repository
local_spec = importlib.util.spec_from_file_location("linux_local_tests", SCRIPTS / "test-linux-local.py")
local_tests = importlib.util.module_from_spec(local_spec)
local_spec.loader.exec_module(local_tests)


class ReleaseMetadataTests(unittest.TestCase):
    def test_versions_cannot_inject_paths_or_shell_commands(self):
        for version in ["0.1.0", "1.12.0", "2.0.0-rc.1"]:
            self.assertEqual(release.validate_version(version), version)
        for version in ["v1.0.0", "01.0.0", "1.0", "1.0.0-01", "1.0.0-..", "../1.0.0", "1.0.0;id", "1.0.0\n"]:
            with self.subTest(version=version), self.assertRaises(ValueError):
                release.validate_version(version)

    def test_notes_require_exact_version_and_keep_section_boundaries(self):
        changelog = "## [Unreleased]\n\n- Future\n\n## [1.0.0] - 2026-01-01\n\n### Fixed\n\n- A & B\n\n## [0.9.0] - 2025-12-01\n\n- Older\n"
        self.assertEqual(release.release_notes(changelog, "1.0.0"), ("2026-01-01", "### Fixed\n\n- A & B"))
        for text in [changelog + "\n## [1.0.0] - 2026-01-01\n- Duplicate", "## [1.0.0]\n- No date", "## [1.0.0] - 2026-99-99\n- Bad date", "## [1.0.0] - 2026-01-01\n"]:
            with self.assertRaises(ValueError):
                release.release_notes(text, "1.0.0")

    def test_appstream_escapes_notes_and_points_to_the_actual_repository(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "CHANGELOG.md").write_text("## [1.0.0-rc.1] - 2026-01-01\n\n- A & <B>\n")
            with patch.object(release, "ROOT", root):
                info = release.metadata("1.0.0-rc.1", "owner/project", root / "output")
            self.assertTrue(info["prerelease"])
            tree = ET.parse(root / "output" / (release.APP_ID + ".metainfo.xml"))
            self.assertEqual(tree.findtext("releases/release/description/ul/li"), "A & <B>")
            self.assertEqual(tree.findtext("url[@type='bugtracker']"), "https://github.com/owner/project/issues")

    def test_checksums_detect_tampered_packages(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory)
            package = output / "atlas.deb"
            package.write_bytes(b"original")
            release.write_checksums(output)
            repository.verify_assets(output)
            package.write_bytes(b"tampered")
            with self.assertRaises(ValueError):
                repository.verify_assets(output)

    def test_stable_repository_rejects_prereleases_before_signing(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "atlas-linux.json").write_text(json.dumps({"version": "1.0.0-rc.1", "prerelease": True}))
            with self.assertRaisesRegex(ValueError, "Prereleases"):
                repository.build(root, root / "output", "https://example.com/atlas", "A" * 40)

    def test_old_artifacts_cannot_silently_enter_a_new_release(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory)
            (output / "old.deb").write_bytes(b"old package")
            with self.assertRaisesRegex(ValueError, "must be empty"):
                release.packages("1.0.0", "owner/repo", output / "binary", output, output, output)

    def test_distributable_payload_includes_extensions_and_notices(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binaries = root / "release"
            binaries.mkdir()
            for name in ("atlas", "atlas-search-service", "atlas-cloud-extension", "atlas-icon-extension"):
                binary = binaries / name
                binary.write_bytes(b"built executable")
                binary.chmod(0o755)
            output = root / "dist"

            def packaging_tool(*args, **kwargs):
                args = [str(arg) for arg in args]
                if args[0] == 'rsvg-convert':
                    subprocess.run(args, check=True)
                elif "--target" in args:
                    Path(args[args.index("--target") + 1]).write_bytes(b"native package")
                else:
                    (output / kwargs["env"]["OUTPUT"]).write_bytes(b"AppImage")

            with patch.object(release, "run", side_effect=packaging_tool):
                release.packages("0.1.5", "owner/repo", binaries / "atlas", output,
                                 root / "tools", root / "data")
            with tarfile.open(output / "atlas-0.1.5-linux-x86_64.tar.gz") as archive:
                for name in ("atlas", "atlas-search-service", "atlas-cloud-extension", "atlas-icon-extension"):
                    member = archive.getmember(f"atlas-0.1.5-linux-x86_64/bin/{name}")
                    self.assertTrue(member.mode & 0o111, name)
                notice = archive.extractfile("atlas-0.1.5-linux-x86_64/share/doc/atlas/THIRD-PARTY-NOTICES.txt")
                self.assertEqual(notice.read(), (release.ROOT / "legal/THIRD-PARTY-NOTICES.txt").read_bytes())
                logo = archive.extractfile("atlas-0.1.5-linux-x86_64/share/icons/hicolor/512x512/apps/atlas.png")
                with Image.open(io.BytesIO(logo.read())) as image:
                    self.assertEqual(image.size, (512, 512))
                    self.assertGreater(image.convert('RGBA').getchannel('A').getextrema()[1], 0,
                                       'The installed logo must contain visible pixels')


class InstallerTests(unittest.TestCase):
    def run_installer(self, os_release, *args, env=None):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "os-release"
            path.write_text(os_release)
            return subprocess.run(["bash", str(SCRIPTS / "install-linux.sh"), *args],
                                  env=dict(os.environ, ATLAS_OS_RELEASE=str(path), **(env or {})),
                                  capture_output=True, text=True)

    def test_distribution_detection_without_mutating_the_system(self):
        for identity, manager in [("ubuntu", "apt"), ("debian", "apt"), ("linuxmint", "apt"), ("fedora", "dnf"), ("opensuse-tumbleweed", "zypper"), ("arch", "pacman"), ("endeavouros", "pacman")]:
            with self.subTest(identity=identity):
                result = self.run_installer(f'ID={identity}\n', "--dry-run")
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn(f"manager: {manager}", result.stdout)
                self.assertIn("No system changes", result.stdout)
        result = self.run_installer('ID=custom\nID_LIKE="ubuntu debian"\n', "--dry-run")
        self.assertIn("manager: apt", result.stdout)
        self.assertNotEqual(self.run_installer("ID=alpine\n", "--dry-run").returncode, 0)

    def test_missing_trust_configuration_fails_before_installation(self):
        result = self.run_installer("ID=debian\n", "--repo-url", "https://example.com/atlas")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("fingerprint", result.stderr)

    def test_mismatched_key_never_invokes_the_package_manager(self):
        with tempfile.TemporaryDirectory() as directory:
            tools = Path(directory)
            marker = tools / "manager-ran"
            mocks = {
                "curl": '#!/bin/bash\nwhile (($#)); do if [[ $1 == -o ]]; then echo public-key > "$2"; exit 0; fi; shift; done\n',
                "gpg": '#!/bin/bash\necho "fpr:::::::::BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB:"\n',
                "apt": f'#!/bin/bash\ntouch "{marker}"\n',
            }
            for name, content in mocks.items():
                (tools / name).write_text(content)
                (tools / name).chmod(0o755)
            result = self.run_installer("ID=debian\n", "--repo-url", "https://example.com/atlas", "--fingerprint", "A" * 40,
                                        env={"PATH": str(tools) + ":" + os.environ["PATH"]})
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("does not match", result.stderr)
            self.assertFalse(marker.exists())


class LocalMatrixTests(unittest.TestCase):
    def test_portable_artifact_tampering_is_rejected_before_any_container_runs(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory)
            (output / "atlas-linux.json").write_text(json.dumps({"version": "0.1.0", "arch": "x86_64"}))
            portable = output / "Atlas.AppImage"
            portable.write_bytes(b"original binary")
            release.write_checksums(output)
            self.assertEqual(local_tests.verify_artifacts(output), "0.1.0")
            portable.write_bytes(b"modified binary")
            with self.assertRaisesRegex(ValueError, "checksum"):
                local_tests.verify_artifacts(output)
            (output / "SHA256SUMS").write_text("")
            with self.assertRaisesRegex(ValueError, "do not cover"):
                local_tests.verify_artifacts(output)

    def test_checksum_paths_cannot_escape_the_artifact_directory(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            output = root / "artifacts"
            output.mkdir()
            outside = root / "outside.bin"
            outside.write_bytes(b"outside")
            (output / "SHA256SUMS").write_text(f"{release.sha256(outside)}  ../outside.bin\n")
            with self.assertRaisesRegex(ValueError, "checksum"):
                local_tests.verify_artifacts(output)


if __name__ == "__main__":
    unittest.main()
