#!/usr/bin/env python3
"""Reproduce dconf activation with an empty bus and with its service installed.

Run inside a dev sandbox. Only disposable homes and private D-Bus sessions are
used; the host and guest's existing dconf settings are never read or written.
"""

import html
import os
import shutil
from pathlib import Path
import subprocess
import sys
import tempfile


def main():
    if len(sys.argv) > 1:
        dconf, dbus = map(Path, sys.argv[1:])
    else:
        binaries = [shutil.which(name) for name in ["dconf", "dbus-run-session"]]
        if not all(binaries):
            raise RuntimeError(
                "dconf and dbus-run-session must be on PATH; use nix develop"
            )
        dconf, dbus = (Path(binary).resolve().parent.parent for binary in binaries)
    with tempfile.TemporaryDirectory(prefix="nook-dconf-", dir="/tmp") as temporary:
        root = Path(temporary)
        config = root / "session.conf"
        home = root / "home"
        home.mkdir()
        runtime = root / "run"
        runtime.mkdir(mode=0o700)
        environment = os.environ | {
            "HOME": str(home),
            "XDG_CONFIG_HOME": str(home / ".config"),
            "XDG_RUNTIME_DIR": str(runtime),
        }
        for name in ["DBUS_SESSION_BUS_ADDRESS", "DCONF_PROFILE", "GSETTINGS_BACKEND"]:
            environment.pop(name, None)

        def invoke(services):
            config.write_text(
                "<busconfig><type>session</type><listen>unix:tmpdir=/tmp</listen>"
                '<auth>EXTERNAL</auth><policy context="default">'
                '<allow send_destination="*"/><allow receive_sender="*"/>'
                '<allow own="*"/></policy>'
                + (
                    f"<servicedir>{html.escape(str(services))}</servicedir>"
                    if services
                    else ""
                )
                + "</busconfig>"
            )
            return subprocess.run(
                [
                    str(dbus / "bin/dbus-run-session"),
                    "--config-file",
                    str(config),
                    "--dbus-daemon",
                    str(dbus / "bin/dbus-daemon"),
                    "--",
                    str(dconf / "bin/dconf"),
                    "load",
                    "/",
                ],
                input="[org/gnome/desktop/interface]\ngtk-theme='Adwaita-dark'\nicon-theme='Adwaita'\ncolor-scheme='prefer-dark'\n",
                env=environment,
                capture_output=True,
                text=True,
                timeout=15,
            )

        broken = invoke(None)
        assert broken.returncode != 0 and "ServiceUnknown" in broken.stderr, (
            broken.stderr
        )
        fixed = invoke(dconf / "share/dbus-1/services")
        assert fixed.returncode == 0, fixed.stderr
        for key, value in {
            "gtk-theme": "Adwaita-dark",
            "icon-theme": "Adwaita",
            "color-scheme": "prefer-dark",
        }.items():
            result = subprocess.run(
                [
                    str(dconf / "bin/dconf"),
                    "read",
                    "/org/gnome/desktop/interface/" + key,
                ],
                env=environment,
                check=True,
                capture_output=True,
                text=True,
                timeout=5,
            )
            assert result.stdout.strip() == repr(value), (key, result.stdout)
    print(
        "Reproduced missing-service failure; installing dconf's activation service allowed all theme settings to persist"
    )


if __name__ == "__main__":
    main()
