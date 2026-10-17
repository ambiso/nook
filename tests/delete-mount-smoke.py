#!/usr/bin/env python3
"""Refuse a bind mount without deleting either the instance or mounted data.

Run inside a dev sandbox with bubblewrap on PATH. The extra user/mount namespace
needs no host root and contains only disposable fixtures.
"""

import os
from pathlib import Path
import subprocess
import sys
import tempfile


def main():
    binary = Path(sys.argv[1]).resolve(strict=True)
    with tempfile.TemporaryDirectory(prefix="nook-delete-mount-") as temporary:
        root = Path(temporary)
        profile = root / "profiles/test"
        profile.mkdir(parents=True)
        state = root / "state"
        home = state / "instances/n-project/home"
        (home / "shared").mkdir(parents=True)
        (home / "keep").write_text("instance data")
        outside = root / "outside"
        outside.mkdir()
        (outside / "keep").write_text("host data")
        values = {
            "name": "test",
            "uid": os.getuid(),
            "gid": os.getgid(),
            "state": state,
        }
        (profile / "profile").write_text(
            "".join(f"{key}\t{value}\n" for key, value in values.items())
        )
        for name in ["mounts", "environment", "session-rules", "system-rules", "pasta"]:
            (profile / name).write_text("")
        mock = root / "bin"
        mock.mkdir()
        systemctl = mock / "systemctl"
        systemctl.write_text(
            f"#!{Path(sys.executable).resolve()}\n"
            "import sys\n"
            "assert sys.argv[1] == 'show'\n"
            "print('inactive')\n"
        )
        systemctl.chmod(0o700)
        result = subprocess.run(
            [
                "bwrap",
                "--unshare-user",
                "--uid",
                str(os.getuid()),
                "--gid",
                str(os.getgid()),
                "--ro-bind",
                "/",
                "/",
                "--dev",
                "/dev",
                "--bind",
                str(root),
                str(root),
                "--bind",
                str(outside),
                str(home / "shared"),
                "--",
                str(binary),
                "--profiles",
                str(profile.parent),
                "delete",
                "test",
                "--name",
                "project",
                "--yes",
            ],
            env=os.environ | {"PATH": str(mock) + os.pathsep + os.environ["PATH"]},
            capture_output=True,
            text=True,
            timeout=15,
        )
        assert result.returncode == 1, result.stderr
        assert "mount points are refused" in result.stderr, result.stderr
        assert (home / "keep").read_text() == "instance data"
        assert (outside / "keep").read_text() == "host data"
    print(
        "Deletion refused a same-filesystem bind mount and preserved all fixture data"
    )


if __name__ == "__main__":
    main()
