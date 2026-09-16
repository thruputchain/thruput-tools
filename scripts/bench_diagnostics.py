"""Bounded diagnostic artifacts for an owned gateway; never throughput evidence."""
import json
from pathlib import Path
import signal
import subprocess
import threading
import time


class Capture:
    def __init__(self, kind, out, seconds, max_bytes):
        self.kind, self.out = kind, Path(out)
        self.seconds, self.max_bytes = seconds, max_bytes
        self.started = None
        self.written = self.discarded = 0
        self.error = None
        self.reader = self.perf = self.perf_log = None
        self.command = None

    def drain_trace(self, pipe):
        def drain():
            try:
                with pipe, (self.out / "trace.log").open("wb") as output:
                    while chunk := pipe.read1(8192):
                        if self.started is None or time.monotonic() - self.started >= self.seconds:
                            continue
                        kept = chunk[:max(0, self.max_bytes - self.written)]
                        output.write(kept)
                        self.written += len(kept)
                        self.discarded += len(chunk) - len(kept)
            except OSError as error:
                self.error = str(error)
        self.reader = threading.Thread(target=drain, daemon=True)
        self.reader.start()

    def start(self, pid):
        self.started = time.monotonic()
        if self.kind == "perf":
            self.command = ["perf", "record", "--pid", str(pid), "--freq", "99",
                "--event", "cpu-clock:u", "--call-graph", "dwarf,4096",
                "--no-buildid-cache", "--max-size", f"{self.max_bytes}B",
                "--output", str(self.out / "perf.data"), "--", "sleep", str(self.seconds)]
            self.perf_log = (self.out / "perf.stderr").open("w")
            self.perf = subprocess.Popen(self.command, stdout=subprocess.DEVNULL, stderr=self.perf_log)

    def finish(self):
        # Caller stops the owned gateway first, so its pipe must reach EOF.
        if self.reader:
            self.reader.join(timeout=10)
            if self.reader.is_alive():
                self.error = "gateway trace pipe did not close"
        if self.perf:
            try:
                self.perf.wait(timeout=self.seconds + 10)
            except subprocess.TimeoutExpired:
                self.perf.send_signal(signal.SIGINT)
                try:
                    self.perf.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    self.perf.kill()
                    self.perf.wait(timeout=10)
                self.error = "perf exceeded capture deadline"
            if self.perf.returncode:
                self.error = f"perf exited {self.perf.returncode}; see perf.stderr"
        if self.perf_log:
            self.perf_log.close()
        artifact = self.out / ("trace.log" if self.kind == "trace" else "perf.data")
        if self.started is None or not artifact.exists() or artifact.stat().st_size == 0:
            self.error = self.error or "no diagnostic capture produced"
        report = dict(kind=self.kind, purpose="diagnostic; not performance comparison",
            window="begins after setup at warmup load start; includes warmup and may overlap measurement",
            requestedSeconds=self.seconds, maxBytes=self.max_bytes,
            bytes=artifact.stat().st_size if artifact.exists() else 0,
            discardedTraceBytes=self.discarded, command=self.command, error=self.error,
            bounds="trace byte cap is exact; perf max-size may overshoot by a record buffer")
        (self.out / "diagnostic.json").write_text(json.dumps(report, indent=2) + "\n")
        return self.error
