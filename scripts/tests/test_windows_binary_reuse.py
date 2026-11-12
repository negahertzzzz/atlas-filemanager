import hashlib
import importlib.util
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('reuse', Path(__file__).parents[1] / 'restore-windows-binaries.py')
reuse = importlib.util.module_from_spec(spec)
spec.loader.exec_module(reuse)


class CandidateReuseTests(unittest.TestCase):
    def exercise(self, changed=(), corrupt=False, edition='full', event='workflow_dispatch'):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            archive = root / 'symbols'
            archive.mkdir()
            entries = []
            for name in reuse.NAMES:
                executable, pdb = name + '.exe', name.replace('-', '_') + '.pdb'
                (archive / executable).write_bytes(b'built binary')
                (archive / pdb).write_bytes(b'paired symbols')
                entries.append(dict(exe=executable, pdb=pdb, sha256=hashlib.sha256(b'built binary').hexdigest()))
            (archive / 'manifest.json').write_text(json.dumps(dict(version='1.2.3-installer', binaries=entries)))
            if corrupt:
                (archive / 'atlas.exe').write_bytes(b'tampered')

            def command(*args):
                if args[:2] == ('git', 'fetch'):
                    return ''
                if args[:2] == ('git', 'diff'):
                    return '\n'.join(changed)
                url = args[2]
                if url.endswith('/runs/7'):
                    return json.dumps(dict(head_sha='a' * 40, event=event, head_branch='main',
                                           head_repository=dict(full_name='owner/repo'), path='.github/workflows/packages.yml'))
                if url.endswith('/jobs'):
                    return json.dumps(dict(jobs=[dict(id=8, name='windows / EXE installer and MSIX x64', steps=[
                        dict(name='Build optimized binaries once', conclusion='success')])]))
                return f'2026-01-01T00:00:00Z   ATLAS_VERSION: 1.2.3\n2026-01-01T00:00:00Z   ATLAS_EDITION: {edition}\n'

            env = dict(GITHUB_REPOSITORY='owner/repo', GITHUB_SHA='b' * 40, ATLAS_VERSION='1.2.3', ATLAS_EDITION='full')
            with patch.dict(os.environ, env), patch.object(reuse, 'ROOT', root), patch.object(reuse, 'command', side_effect=command):
                reuse.restore('7', archive)
            self.assertEqual((root / 'target/release/atlas.exe').read_bytes(), b'built binary')
            self.assertEqual(json.loads((root / 'target/reused-binaries.json').read_text())['sourceRevision'], 'a' * 40)

    def test_packaging_and_search_only_changes_preserve_verified_application_binaries(self):
        self.exercise(changed=['scripts/build-windows-msix.ps1', 'crates/atlas-search-service/src/main.rs'])

    def test_changed_application_inputs_require_a_full_build(self):
        for path in ['crates/atlas-ui/src/app/startup.rs', 'Cargo.lock', 'scripts/winfsp-build.rs', 'packaging/icons/atlas.ico']:
            with self.subTest(path=path), self.assertRaisesRegex(ValueError, 'full build'):
                self.exercise(changed=[path])

    def test_tampered_binary_is_rejected(self):
        with self.assertRaisesRegex(ValueError, 'checksum'):
            self.exercise(corrupt=True)

    def test_different_edition_and_pull_request_artifacts_are_rejected(self):
        with self.assertRaisesRegex(ValueError, 'ATLAS_EDITION'):
            self.exercise(edition='free')
        with self.assertRaisesRegex(ValueError, 'manually built'):
            self.exercise(event='pull_request')


if __name__ == '__main__':
    unittest.main()
