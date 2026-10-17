"""Collect detailed startup accounting and kernel traces in an isolated VM."""

machine.start()
machine.wait_for_unit("multi-user.target")
machine.wait_for_unit("home-manager-alice.service")
machine.wait_until_succeeds("test -S /run/user/1000/wayland-test")
machine.wait_until_succeeds("test -S /run/user/1000/pipewire-0")
machine.wait_until_succeeds("test -S /run/user/1000/pulse/native")

print(machine.succeed(f"{DESKTOP_PYTHON} {PROFILE_SCRIPT}", timeout=300))
machine.copy_from_machine("/tmp/startup-profile", "startup-profile")
