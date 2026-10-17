#!/usr/bin/env python3
"""Exercise privileged entry checks in a disposable user/mount namespace.

Bubblewrap's root maps only to the invoking user. Installed profiles, /etc,
/run, and systemd-run are fixtures; no host service or real root helper is used.
"""

import json
import os
from pathlib import Path
import pwd
import subprocess
import sys
import tempfile


def owner_key(username):
    return "u" + "".join(
        chr(byte) if 97 <= byte <= 122 or 48 <= byte <= 57 else f"-{byte:02x}"
        for byte in username.encode()
    )


def main():
    binary = Path(sys.argv[1]).resolve(strict=True)
    owner = pwd.getpwuid(os.getuid())
    key = owner_key(owner.pw_name)
    # Mount fixture profiles over existing source directories, avoiding writes
    # to the real store while leaving all executable/library closures visible.
    store_paths = list(Path("/nix/store").glob("*-source"))[:2]
    assert len(store_paths) == 2
    installed, pinned = store_paths
    with tempfile.TemporaryDirectory(prefix="nook-entry-", dir="/tmp") as temporary:
        root = Path(temporary)
        etc = root / "etc"
        registry = etc / f"sandbox/profiles/{key}"
        registry.mkdir(parents=True)
        (registry / "check").symlink_to(installed)
        (etc / "passwd").write_text(
            f"root:x:0:0:root:/root:/bin/sh\n{owner.pw_name}:x:{owner.pw_uid}:{owner.pw_gid}::{owner.pw_dir}:/bin/sh\n"
        )
        (etc / "group").write_text(f"root:x:0:\nusers:x:{owner.pw_gid}:\n")
        (etc / "nsswitch.conf").write_text("passwd: files\ngroup: files\n")
        runtime = root / f"run/nook-{key}-check-main"
        runtime.mkdir(parents=True)
        (runtime / "profile-root").symlink_to(pinned)
        identity = runtime / "host-identity"
        identity.write_text(f"{owner.pw_uid}\t{owner.pw_gid}\n")
        profiles = []
        for generation, guest_uid in [("installed", 1000), ("running", 2345)]:
            profile = root / generation
            profile.mkdir()
            values = {
                "name": "check",
                "username": owner.pw_name,
                "identityVersion": 2,
                "uid": guest_uid,
                "gid": 2346,
                "home": owner.pw_dir,
                "shell": f"/{generation}-shell",
                "entry": f"/{generation}-entry",
            }
            (profile / "profile").write_text(
                "".join(f"{k}\t{v}\n" for k, v in values.items())
            )
            for name in [
                "mounts",
                "environment",
                "session-rules",
                "system-rules",
                "pasta",
            ]:
                (profile / name).write_text("")
            profiles.append(profile)
        mock = root / "bin"
        mock.mkdir()
        log = root / "entry.json"
        systemd_run = mock / "systemd-run"
        systemd_run.write_text(
            f"#!{Path(sys.executable).resolve()}\nimport json, os, sys\n"
            "from pathlib import Path\n"
            "Path(os.environ['NOOK_ENTRY_LOG']).write_text(json.dumps(sys.argv[1:]))\n"
        )
        systemd_run.chmod(0o700)
        command = [
            "bwrap",
            "--unshare-user",
            "--uid",
            "0",
            "--gid",
            "0",
            "--ro-bind",
            "/",
            "/",
            "--dev",
            "/dev",
            "--bind",
            str(root),
            str(root),
            "--ro-bind",
            str(etc),
            "/etc",
            "--ro-bind",
            str(runtime.parent),
            "/run",
            "--ro-bind",
            str(profiles[0]),
            str(installed),
            "--ro-bind",
            str(profiles[1]),
            str(pinned),
            "--",
            str(binary),
            "--enter",
            str(installed),
            "main",
        ]
        environment = os.environ | {
            "PATH": str(mock) + os.pathsep + os.environ["PATH"],
            "PKEXEC_UID": str(owner.pw_uid),
            "NOOK_ENTRY_LOG": str(log),
        }

        def invoke(**changes):
            return subprocess.run(
                command,
                env=environment | changes,
                capture_output=True,
                text=True,
                timeout=15,
            )

        result = invoke()
        assert result.returncode == 0, result.stderr
        arguments = json.loads(log.read_text())
        assert f"{owner.pw_name}@sbx-{owner.pw_uid}-check-main" in arguments
        assert arguments[-2:] == ["/running-entry", "/running-shell"], arguments
        log.unlink()
        result = invoke(PKEXEC_UID=str(owner.pw_uid + 1))
        assert result.returncode == 1 and "does not own" in result.stderr, result.stderr
        assert not log.exists()
        for uid, gid in [
            (owner.pw_uid + 1, owner.pw_gid),
            (owner.pw_uid, owner.pw_gid + 1),
        ]:
            identity.write_text(f"{uid}\t{gid}\n")
            result = invoke()
            assert result.returncode == 1 and "IDs changed" in result.stderr, (
                result.stderr
            )
            assert not log.exists()
    print(
        "Entry helper used the pinned guest generation and refused changed owners/host IDs"
    )


if __name__ == "__main__":
    main()
