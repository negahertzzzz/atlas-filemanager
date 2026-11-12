import hashlib
import importlib.util
import os
from pathlib import Path
import tarfile
import tempfile
import unittest
from unittest.mock import patch
import zipfile

spec = importlib.util.spec_from_file_location('ci_archive', Path(__file__).parents[1] / 'stage-ci-build.py')
archive = importlib.util.module_from_spec(spec)
spec.loader.exec_module(archive)


class DevelopmentArchiveTests(unittest.TestCase):
    def test_archive_retains_companions_notices_and_checksum(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            suffix = '.exe' if os.name == 'nt' else ''
            binaries = root / 'target/debug'
            binaries.mkdir(parents=True)
            for name in ('atlas', 'atlas-search-service', 'atlas-cloud-extension', 'atlas-icon-extension'):
                p = binaries / (name + suffix)
                p.write_bytes(b'fixture binary')
                p.chmod(0o755)
            (root / 'LICENSE').write_text('fixture license')
            (root / 'legal').mkdir()
            (root / 'legal/THIRD-PARTY-NOTICES.txt').write_text('fixture notices')
            if os.name == 'nt':
                unrar = root / 'crates/atlas-core/vendor/unrar-bin/UnRAR.exe'
                unrar.parent.mkdir(parents=True)
                unrar.write_bytes(b'fixture tool')
                runtime = root / 'target/windows-runtime'
                runtime.mkdir()
                (runtime / 'vcruntime140.dll').write_bytes(b'fixture runtime')
            with patch.object(archive, 'ROOT', root):
                archive.stage('debug', root / 'output')
            bundle = next(p for p in (root / 'output').iterdir() if not p.name.endswith('.sha256'))
            expected = hashlib.sha256(bundle.read_bytes()).hexdigest()
            self.assertEqual((bundle.parent / (bundle.name + '.sha256')).read_text(), f'{expected}  {bundle.name}\n')
            if os.name == 'nt':
                with zipfile.ZipFile(bundle) as z:
                    names = z.namelist()
            else:
                with tarfile.open(bundle) as z:
                    names = z.getnames()
                    binary = next(p for p in z.getmembers() if p.name.endswith('/atlas'))
                    self.assertTrue(binary.mode & 0o111)
            for name in ('atlas' + suffix, 'atlas-cloud-extension' + suffix, 'README.txt', 'BUILD.json', 'LICENSE.txt'):
                self.assertTrue(any(p.endswith('/' + name) for p in names), name)

    def test_missing_binary_is_not_a_successful_download(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            with patch.object(archive, 'ROOT', root), self.assertRaises(FileNotFoundError):
                archive.stage('debug', root / 'output')
            self.assertEqual(list((root / 'output').iterdir()), [])


if __name__ == '__main__':
    unittest.main()
