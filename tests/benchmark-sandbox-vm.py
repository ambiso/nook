"""Run the host benchmark in the same headless VM used by the integration test."""

import json
import shlex

machine.start()
machine.wait_for_unit("multi-user.target")
machine.wait_for_unit("home-manager-alice.service")
machine.wait_until_succeeds("test -S /run/user/1000/wayland-test")
machine.wait_until_succeeds("test -S /run/user/1000/pipewire-0")
machine.wait_until_succeeds("test -S /run/user/1000/pulse/native")

command = (
    f"WAYLAND_DISPLAY=wayland-test {DESKTOP_PYTHON} {BENCHMARK_SCRIPT} "
    "check --runs 6 --output /home/alice/startup-benchmark.json"
)
print(machine.succeed("su - alice -c " + shlex.quote(command)))
results = json.loads(machine.succeed("cat /home/alice/startup-benchmark.json"))
print("SANDBOX_BENCHMARK=" + json.dumps(results, sort_keys=True))
