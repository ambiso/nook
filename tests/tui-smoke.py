#!/usr/bin/env python3
"""Exercise the built selector in a PTY without starting any real services."""

import fcntl
import json
import os
import pwd
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


def owner_key():
    return "u" + "".join(
        chr(byte) if 97 <= byte <= 122 or 48 <= byte <= 57 else f"-{byte:02x}"
        for byte in pwd.getpwuid(os.getuid()).pw_name.encode()
    )


def select_instance(binary, profiles, environment, actions, output_log=None):
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
                code = process.wait(timeout=10)
            finally:
                if process.poll() is None:
                    process.kill()
            if output_log is not None:
                output_log.extend(output)
            return code
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
        stopped = root / "stopped.json"
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
                "identityVersion": 2,
                "username": pwd.getpwuid(os.getuid()).pw_name,
                "uid": 2345,
                "gid": 2346,
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
            "if sys.argv[1] == 'is-active':\n"
            "    sys.exit(0 if os.environ.get('NOOK_SMOKE_STATE') == 'active' else 1)\n"
            "if sys.argv[1] == 'show':\n"
            "    print(os.environ.get('NOOK_SMOKE_STATE', 'inactive'))\n"
            "    sys.exit(1 if os.environ.get('NOOK_SMOKE_QUERY_FAIL') else 0)\n"
            "if sys.argv[1] == 'start':\n"
            "    Path(os.environ['NOOK_SMOKE_LOG']).write_text(json.dumps(sys.argv[2:]))\n"
            "if sys.argv[1] == 'stop':\n"
            "    with open(os.environ['NOOK_SMOKE_STOP_LOG'], 'a') as log:\n"
            "        log.write(json.dumps(sys.argv[2:]) + '\\n')\n"
            "sys.exit(0 if sys.argv[1] in ('list-units', 'stop') else 1)\n"
        )
        systemctl.chmod(0o700)
        environment = os.environ | {
            "TERM": "xterm-256color",
            "PATH": str(mock_bin) + os.pathsep + os.environ["PATH"],
            "NOOK_SMOKE_LOG": str(started),
            "NOOK_SMOKE_STOP_LOG": str(stopped),
            "NOOK_SMOKE_STATE": "inactive",
        }
        cases = [
            ([("Nook", b"n"), ("template:", b"project\r")], names[0], "n-project"),
            ([("Nook", b"j\r")], names[0], "n-saved"),
            ([("Nook", b"jjn"), ("template:", b"other\r")], names[1], "n-other"),
            ([("Nook", b"e")], names[0], r"e-[a-f0-9]{16}"),
        ]
        try:
            # --join must fail before creating runtime state or issuing a
            # start request, for both the main instance and a saved named one.
            for selection in [[], ["--name", "saved"]]:
                result = subprocess.run(
                    [
                        str(binary),
                        "--profiles",
                        str(profiles),
                        "launch",
                        names[0],
                        "--join",
                        *selection,
                        "--",
                        "true",
                    ],
                    env=environment,
                    capture_output=True,
                    text=True,
                    timeout=10,
                )
                assert result.returncode == 1, result.stderr
                assert "omit --join" in result.stderr, result.stderr
                assert not started.exists()
                assert not (runtime / names[0]).exists()
            missing = subprocess.run(
                [
                    str(binary),
                    "--profiles",
                    str(profiles),
                    "launch",
                    names[0],
                    "--",
                    "true",
                ],
                env=environment | {"NOOK_SMOKE_STATE": "not-found"},
                capture_output=True,
                text=True,
                timeout=10,
            )
            assert (
                missing.returncode == 1
                and "nook.nixosModules.default" in missing.stderr
            ), missing.stderr
            for actions, name, instance in cases:
                started.unlink(missing_ok=True)
                assert select_instance(binary, profiles, environment, actions) == 1
                unit = json.loads(started.read_text())[0]
                assert re.fullmatch(
                    rf"sandbox-{owner_key()}-{re.escape(name)}@{instance}\.service",
                    unit,
                ), unit
            assert select_instance(binary, profiles, environment, [("Nook", b"q")]) == 0
            indicator_output = bytearray()
            assert (
                select_instance(
                    binary,
                    profiles,
                    environment | {"NOOK_SMOKE_STATE": "active"},
                    [("Nook", b"q")],
                    indicator_output,
                )
                == 0
            )
            assert "●" in indicator_output.decode(errors="replace"), indicator_output
            stopped.unlink(missing_ok=True)
            assert (
                select_instance(
                    binary,
                    profiles,
                    environment | {"NOOK_SMOKE_STATE": "active"},
                    [("Nook", b"s"), ("Stopping", b"q")],
                )
                == 0
            )
            stop_arguments = json.loads(stopped.read_text())
            assert stop_arguments[0] == "--no-block", stop_arguments
            assert re.fullmatch(
                rf"sandbox-{owner_key()}-{re.escape(names[0])}@main\.service",
                stop_arguments[1],
            ), stop_arguments
            result = subprocess.run(
                [
                    str(binary),
                    "--profiles",
                    str(profiles),
                    "stop",
                    "--all",
                ],
                env=environment | {"NOOK_SMOKE_STATE": "active"},
                capture_output=True,
                text=True,
                timeout=10,
            )
            assert result.returncode == 0, result.stderr
            assert "Stopped " in result.stdout, (result.stdout, result.stderr)
            units = [json.loads(line)[0] for line in stopped.read_text().splitlines()]
            assert len(units) == 4, units
            assert all(
                unit
                    == f"sandbox-{owner_key()}-{names[int(index / 2)]}@{instance}.service"
                for index, (unit, instance) in enumerate(
                    zip(units, ["main", "n-saved", "main", "n-saved"])
                )
            ), units

            stopped.unlink()
            result = subprocess.run(
                [
                    str(binary),
                    "--profiles",
                    str(profiles),
                    "stop",
                    names[0],
                    "--all",
                ],
                env=environment | {"NOOK_SMOKE_STATE": "active"},
                capture_output=True,
                text=True,
                timeout=10,
            )
            assert result.returncode == 0, result.stderr
            units = [json.loads(line)[0] for line in stopped.read_text().splitlines()]
            assert len(units) == 2, units
            assert all(
                unit == f"sandbox-{owner_key()}-{names[0]}@{instance}.service"
                for unit, instance in zip(units, ["main", "n-saved"])
            ), units

            stopped.unlink()
            saved = root / names[0] / "instances/n-saved"
            other = root / names[1] / "instances/n-saved"
            delete = [
                str(binary),
                "--profiles",
                str(profiles),
                "delete",
                names[0],
                "--name",
                "saved",
            ]
            # A script must explicitly opt into irreversible deletion.
            result = subprocess.run(
                delete, env=environment, capture_output=True, text=True
            )
            assert result.returncode == 1 and "confirmation" in result.stderr, (
                result.stderr
            )
            assert saved.is_dir()
            for extra in [
                {"NOOK_SMOKE_STATE": "active"},
                {"NOOK_SMOKE_STATE": "activating"},
                {"NOOK_SMOKE_QUERY_FAIL": "1"},
            ]:
                result = subprocess.run(
                    delete + ["--yes"],
                    env=environment | extra,
                    capture_output=True,
                    text=True,
                )
                assert result.returncode == 1, result.stderr
                assert saved.is_dir()

            # Cancelling the TUI dialog keeps all data; confirming removes only
            # that named instance, not the template or its neighbor.
            (saved / "home/keep").write_text("saved data")
            assert (
                select_instance(
                    binary,
                    profiles,
                    environment,
                    [("Nook", b"jd"), ("instance?", b"\x19nq")],
                )
                == 0
            )
            assert (saved / "home/keep").read_text() == "saved data"
            assert (
                select_instance(
                    binary,
                    profiles,
                    environment,
                    [("Nook", b"jd"), ("instance?", b"yq")],
                )
                == 0
            )
            assert not saved.exists()
            assert other.is_dir()
            result = subprocess.run(
                [
                    str(binary),
                    "--profiles",
                    str(profiles),
                    "delete",
                    names[1],
                    "--name",
                    "saved",
                    "--yes",
                ],
                env=environment,
                capture_output=True,
                text=True,
            )
            assert result.returncode == 0, result.stderr
            assert not other.exists()
        finally:
            for name in names:
                shutil.rmtree(runtime / name, ignore_errors=True)
        print(
            "CLI/TUI checks passed: --join, naming, instances, deletion guards, confirmation, cancellation, quit"
        )


if __name__ == "__main__":
    main()
