# Startup measurements

These measurements were recorded before Nook was extracted from a personal
NixOS configuration. They describe the synthetic headless fixture, not a promise
about your machine. Re-run the tools against the version and profiles you use.

To benchmark the installed launcher on the host as its owner, without restarting
your existing instances:

```sh
python3 tests/benchmark-sandbox.py dev vr --runs 6 --output /tmp/sandbox-startup.json
```

The script creates temporary named instances, measures a clean-home start, then
five cold starts reusing that home, plus six warm command launches. It stops
those instances and removes their homes afterwards. Explicit profile mounts
remain shared; only `true` and timing queries are launched. Results separate
supervisor/nspawn setup, early init, NixOS activation, systemd boot to the user
manager, readiness/Home Manager, readiness handoff, and command entry. Console
boundaries use journal receipt timestamps, so small logging delays are included.
The total excluding NixOS activation is a subtraction from the measured total;
it does not measure an implemented activation cache. NixOS activation still runs
at every cold start with the current disposable root.
The benchmark reader needs access to the host system journal, for example
through membership in `systemd-journal`.

The VM integration fixture also accepts `benchmark = true` to run the same
benchmark against its installed launcher and guest profile. This supports
before/after comparisons without replacing the host's system configuration.

Paired measurements on 2026-10-17 used the headless fixture with four vCPUs and
4 GiB RAM. Each version measured five cold starts after initializing the home,
plus six commands in a running instance:

| VM measurement (median) | Before | After |
| --- | ---: | ---: |
| Cold start, existing home | 2.423 s | 1.867 s |
| Systemd boot to user manager | 990 ms | 885 ms |
| Readiness/Home Manager | 547 ms | 107 ms |
| Readiness handoff | 76 ms | 23 ms |
| Warm command | 187 ms | 182 ms |

Cold startup improved by 23%. Fresh homes still run full Home Manager activation.
Run the host benchmark after rebuilding to measure the desktop configuration;
the figures above describe the VM fixture.

For CPU accounting, unit inventories and kernel traces, import the same test
fixture with `diagnostics = true`. Run it inside the dev sandbox, from the
repository root:

```sh
nix build --impure --expr '
  let flake = builtins.getFlake (toString ./.);
  in import ./tests/sandbox-systemd.nix {
    pkgs = import flake.inputs.nixpkgs { system = "x86_64-linux"; };
    home-manager = flake.inputs.home-manager;
    diagnostics = true;
  }
' --out-link result-startup-profile
python3 tests/summarize-sandbox-trace.py result-startup-profile/startup-profile/startup-profile
```

Each profile gets an initialized home and three untraced cold starts. A separate
trace records exec/fork/scheduling events and mount/sync syscalls, with explicit
markers around startup. The result retains raw traces, CPU accounting and
`systemd-analyze critical-chain` output. The full `check` profile includes
PostgreSQL, portals and private Xwayland; `no-portals` is a smaller profile with
neither Home Manager nor networking/audio. All data is synthetic test-VM data.

An additional paired run on 2026-10-17 measured these medians in the same
four-vCPU fixture:

| Profile | Cold start before | After boot cleanup | Service CPU before | After |
| --- | ---: | ---: | ---: | ---: |
| Full test profile | 1.902 s | 1.780 s | 1.590 s | 1.492 s |
| Smaller profile | 1.633 s | 1.519 s | 1.278 s | 1.203 s |

The cleanup disables log rotation and journal-catalog rebuilding by default for
the disposable root, and shows console status only for errors. Unit logs remain
in the guest journal, and activation output still reaches the host journal.
Profiles retaining system logs can override the logrotate default.

The full profile's baseline trace contained 226 exec events across 170 PIDs in
the supervisor's process tree. The smaller profile contained 181 across 139
PIDs. These include wrapper re-execs and short-lived activation helpers and
generators; counting only processes left after boot misses this work. Service
CPU accounting excludes the host manager, host journal and command client, so
it is not the total CPU cost of a request. Wall time alone is not a cycle count.

In these traces, nspawn exec to guest-init exec took 55–66 ms. That measures
namespace/mount setup until guest init, rather than booting NixOS. Guest init
to systemd exec took 333–348 ms, including about 265 ms of NixOS activation.
The full baseline's 247 mount syscalls totalled about 3 ms; these include
remounts and service mount namespaces, not 247 distinct shared mountpoints.
External `mount` process startup adds overhead beyond syscall duration. The
remaining boot chain includes tmpfiles, D-Bus/NSS, logind, the native user
manager and application-service readiness. These are VM measurements with a
shared store; they should not be treated as native-host instruction counts or
as proof that nspawn intrinsically needs seconds to start.
