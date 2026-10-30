"""Check that importing documentation removes skills without changing the docs."""

import hashlib
import io
import json
from pathlib import Path
import runpy
import tarfile
import tempfile
import unittest


IMPORTER = runpy.run_path(str(Path(__file__).with_name("import-docs-snapshot.py")))


class ImportSnapshotTests(unittest.TestCase):
    def test_import_preserves_docs_and_source_identity_but_removes_skill_downloads(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source, target = root / "source", root / "target"
            source.mkdir()
            snapshot = {
                "schemaVersion": 1,
                "sourceRevision": "a" * 40,
                "sourceDirty": False,
                "agentGuideCliVersion": "0.19.0",
            }
            files = {
                name: b"Documentation\n"
                for name in ["index.html", "start/index.html", "markdown/index.md",
                             "markdown/full.md", "llms.txt", "llms-full.txt"]
            }
            files["snapshot.json"] = json.dumps(snapshot).encode()
            files["markdown/manifest.json"] = json.dumps({
                "schemaVersion": 1, "snapshot": snapshot,
            }).encode()
            files["_astro/inter-latin-wght-normal.abc.woff2"] = b"Selected font bytes"
            guides = ["agent-guide.md", "agents/0.19.0/agent-guide.md", "agents/current.json"]
            with tarfile.open(source / "snapshot.tar.gz", "w:gz") as archive:
                for name, content in {**files, **dict.fromkeys(guides, b"Old skill")}.items():
                    entry = tarfile.TarInfo(name)
                    entry.size = len(content)
                    archive.addfile(entry, io.BytesIO(content))
            original = (source / "snapshot.tar.gz").read_bytes()
            pin = {
                **snapshot,
                "archive": "snapshot.tar.gz",
                "archiveBytes": len(original),
                "sha256": hashlib.sha256(original).hexdigest(),
            }
            (source / "snapshot-lock.json").write_text(json.dumps(pin))

            with self.assertRaisesRegex(ValueError, "skill content must not be bundled"):
                IMPORTER["verify"](source)
            imported = IMPORTER["import_snapshot"](source, target)
            self.assertEqual(imported["sourceArchiveSha256"], pin["sha256"])
            self.assertEqual({field: imported[field] for field in snapshot}, snapshot)
            with tarfile.open(target / "snapshot.tar.gz", "r:gz") as archive:
                self.assertEqual(set(archive.getnames()), set(files) | {"font-licenses/inter.txt"})
                for name, content in files.items():
                    self.assertEqual(archive.extractfile(name).read(), content)
                self.assertEqual(archive.extractfile("font-licenses/inter.txt").read(),
                                 (IMPORTER["FONT_NOTICES"] / "inter.txt").read_bytes())
            self.assertEqual((source / "snapshot.tar.gz").read_bytes(), original)

            # Reimporting the same input and an already-filtered artifact is stable.
            first = (target / "snapshot.tar.gz").read_bytes()
            self.assertEqual(IMPORTER["import_snapshot"](source, target), imported)
            self.assertEqual(IMPORTER["import_snapshot"](target, target), imported)
            self.assertEqual((target / "snapshot.tar.gz").read_bytes(), first)

            # Verify input before replacing any destination files.
            (source / "snapshot.tar.gz").write_bytes(b"corrupt")
            with self.assertRaisesRegex(ValueError, "checksum"):
                IMPORTER["import_snapshot"](source, target)
            self.assertEqual((target / "snapshot.tar.gz").read_bytes(), first)


if __name__ == "__main__":
    unittest.main()
