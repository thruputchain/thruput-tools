#!/usr/bin/env python3
"""Verify and import a selected developer-site CI artifact into the CLI assets."""

import argparse
import gzip
import hashlib
import io
import json
from pathlib import Path, PurePosixPath
import re
import shutil
import tarfile

MAX_FILES = 4096
MAX_BYTES = 64 * 1024 * 1024
MAX_TAR_BYTES = MAX_BYTES + MAX_FILES * 1024 + 10240


def verify(directory: Path) -> dict:
    pin = json.loads((directory / "snapshot-lock.json").read_text())
    archive = directory / "snapshot.tar.gz"
    if (pin.get("schemaVersion") != 1 or pin.get("archive") != archive.name
            or pin.get("sourceDirty") is not False
            or not re.fullmatch(r"[0-9a-f]{40}", pin.get("sourceRevision", ""))):
        raise ValueError("snapshot must identify clean committed source and supported schema")
    data = archive.read_bytes()
    if len(data) != pin.get("archiveBytes") or hashlib.sha256(data).hexdigest() != pin.get("sha256"):
        raise ValueError("archive does not match pinned size/checksum")
    with gzip.GzipFile(fileobj=io.BytesIO(data)) as gz:
        expanded = gz.read(MAX_TAR_BYTES + 1)
    if len(expanded) > MAX_TAR_BYTES:
        raise ValueError("archive exceeds expansion limit")
    paths = set()
    total = 0
    snapshot = None
    with tarfile.open(fileobj=io.BytesIO(expanded), mode="r:") as tar:
        for entry in tar:
            name = entry.name
            if (not entry.isfile() or name in paths or "\\" in name or ":" in name
                    or any(ord(char) < 32 or ord(char) == 127 for char in name)
                    or any(not part or part.startswith(".") for part in name.split("/"))
                    or PurePosixPath(name).is_absolute()):
                raise ValueError(f"unsupported archive entry: {name}")
            paths.add(name)
            total += entry.size
            if len(paths) > MAX_FILES or total > MAX_BYTES:
                raise ValueError("archive exceeds content limits")
            if name == "snapshot.json":
                snapshot = json.load(tar.extractfile(entry))
    fields = ("schemaVersion", "sourceRevision", "sourceDirty", "agentGuideCliVersion")
    if snapshot != {field: pin[field] for field in fields}:
        raise ValueError("archive manifest differs from pin")
    if not {"index.html", "start/index.html", "agent-guide.md"} <= paths:
        raise ValueError("archive is missing required documentation")
    return pin


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("artifact", type=Path, nargs="?", help="selected downloaded CI artifact directory")
    args = parser.parse_args()
    target = Path(__file__).resolve().parent.parent / "crates/tools/cli/assets/docs"
    source = args.artifact if args.artifact else target
    pin = verify(source)
    if args.artifact:
        target.mkdir(parents=True, exist_ok=True)
        for name in ("snapshot.tar.gz", "snapshot-lock.json"):
            shutil.copyfile(source / name, target / name)
    print(f"Verified docs {pin['sha256']} from developers-site {pin['sourceRevision']}")


if __name__ == "__main__":
    main()
