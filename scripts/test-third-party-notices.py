"""Regression checks for license evidence collection (no external services)."""
import importlib.util
import tempfile
import unittest
import hashlib
from pathlib import Path

spec = importlib.util.spec_from_file_location("notices", Path(__file__).with_name("generate-third-party.py"))
notices = importlib.util.module_from_spec(spec)
spec.loader.exec_module(notices)


class NoticeCollectionTests(unittest.TestCase):
    def test_git_line_endings_preserve_evidence_but_content_edits_do_not(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "NOTICE.txt"
            original = b"Copyright Example\r\nLicense text\r\n"
            expected = hashlib.sha256(original).hexdigest()
            path.write_bytes(original.replace(b"\r\n", b"\n"))
            self.assertTrue(notices.text_hash_matches(path, expected))
            path.write_bytes(b"Copyright Someone Else\nLicense text\n")
            self.assertFalse(notices.text_hash_matches(path, expected))

    def test_does_not_ingest_its_own_generated_notices(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "LICENSE").write_text("Upstream license", encoding="utf-8")
            (root / "legal").mkdir()
            (root / "legal/THIRD-PARTY-NOTICES.txt").write_text("Previous generated output", encoding="utf-8")
            self.assertEqual([path.relative_to(root).as_posix() for path in notices.own_notices(root)], ["LICENSE"])

    def test_keeps_upstream_notice_and_nested_license_texts(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "NOTICE").write_text("Author notice", encoding="utf-8")
            (root / "licenses").mkdir()
            (root / "licenses/MIT.txt").write_text("License body", encoding="utf-8")
            self.assertEqual({path.relative_to(root).as_posix() for path in notices.own_notices(root)}, {"NOTICE", "licenses/MIT.txt"})


if __name__ == "__main__":
    unittest.main()
