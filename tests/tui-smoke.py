#!/usr/bin/env python3
"""Exercise the built selector in a PTY without starting any real services."""

import fcntl
import json
import os
from pathlib import Path
import pty
import re
import secrets
import select
import shutil
import struct
import subprocess
import sys
import tempfile
import termios
import time


def select_instance(binary, profiles, environment, actions):
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 100, 0, 0))
    output = bytearray()
    try:
        with subprocess.Popen(
            [str(binary), "--profiles", str(profiles)],
            stdin=slave,
            stdout=slave,
            stderr=slave,
            env=environment,
        ) as process:
            try:
                for expected, keys in actions:
                    deadline = time.monotonic() + 10
                    while expected not in output.decode(errors="replace"):
                        if time.monotonic() >= deadline:
                            raise AssertionError(
                                f"TUI did not display {expected!r}: {output[-2048:].decode(errors='replace')!r}"
                            )
                        if select.select([master], [], [], 0.1)[0]:
                            output.extend(os.read(master, 65536))
                    os.write(master, keys)
                return process.wait(timeout=10)
            finally:
                if process.poll() is None:
                    process.kill()
    finally:
        os.close(master)
        os.close(slave)


def main():
    binary = Path(sys.argv[1]).resolve(strict=True)
    with tempfile.TemporaryDirectory(prefix="nook-tui-") as temporary:
        root = Path(temporary)
        profiles = root / "profiles"
        mock_bin = root / "bin"
        profiles.mkdir()
        mock_bin.mkdir()
        started = root / "started.json"
        token = secrets.token_hex(3)
        names = [f"smoke-{token}-{suffix}" for suffix in ["a", "b"]]
        runtime = Path(f"/run/user/{os.getuid()}/sandbox")
        assert all(not (runtime / name).exists() for name in names)

        for name in names:
            profile = profiles / name
            profile.mkdir()
            state = root / name
            (state / "instances/n-saved/home").mkdir(parents=True)
            values = {
                "name": name,
                "uid": os.getuid(),
                "gid": os.getgid(),
                "home": str(Path.home()),
                "state": state,
                "persistent": 1,
            }
            (profile / "profile").write_text(
                "".join(f"{key}\t{value}\n" for key, value in values.items())
            )
            for filename in [
                "mounts",
                "environment",
                "session-rules",
                "system-rules",
                "pasta",
            ]:
                (profile / filename).write_text("")

        # List stopped instances normally, but fail start before the launcher
        # can invoke pkexec. This tests selection without touching host services.
        systemctl = mock_bin / "systemctl"
        systemctl.write_text(
            f"#!{sys.executable}\n"
            "import json, os, sys\n"
            "from pathlib import Path\n"
            "if sys.argv[1] == 'start':\n"
            "    Path(os.environ['NOOK_SMOKE_LOG']).write_text(json.dumps(sys.argv[2:]))\n"
            "sys.exit(0 if sys.argv[1] == 'list-units' else 1)\n"
        )
        systemctl.chmod(0o700)
        environment = os.environ | {
            "TERM": "xterm-256color",
            "PATH": str(mock_bin) + os.pathsep + os.environ["PATH"],
            "NOOK_SMOKE_LOG": str(started),
        }
        cases = [
            ([("Nook", b"n"), ("template:", b"project\r")], names[0], "n-project"),
            ([("Nook", b"j\r")], names[0], "n-saved"),
            ([("Nook", b"jjn"), ("template:", b"other\r")], names[1], "n-other"),
            ([("Nook", b"e")], names[0], r"e-[a-f0-9]{16}"),
        ]
        try:
            for actions, name, instance in cases:
                started.unlink(missing_ok=True)
                assert select_instance(binary, profiles, environment, actions) == 1
                unit = json.loads(started.read_text())[0]
                assert re.fullmatch(
                    rf"sandbox-{os.getuid()}-{re.escape(name)}@{instance}\.service",
                    unit,
                ), unit
            assert select_instance(binary, profiles, environment, [("Nook", b"q")]) == 0
        finally:
            for name in names:
                shutil.rmtree(runtime / name, ignore_errors=True)
        print(
            "TUI checks passed: naming, saved instances, templates, ephemeral creation, quit"
        )


if __name__ == "__main__":
    main()
