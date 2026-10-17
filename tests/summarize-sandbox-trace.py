#!/usr/bin/env python3
"""Summarize trace-cmd reports written by profile-sandbox-startup.py.

Run with the directory containing samples.json and the *.trace reports. CPU
accounting comes from the untraced samples, not inferred scheduling intervals:
a vCPU can be descheduled by its host while a task appears to be running.
"""

import argparse
import collections
import json
import re
import statistics
from pathlib import Path

EVENT = re.compile(r"-(\d+)\s+\[\d+\].*?\s(\d+\.\d+): (\w+):\s*(.*)")


def summarize(directory, profile, samples):
    events = []
    for line in (directory / f"{profile}.trace").read_text().splitlines():
        if match := EVENT.search(line):
            events.append((int(match[1]), float(match[2]), match[3], match[4]))
    start = next(t for _, t, _, data in events if "SANDBOX_START" in data)
    end = next(t for _, t, _, data in events if "SANDBOX_READY" in data)
    traced_sample = json.loads(
        (directory / f"{profile}-trace-sample.txt").read_text().splitlines()[0]
    )
    supervisor = traced_sample["supervisor_pid"]
    parents = {}
    for _, _, kind, data in events:
        if kind == "sched_process_fork":
            fields = dict(re.findall(r"(\w+)=([^ ]+)", data))
            parents[int(fields["child_pid"])] = int(fields["parent_pid"])

    def belongs_to_service(pid):
        seen = set()
        while pid not in seen:
            if pid == supervisor:
                return True
            seen.add(pid)
            pid = parents.get(pid, 0)
        return False

    # Follow the supervisor's process tree, excluding the recorder and the
    # later systemctl inventory queries. PIDs in the trace are VM-global.
    executions = []
    pending_calls = {}
    calls = collections.defaultdict(list)
    for pid, timestamp, kind, data in events:
        if not start <= timestamp <= end or not belongs_to_service(pid):
            continue
        if kind == "sched_process_exec":
            filename = dict(re.findall(r"(\w+)=([^ ]+)", data))["filename"]
            executions.append((pid, timestamp, filename))
        elif kind.startswith("sys_enter_"):
            pending_calls[(pid, kind.removeprefix("sys_enter_"))] = timestamp
        elif kind.startswith("sys_exit_"):
            name = kind.removeprefix("sys_exit_")
            if entered := pending_calls.pop((pid, name), None):
                calls[name].append(timestamp - entered)

    nspawn = next(t for _, t, name in executions if name.endswith("/systemd-nspawn"))
    init = next(t for _, t, name in executions if name.endswith("/init"))
    init_pid = next(pid for pid, t, _ in executions if t == init)
    systemd = next(
        t
        for pid, t, name in executions
        if pid == init_pid and name.endswith("/systemd")
    )
    return {
        "untraced_medians": {
            key: statistics.median(sample[key] for sample in samples)
            for key in ["wall_seconds", "service_cpu_seconds", "client_cpu_seconds"]
        },
        "trace_wall_seconds": end - start,
        "service_exec_events": len(executions),
        "distinct_executing_pids": len({pid for pid, _, _ in executions}),
        # This interval includes namespace/mount setup up to guest init exec,
        # not booting NixOS or systemd. It is a separate traced observation.
        "nspawn_exec_to_guest_init_seconds": init - nspawn,
        "guest_init_to_systemd_exec_seconds": systemd - init,
        "completed_syscalls": {
            name: {"count": len(durations), "total_seconds": sum(durations)}
            for name, durations in calls.items()
        },
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path)
    args = parser.parse_args()
    samples = json.loads((args.directory / "samples.json").read_text())
    print(
        json.dumps(
            {
                profile: summarize(args.directory, profile, values)
                for profile, values in samples.items()
            },
            indent=2,
        )
    )


if __name__ == "__main__":
    main()
