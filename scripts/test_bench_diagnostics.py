"""Exercise bounded capture using a real OS pipe, without a gateway."""
import json
import os
from pathlib import Path
import tempfile
import unittest

from bench_diagnostics import Capture


class CaptureTests(unittest.TestCase):
    def test_trace_cap_drains_excess_and_reports_truncation(self):
        with tempfile.TemporaryDirectory() as directory:
            capture = Capture("trace", directory, 5, 1000)
            read, write = os.pipe()
            capture.drain_trace(os.fdopen(read, "rb"))
            capture.start(0)
            with os.fdopen(write, "wb") as writer:
                writer.write(b"x" * 100000)
            self.assertIsNone(capture.finish())
            self.assertEqual((Path(directory) / "trace.log").stat().st_size, 1000)
            report = json.loads((Path(directory) / "diagnostic.json").read_text())
            self.assertEqual(report["discardedTraceBytes"], 99000)

    def test_empty_capture_fails_explicitly(self):
        with tempfile.TemporaryDirectory() as directory:
            capture = Capture("trace", directory, 5, 1000)
            read, write = os.pipe()
            capture.drain_trace(os.fdopen(read, "rb"))
            os.close(write)
            self.assertEqual(capture.finish(), "no diagnostic capture produced")


if __name__ == "__main__":
    unittest.main()
