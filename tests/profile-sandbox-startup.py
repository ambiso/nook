#!/usr/bin/env python3
"""Profile synthetic sandbox instances as root inside the test VM.

Untraced samples measure wall time and CPU accounting. A separate kernel trace
records transient executables and scheduling, rather than perturbing every
system call with ptrace. No real desktop home or application data is inspected.
"""

import argparse
import json
import os
import resource
import subprocess
import sys
import time
from pathlib import Path

INSTANCE = "n-profile"
LAUNCHER = "/etc/profiles/per-user/alice/bin/nk"
UNIT_FIELDS = [
    "Id",
    "ExecMainStartTimestampMonotonic",
    "ExecMainExitTimestampMonotonic",
    "ActiveEnterTimestampMonotonic",
    "CPUUsageNSec",
    "Result",
]


def run(*command):
    return subprocess.run(command, check=True, capture_output=True, text=True).stdout


def owner_command(profile, action="launch"):
    command = [
        "runuser",
        "-u",
        "alice",
        "--",
        "env",
        "WAYLAND_DISPLAY=wayland-test",
        LAUNCHER,
        action,
        profile,
        "--instance",
        INSTANCE,
    ]
    return command + ["--", "true"] if action == "launch" else command


def cpu_snapshot():
    # guest/guest_nice are already included in user/nice. Do not count twice.
    fields = Path("/proc/stat").read_text().splitlines()[0].split()[1:]
    ticks = [int(value) for value in fields]
    clock_ticks = os.sysconf("SC_CLK_TCK")
    return {
        "busy": sum(ticks[index] for index in [0, 1, 2, 5, 6]) / clock_ticks,
        "iowait": ticks[4] / clock_ticks,
        "steal": ticks[7] / clock_ticks,
    }


def properties(output):
    return dict(line.split("=", 1) for line in output.splitlines() if "=" in line)


def unit_inventory(machine, user=False):
    connection = ["systemctl", "--machine", f"alice@{machine}" if user else machine]
    if user:
        connection.append("--user")
    units = run(
        *connection, "list-units", "--type=service", "--all", "--plain", "--no-legend"
    ).splitlines()
    names = [line.split()[0] for line in units if line.strip()]
    output = run(
        *connection, "show", *names, *[f"--property={field}" for field in UNIT_FIELDS]
    )
    return [properties(block) for block in output.strip().split("\n\n")]


def sample(profile, markers=False):
    trace_marker = Path("/sys/kernel/tracing/trace_marker")
    if markers:
        trace_marker.write_text(f"SANDBOX_START {profile}\n")
    before_cpu = cpu_snapshot()
    before_client = resource.getrusage(resource.RUSAGE_CHILDREN)
    started = time.monotonic()
    run(*owner_command(profile))
    finished = time.monotonic()
    after_client = resource.getrusage(resource.RUSAGE_CHILDREN)
    after_cpu = cpu_snapshot()
    if markers:
        trace_marker.write_text(f"SANDBOX_READY {profile}\n")

    unit = f"sandbox-ualice-{profile}@{INSTANCE}.service"
    machine = f"sbx-1000-{profile}-{INSTANCE}"
    host = properties(
        run(
            "systemctl",
            "show",
            unit,
            "--property=CPUUsageNSec",
            "--property=TasksCurrent",
            "--property=MainPID",
        )
    )
    cpu_usage = host["CPUUsageNSec"]
    result = {
        "wall_seconds": finished - started,
        "service_cpu_seconds": int(cpu_usage) / 1e9 if cpu_usage.isdigit() else None,
        "client_cpu_seconds": (
            after_client.ru_utime
            + after_client.ru_stime
            - before_client.ru_utime
            - before_client.ru_stime
        ),
        "vm_cpu_seconds": {key: after_cpu[key] - before_cpu[key] for key in before_cpu},
        "tasks_after_boot": int(host["TasksCurrent"]),
        "supervisor_pid": int(host["MainPID"]),
        "system_units": unit_inventory(machine),
        "user_units": unit_inventory(machine, user=True),
        "critical_chain": run(
            "systemd-analyze",
            "--machine",
            machine,
            "critical-chain",
            "sandbox-ready.service",
        ),
    }
    return result


def trace(profile, output):
    data = output / f"{profile}.dat"
    events = [
        "sched:sched_process_exec",
        "sched:sched_process_fork",
        "sched:sched_process_exit",
        "sched:sched_switch",
        "sched:sched_wakeup",
        "syscalls:sys_enter_fsync",
        "syscalls:sys_exit_fsync",
        "syscalls:sys_enter_fdatasync",
        "syscalls:sys_exit_fdatasync",
        "syscalls:sys_enter_mount",
        "syscalls:sys_exit_mount",
    ]
    command = ["trace-cmd", "record", "-q", "-b", "16384", "-o", str(data)]
    for event in events:
        command += ["-e", event]
    command += ["--", sys.executable, __file__, "--single", profile]
    result = subprocess.run(command, capture_output=True, text=True, check=True)
    (output / f"{profile}-trace-sample.txt").write_text(result.stdout)
    report = run("trace-cmd", "report", "-R", "-i", str(data))
    (output / f"{profile}.trace").write_text(report)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--single", help=argparse.SUPPRESS)
    parser.add_argument("--output", type=Path, default=Path("/tmp/startup-profile"))
    args = parser.parse_args()
    if os.geteuid() != 0:
        parser.error("Run inside the test VM as root")
    if args.single:
        print(json.dumps(sample(args.single, markers=True)))
        return

    args.output.mkdir(parents=True, exist_ok=True)
    results = {}
    for profile in ["check", "no-portals"]:
        # Initialize each independent home before measuring repeated cold boots.
        run(*owner_command(profile))
        run(*owner_command(profile, "stop"))
        samples = []
        try:
            for _ in range(3):
                samples.append(sample(profile))
                run(*owner_command(profile, "stop"))
            trace(profile, args.output)
        finally:
            run(*owner_command(profile, "stop"))
        results[profile] = samples
        (args.output / "samples.json").write_text(json.dumps(results, indent=2) + "\n")
    print("STARTUP_PROFILE=" + json.dumps(results))


if __name__ == "__main__":
    main()
