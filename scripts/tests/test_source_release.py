import importlib.util
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import zipfile
import sys

SCRIPTS = Path(__file__).parents[1]
sys.path.insert(0, str(SCRIPTS))


def load(name, filename):
    spec = importlib.util.spec_from_file_location(name, SCRIPTS / filename)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


source = load('source_release', 'build-source-release.py')
verify = load('source_verify', 'verify-source-release.py')


class SourceSnapshotTests(unittest.TestCase):
    def test_snapshot_contains_linux_packaging_excludes_private_files_and_detects_staleness(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for name in source.ROOT_FILES:
                (root / name).write_text('fixture source\n')
            for name, text in {
                '.cargo/config.toml': '[build]\n',
                'target/source-vendor-config.toml': '[source.vendored-sources]\ndirectory = "fixture"\n',
                'target/source-release-1.2.3-rc.1/vendor-registry/example/LICENSE': 'upstream notice',
                'packaging/linux/atlas.desktop': '[Desktop Entry]\nName=Atlas\n',
                'scripts/linux_release.py': '# packaging helper',
                'src/main.rs': 'fn main() {}',
                'src/.env': 'PRIVATE_CONFIGURATION',
                'src/signing.pfx': 'PRIVATE_KEY',
            }.items():
                path = root / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(text)
            with patch.object(source, 'ROOT', root), patch.object(verify, 'ROOT', root):
                source.build('1.2.3-rc.1', 'https://example.com/source')
                verify.verify('1.2.3-rc.1')
                with zipfile.ZipFile(root / 'target/source/atlas-1.2.3-rc.1-source.zip') as bundle:
                    names = bundle.namelist()
                    self.assertIn('packaging/linux/atlas.desktop', names)
                    self.assertIn('scripts/linux_release.py', names)
                    self.assertNotIn('src/.env', names)
                    self.assertNotIn('src/signing.pfx', names)
                (root / 'src/main.rs').write_text('fn main() { panic!("changed"); }')
                with self.assertRaisesRegex(SystemExit, 'stale'):
                    verify.verify('1.2.3-rc.1')


if __name__ == '__main__':
    unittest.main()
