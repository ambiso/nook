"""Render the real Nook selector in KGX on the private display from Xvfb."""

import os
import shlex
import shutil
import subprocess
import sys
import tempfile
import time
from contextlib import contextmanager
from pathlib import Path

# profile, instance, uptime in seconds, CPU percent, RAM MiB, swap MiB
RUNNING = [
    ("dev", "main", 2537, 4, 1413, 0),
    ("dev", "n-web-api", 615, 18, 528, 24),
    ("staging", "e-1a6e8b09d12cf207", 135, 1, 181, 0),
    ("postgres", "main", 4398, 2, 312, 0),
    ("steam", "main", 1264, 7, 2324, 96),
]
PROFILES = ("dev", "postgres", "signal", "staging", "steam", "zotero")
SAVED = (("dev", "n-scratch"), ("dev", "n-web-api"))
MIB = 1024 * 1024
WINDOW_WIDTH, WINDOW_HEIGHT = 1100, 430
MARGIN = 36


def unit(profile, instance):
    return f"sandbox-1234-{profile}@{instance}.service"


def systemctl(root, command, *args):
    """Serve only the read operations needed by the selector."""
    running = {unit(profile, instance) for profile, instance, *_ in RUNNING}
    if command == "list-units":
        for service in sorted(running):
            print(f"{service} loaded active running Nook")
    elif command == "is-active":
        return int(args[-1] not in running)
    elif command == "show":
        now = time.monotonic_ns()
        for profile, instance, uptime, cpu, _ram, swap in RUNNING:
            service = unit(profile, instance)
            started = max(1, now // 1000 - uptime * 1_000_000)
            print(
                f"Id={service}\nActiveState=active\nInvocationID={service}\n"
                f"ActiveEnterTimestampMonotonic={started}\n"
                f"CPUUsageNSec={now * cpu // 100}\nControlGroup=/{service}\n"
                f"MemorySwapCurrent={swap * MIB}\n"
            )
        with (root / "samples").open("a") as samples:
            print("sample", file=samples)
    else:
        return 1
    return 0


def executable(path, contents):
    path.write_text(contents)
    path.chmod(0o755)


def prepare_demo(root, nk):
    for name in PROFILES:
        profile = root / "profiles" / name
        profile.mkdir(parents=True)
        (profile / "profile").write_text(
            f"name\t{name}\nusername\talice\nuid\t1234\ngid\t1234\n"
            f"state\t{root / 'state' / name}\npersistent\t1\n"
        )
        for filename in (
            "mounts",
            "environment",
            "session-rules",
            "system-rules",
            "pasta",
        ):
            (profile / filename).touch()
    for profile, instance in SAVED:
        (root / "state" / profile / "instances" / instance / "home").mkdir(parents=True)
    for profile, instance, _uptime, _cpu, ram, _swap in RUNNING:
        group = root / "cgroup" / unit(profile, instance)
        group.mkdir(parents=True)
        inactive_file = 256 * MIB
        (group / "memory.current").write_text(f"{ram * MIB + inactive_file}\n")
        (group / "memory.stat").write_text(f"inactive_file {inactive_file}\n")

    # The installed wrapper puts real systemctl first. Give the actual Nook
    # binary a private PATH and cgroup tree containing only the demo data.
    (root / "bin").mkdir()
    shim = shlex.join(
        [sys.executable, str(Path(__file__).resolve()), "--systemctl", str(root)]
    )
    bash = shutil.which("bash")
    executable(root / "bin/systemctl", f'#!{bash}\nexec {shim} "$@"\n')
    command = [
        shutil.which("bwrap"),
        "--unshare-user",
        "--die-with-parent",
        "--ro-bind",
        "/nix/store",
        "/nix/store",
        "--dev-bind",
        "/dev",
        "/dev",
        "--proc",
        "/proc",
        "--bind",
        str(root),
        str(root),
        "--ro-bind",
        str(root / "cgroup"),
        "/sys/fs/cgroup",
        "--chdir",
        str(root),
        "--setenv",
        "PATH",
        str(root / "bin"),
        "--",
        nk,
        "--profiles",
        str(root / "profiles"),
    ]
    executable(root / "launch-demo", f'#!{bash}\nexec {shlex.join(command)} "$@"\n')


def xdotool(*args):
    return subprocess.check_output(["xdotool", *args], text=True, timeout=12).strip()


@contextmanager
def managed_process(command, **kwargs):
    process = subprocess.Popen(command, **kwargs)
    try:
        yield process
    finally:
        if process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=3)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()


