#!/usr/bin/env python3
"""Check cursor discovery in real systemd services without an X11 server.

Run inside a dev sandbox. The JSON input is the guest's evaluated
environment.variables. Bubblewrap supplies a disposable home containing a
cursor theme, so the test does not change the user's real theme or home files.
"""

import argparse
import ctypes
import json
import os
import shutil
import subprocess
import sys
import tempfile
import uuid
from pathlib import Path


def probe(library):
    cursor = ctypes.CDLL(library)
    cursor.XcursorLibraryLoadImage.argtypes = [
        ctypes.c_char_p,
        ctypes.c_char_p,
        ctypes.c_int,
    ]
    cursor.XcursorLibraryLoadImage.restype = ctypes.c_void_p
    cursor.XcursorImageDestroy.argtypes = [ctypes.c_void_p]
    cursor.XcursorImageDestroy.restype = None
    image = cursor.XcursorLibraryLoadImage(b"nook-test-only-cursor", b"default", 24)
    if not image:
        return 42
    cursor.XcursorImageDestroy(image)
    return 0


def main():
    if len(sys.argv) == 3 and sys.argv[1] == "--probe":
        return probe(sys.argv[2])
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("library", help="Path to libXcursor.so.1")
    parser.add_argument("cursor_file", help="An installed Xcursor-format cursor")
    parser.add_argument("environment", help="Evaluated environment.variables JSON")
    parser.add_argument("--bubblewrap", default="bwrap")
    args = parser.parse_args()
    environment = json.loads(Path(args.environment).read_text())
    home = Path(os.environ["HOME"])
    assert home.is_absolute() and home.parent != Path("/")

    with tempfile.TemporaryDirectory(prefix="nook-cursor-test-") as temporary:
        fixture = Path(temporary) / "home"
        theme = fixture / ".icons/nook-test-theme/cursors"
        theme.mkdir(parents=True)
        shutil.copyfile(args.cursor_file, theme / "nook-test-only-cursor")
        (fixture / ".icons/default").symlink_to("nook-test-theme")
        command = [
            args.bubblewrap,
            "--unshare-user",
            "--unshare-pid",
            "--die-with-parent",
            "--ro-bind",
            "/",
            "/",
            "--dev",
            "/dev",
            "--tmpfs",
            "/tmp",
            "--tmpfs",
            str(home.parent),
            "--dir",
            str(home),
            "--bind",
            str(fixture),
            str(home),
            "--ro-bind",
            str(Path(__file__).resolve()),
            "/tmp/probe.py",
            "--chdir",
            "/tmp",
            "--",
            str(Path(sys.executable).resolve()),
            "/tmp/probe.py",
            "--probe",
            args.library,
        ]
        # Each service gets a fresh process: libXcursor caches its search path.
        cases = [
            ("single theme directory", str(Path(args.cursor_file).parents[2]), 42),
            ("unexpanded HOME", "$HOME/.icons", 42),
            ("resolved NixOS search path", environment["XCURSOR_PATH"], 0),
        ]
        for label, path, expected in cases:
            unit = "nook-cursor-test-" + uuid.uuid4().hex + ".service"
            try:
                result = subprocess.run(
                    [
                        "systemd-run",
                        "--user",
                        "--quiet",
                        "--wait",
                        "--pipe",
                        "--collect",
                        "--unit=" + unit,
                        "--setenv=XCURSOR_PATH=" + path,
                        "--setenv=HOME=" + str(home),
                        "--",
                        *command,
                    ],
                    check=False,
                    capture_output=True,
                    text=True,
                    timeout=30,
                )
            finally:
                # Killing a timed-out systemd-run client does not stop its unit.
                subprocess.run(
                    ["systemctl", "--user", "stop", unit],
                    check=False,
                    capture_output=True,
                    timeout=15,
                )
            assert result.returncode == expected, (label, result.stderr, result.stdout)
            print(f"{label}: {'found' if expected == 0 else 'not found'} as expected")
    return 0


if __name__ == "__main__":
    sys.exit(main())
