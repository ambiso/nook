# Development

The Rust launcher lives in `src/`. `nix/home-manager.nix` defines profiles and
their guest configurations; `nix/nixos.nix` installs the host services and
authorization rules; `nix/container.nix` configures the guest system.

## Verification of the extraction

On 2026-10-17, the extracted Rust code passed all 16 native tests, strict Clippy,
and 15 Miri tests. The Linux `openat` test was skipped under Miri and passed
natively. A PTY smoke test exercised the new `nk` binary's naming prompt,
saved-instance selection, template selection, ephemeral creation, and quit.
These checks ran inside the existing dev sandbox. Cargo-deny's license check
passed online and offline, including with vendored crates and an empty Cargo
cache. A separate user-namespace fixture exercised the entry helper with mocked
services: it selected the running generation's shell, GUI command, username,
and working directory and rejected mismatched owners and profiles.

`nix flake check --no-build --all-systems` passed, and the original host's full
configuration evaluated using the extracted module. On 2026-10-18, the full
x86_64 integration suite passed with KVM: the VM script took 131.28 seconds,
and evaluation, configuration builds, and execution finished in about five
minutes. This included rebuild preservation, automatic host IDs, dconf, cursor
paths, GTK/Qt portals, networking, hotplug devices, nested Podman, and CLI/TUI
instance management. The aarch64 VM derivation has only been evaluated. Earlier
performance measurements in the benchmark notes predate the extraction.

The instance-deletion addition passed 22 native tests, 16 Miri tests, Clippy,
the CLI/TUI smoke checks, and a bind-mount refusal check in a nested user/mount
namespace. Six Linux-specific tests were excluded from Miri and passed natively.

The runtime host-identity change passed 27 native tests, 19 Miri tests, Clippy,
and the CLI/TUI and deletion checks. Two additional tests (NSS account lookup
and reservation migration locks) are marked unsupported under Miri and passed
natively. The entry-helper fixture runs as namespace root inside bubblewrap;
it checks the running generation, caller ownership, and pinned host UID/GID
without invoking a real host service. Module checks cover automatic UID/GID
allocation and migration errors for removed host-ID overrides.

The dconf activation fix was checked with two private D-Bus sessions inside dev:
an empty service registry reproduces `ServiceUnknown`, and registering dconf's
service allows theme, icon, and color-scheme writes to persist in a disposable
home. Module assertions cover the guest D-Bus/systemd registration.

The preference/cursor behavior has Nix assertions for module and profile
overrides and unset variables. The cursor smoke test also ran inside dev using
real transient systemd services and libXcursor, with a disposable home in
bubblewrap. A single theme directory and an unexpanded `$HOME` path both fail
to find the fixture cursor; the evaluated NixOS search path finds it. The full
VM run also verified cursor paths in a booted Nook guest.

To repeat that focused runtime check, supply a libXcursor library, an installed
Xcursor-format file (such as Adwaita's `cursors/left_ptr`), and JSON containing
your guest's evaluated `environment.variables`. The guest home in that
configuration must match the user running the test:

```sh
python3 tests/cursor-smoke.py LIBXCURSOR_SO CURSOR_FILE ENVIRONMENT_JSON
```

The test needs `systemd-run` and bubblewrap inside a running development sandbox;
`--bubblewrap PATH` selects a bubblewrap executable outside `PATH`.

## Rust checks

Run these commands from the repository root inside a development sandbox:

```sh
nix develop
cargo fmt --all -- --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo deny --locked check licenses
cargo build --locked
python3 tests/tui-smoke.py target/debug/nk
python3 tests/delete-mount-smoke.py target/debug/nk
python3 tests/entry-identity-smoke.py target/debug/nook-root
python3 tests/dconf-smoke.py
```

`nix develop` provides build tools; it does not itself isolate execution from
your host. The tests create temporary files and exercise mount-path validation,
instance naming, current-directory translation, and portal request routing.
The PTY smoke test uses a temporary profile catalog and a fake `systemctl`; it
checks the real selector without starting services or invoking a root helper.
The deletion tests use disposable directories. They check confirmation and
cancellation, refusal of active instances, symlink and replacement guards, and
a same-filesystem bind mount inside a temporary user/mount namespace.

`deny.toml` checks the locked Rust dependency graph, including build and test
dependencies, against a license allowlist. An unlisted or missing license fails
the check. The package's Nix build runs the same check offline against its
vendored crates. This covers Cargo dependencies, not the native libraries and
tools supplied by Nix; it is not an automatic legal compatibility proof.

For Miri, install the `miri` component of `nightly-2026-09-17` using rustup, then
run inside the sandbox:

```sh
unset RUSTC RUSTDOC RUSTC_WRAPPER
TMPDIR=/tmp MIRIFLAGS="-Zmiri-disable-isolation -Zmiri-env-forward=TMPDIR" rustup run nightly-2026-09-17 cargo miri test --locked -- \
  --skip supervise::tests::nested_mountpoint_creation_cannot_follow_symlinks \
  --skip storage::tests::deleting_one_home_preserves_neighbors_and_symlink_targets \
  --skip storage::tests::symlink_instance_roots_are_refused \
  --skip storage::tests::mounts_are_rejected_before_any_removal \
  --skip storage::tests::lifecycle_lock_excludes_startup_and_deletion \
  --skip storage::tests::replacing_a_confirmed_directory_is_refused \
  --skip completion::tests::custom_catalog_and_selected_profile_control_instance_candidates
```

Filesystem access is needed for the temporary-directory tests. The skipped
tests use Linux `openat`, `openat2`, directory iteration, or file locks that Miri
does not implement; they run in the native test suite instead. NSS lookup and
reservation migration also have Miri-specific ignores and run natively. The
completion test above invokes `systemctl`, whose process-spawning API is not
implemented by Miri, and also runs natively.
Use a stable `TMPDIR` when building Miri artifacts. If cached
artifacts retain a deleted directory from an earlier Nix shell, rerun with a
fresh `CARGO_TARGET_DIR`.

## NixOS integration test

```sh
nix build --print-build-logs .#checks.x86_64-linux.integration
```

This boots a headless NixOS VM and exercises the installed launcher and modules:
systemd ownership and readiness, Home Manager activation and caching, persistent
and ephemeral homes, the TUI, directory translation, nested Podman and FHS
execution, Wayland and private Xwayland, audio, devices, networking, desktop
metadata, and portals. The portal frontend and document store are real; a test
backend supplies deterministic consent responses. These tests do not replace
checking a real desktop's consent dialogs, GPU applications, or controller input.

The flake exports packages and checks for `x86_64-linux` and `aarch64-linux`.
Development and runtime testing have used x86_64; the aarch64 outputs have not
had equivalent runtime testing.

Format Nix with `nix fmt`. The Python fixtures use Ruff formatting. VM driver
scripts receive `machine` and fixture constants from Nix, so standalone Python
linters will report those injected names as undefined.