def capture(root, output):
    config = root / "config/glib-2.0/settings"
    config.mkdir(parents=True)
    (config / "keyfile").write_text(
        "[org/gnome/Console]\ntheme='night'\nuse-system-font=false\n"
        f"custom-font='Adwaita Mono 14'\nlast-window-size=({WINDOW_WIDTH}, {WINDOW_HEIGHT})\n"
    )
    env = dict(
        os.environ,
        XDG_CONFIG_HOME=str(root / "config"),
        GSETTINGS_BACKEND="keyfile",
        GDK_BACKEND="x11",
        GSK_RENDERER="cairo",
        GTK_A11Y="none",
    )
    background = root / "background.png"
    width, height = map(int, xdotool("getdisplaygeometry").split())
    window_x = (width - WINDOW_WIDTH) // 2
    window_y = (height - WINDOW_HEIGHT) // 2
    crop = f"{WINDOW_WIDTH + 2 * MARGIN}x{WINDOW_HEIGHT + 2 * MARGIN}+{window_x - MARGIN}+{window_y - MARGIN}"
    subprocess.run(
        [
            "magick",
            "-size",
            f"{width}x{height}",
            "gradient:#273348-#151924",
            str(background),
        ],
        check=True,
    )
    # GTK draws its own rounded corners and shadows when a compositor is present.
    with (
        managed_process(["xcompmgr", "-n"]) as compositor,
        (root / "kgx.log").open("w+") as log,
    ):
        time.sleep(0.15)
        if compositor.poll() is not None:
            raise RuntimeError("the offscreen X11 compositor exited")
        # Use an ordinary backing window because Xvfb has no desktop wallpaper.
        with (
            managed_process(
                [
                    "feh",
                    "--borderless",
                    "--geometry",
                    f"{width}x{height}+0+0",
                    "--title",
                    "Nook backdrop",
                    str(background),
                ]
            ),
            managed_process(
                ["kgx", "--title=Nook", "--", str(root / "launch-demo")],
                env=env,
                stdout=log,
                stderr=log,
            ) as kgx,
        ):
            try:
                xdotool(
                    "search", "--sync", "--onlyvisible", "--name", "^Nook backdrop$"
                )
                window = xdotool(
                    "search", "--sync", "--onlyvisible", "--name", "^Nook$"
                ).splitlines()[0]
                xdotool(
                    "windowsize",
                    "--sync",
                    window,
                    str(WINDOW_WIDTH),
                    str(WINDOW_HEIGHT),
                )
                xdotool("windowmove", "--sync", window, str(window_x), str(window_y))
                xdotool("windowraise", window)
                xdotool("windowfocus", window)
                # CPU has a value only after the second systemd sample.
                samples = root / "samples"
                until = time.monotonic() + 10
                while not samples.exists() or len(samples.read_text().splitlines()) < 2:
                    if kgx.poll() is not None or time.monotonic() >= until:
                        raise RuntimeError(
                            "Nook did not finish its second resource sample"
                        )
                    time.sleep(0.05)
                time.sleep(0.2)  # Let GTK paint the frame containing that sample.
                subprocess.run(
                    [
                        "magick",
                        "import",
                        "-silent",
                        "-window",
                        "root",
                        "-crop",
                        crop,
                        "+repage",
                        "-strip",
                        f"PNG24:{output}",
                    ],
                    check=True,
                )
            except Exception:
                log.seek(0)
                print(log.read(), file=sys.stderr, end="")
                raise


def main():
    if sys.argv[1] == "--systemctl":
        return systemctl(Path(sys.argv[2]), *sys.argv[3:])
    nk, output = sys.argv[1:]
    with tempfile.TemporaryDirectory(prefix="nook-demo-") as temporary:
        root = Path(temporary)
        prepare_demo(root, nk)
        capture(root, output)
    return 0


if __name__ == "__main__":
    sys.exit(main())
