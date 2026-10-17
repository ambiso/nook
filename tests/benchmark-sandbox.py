#!/usr/bin/env python3
"""Measure installed sandbox startup without restarting an existing instance.

Run on the host with nixpkgs Python. Each profile gets a temporary named
instance: its home survives the measured restarts, then is removed. Explicit
profile mounts remain shared, but no application is started in the instance.
"""

import argparse
import json
import os
import pwd
import secrets
import shutil
import statistics
import subprocess
import time
from pathlib import Path


def run(*command):
    return subprocess.run(command, check=True, capture_output=True, text=True).stdout


def properties(output):
    values = {}
    for line in output.splitlines():
        key, value = line.split("=", 1)
        values[key] = int(value)
    return values


def timed_launch(launcher, profile, instance):
    started = time.monotonic()
    run(launcher, "launch", profile, "--instance", instance, "--", "true")
    return started, time.monotonic()


def measure(launcher, profile, instance):
    started, finished = timed_launch(launcher, profile, instance)
    unit = f"sandbox-u{pwd.getpwuid(os.getuid()).pw_name}-{profile}@{instance}.service"
    host = properties(
        run(
            "systemctl",
            "show",
            unit,
            "-p",
            "InactiveExitTimestampMonotonic",
            "-p",
            "ActiveEnterTimestampMonotonic",
        )
    )
    guest = properties(
        run(
            launcher,
            "launch",
            profile,
            "--join",
            "--instance",
            instance,
            "--",
            "systemctl",
            "show",
            "sandbox-ready.service",
            "-p",
            "ExecMainStartTimestampMonotonic",
            "-p",
            "ExecMainExitTimestampMonotonic",
        )
    )
    entries = [
        json.loads(line)
        for line in run(
            "journalctl", "-b", "-u", unit, "-o", "json", "--no-pager"
        ).splitlines()
    ]
    service_started = host["InactiveExitTimestampMonotonic"]

    def marker(message):
        # Host and nspawn share CLOCK_MONOTONIC. Journal timestamps are receipt
        # times, so these phase boundaries include small console logging delays.
        for entry in entries:
            timestamp = int(entry["__MONOTONIC_TIMESTAMP"])
            content = entry.get("MESSAGE", "")
            # journald represents console messages containing ANSI escapes as
            # byte arrays rather than JSON strings.
            if isinstance(content, list):
                content = bytes(content).decode("utf-8", errors="replace")
            if timestamp >= service_started and message in content:
                return timestamp
        raise RuntimeError(f"Missing journal marker {message!r} in {unit}")

    stage2 = marker("<<< NixOS Stage 2 >>>")
    activation = marker("running activation script...")
    systemd = marker("starting systemd...")
    readiness = guest["ExecMainStartTimestampMonotonic"]
    ready = guest["ExecMainExitTimestampMonotonic"]
    active = host["ActiveEnterTimestampMonotonic"]
    boundaries = [
        started * 1e6,
        service_started,
        stage2,
        activation,
        systemd,
        readiness,
        ready,
        active,
        finished * 1e6,
    ]
    names = [
        "request",
        "supervisor_and_nspawn",
        "early_init",
        "nixos_activation",
        "systemd_to_user_manager",
        "readiness_and_home_manager",
        "readiness_handoff",
        "command_entry",
    ]
    phases = {
        name: (end - start) / 1e6
        for name, start, end in zip(names, boundaries, boundaries[1:])
    }
    phases["total"] = finished - started
    # This is a subtraction, not a measurement of a cached-root implementation.
    # A real cache also has preparation costs and must restore volatile state.
    phases["total_excluding_nixos_activation"] = (
        phases["total"] - phases["nixos_activation"]
    )
    return phases


def benchmark(launcher, profile, runs):
    instance = "n-bench-" + secrets.token_hex(4)
    manifest = Path(
        f"/etc/sandbox/profiles/u{pwd.getpwuid(os.getuid()).pw_name}/{profile}/profile"
    )
    values = dict(line.split("\t", 1) for line in manifest.read_text().splitlines())
    if values["persistent"] != "1":
        raise ValueError(
            "This benchmark requires a persistent profile to measure home reuse"
        )
    home = Path(values["state"]) / "instances" / instance
    if home.exists():
        raise FileExistsError(home)
    print(f"{profile}: isolated instance {instance}", flush=True)
    samples = []
    warm = []
    try:
        for index in range(runs):
            samples.append(measure(launcher, profile, instance))
            print(f"  start {index + 1}: {samples[-1]['total']:.3f}s", flush=True)
            if index == 0:
                # This first launch initializes a clean home. The following
                # restarts use its existing Home Manager generation and files.
                for _ in range(runs):
                    start, end = timed_launch(launcher, profile, instance)
                    warm.append(end - start)
            run(launcher, "stop", profile, "--instance", instance)
    finally:
        # Never remove a home while its services might still be using it.
        run(launcher, "stop", profile, "--instance", instance)
        if home.exists():
            shutil.rmtree(home)
    return {"cold_seconds": samples, "warm_seconds": warm}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("profiles", nargs="+", help="Installed profiles, e.g. dev vr")
    parser.add_argument("--launcher", default="nk")
    parser.add_argument("--runs", type=int, default=5)
    parser.add_argument(
        "--output", type=Path, required=True, help="Save raw measurements as JSON"
    )
    args = parser.parse_args()
    if args.runs < 2:
        parser.error(
            "--runs must be at least 2 (one clean-home and one reused-home start)"
        )
    results = {}
    for profile in args.profiles:
        results[profile] = benchmark(args.launcher, profile, args.runs)
        # Save each completed profile, including when a later profile fails.
        args.output.write_text(json.dumps(results, indent=2) + "\n")
        reused = results[profile]["cold_seconds"][1:]
        print("  Median across cold starts with an existing home:")
        for phase in reused[0]:
            print(
                f"    {phase}: {statistics.median(sample[phase] for sample in reused):.3f}s"
            )
        print(
            f"  Median warm command: {statistics.median(results[profile]['warm_seconds']):.3f}s"
        )


if __name__ == "__main__":
    main()
