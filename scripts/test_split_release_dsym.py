"""Check macOS packaging arguments and fail-fast behavior without Apple tools."""
import json
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest


SCRIPT = Path(__file__).with_name("split-release-dsym.sh")


class SplitReleaseDsymTests(unittest.TestCase):
    def package(self, directory, fail_command=""):
        root = Path(directory)
        tools = root / "tools"
        tools.mkdir()
        mock = tools / "mock"
        mock.write_text(
            "#!/usr/bin/env python3\n"
            "import json, os, pathlib, sys\n"
            "name = pathlib.Path(sys.argv[0]).name\n"
            "with open(os.environ['COMMAND_LOG'], 'a') as log:\n"
            "    log.write(json.dumps([name, *sys.argv[1:]]) + '\\n')\n"
            "if name == os.environ['FAIL_COMMAND']:\n"
            "    sys.exit(1)\n"
            "if name == 'dsymutil':\n"
            "    bundle = pathlib.Path(sys.argv[3])\n"
            "    bundle.mkdir()\n"
            "    (bundle / 'debug').write_text('debug info')\n"
        )
        mock.chmod(0o755)
        for command in ("dsymutil", "strip", "codesign"):
            (tools / command).symlink_to(mock)
        binary = root / "staged binary" / "thruput"
        binary.parent.mkdir()
        binary.write_text("binary")
        debug_root = root / "debug root"
        archive = root / "debug archive.tar.gz"
        log = root / "commands.jsonl"
        result = subprocess.run(
            ["bash", str(SCRIPT), str(binary), str(debug_root), str(archive)],
            env={**os.environ, "PATH": f"{tools}{os.pathsep}{os.environ['PATH']}",
                 "COMMAND_LOG": str(log), "FAIL_COMMAND": fail_command},
            capture_output=True, text=True,
        )
        commands = [json.loads(line) for line in log.read_text().splitlines()]
        return result, commands, binary, debug_root, archive

    def test_packages_dsym_and_resigns_staged_binary(self):
        with tempfile.TemporaryDirectory() as directory:
            result, commands, binary, debug_root, archive = self.package(directory)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(commands, [
                ["dsymutil", str(binary), "-o", str(debug_root / "thruput.dSYM")],
                ["strip", "-S", str(binary)],
                ["codesign", "--force", "--sign", "-", str(binary)],
                ["codesign", "--verify", "--strict", str(binary)],
            ])
            with tarfile.open(archive) as contents:
                self.assertIn("thruput.dSYM/debug", contents.getnames())

    def test_tool_failure_stops_packaging(self):
        for command, count in (("dsymutil", 1), ("strip", 2), ("codesign", 3)):
            with self.subTest(command=command), tempfile.TemporaryDirectory() as directory:
                result, commands, _, _, archive = self.package(directory, command)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(len(commands), count)
                self.assertFalse(archive.exists())


if __name__ == "__main__":
    unittest.main()
