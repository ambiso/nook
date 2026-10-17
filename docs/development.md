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
configuration evaluated using the extracted module. The VM test derivations
were evaluated for both architectures, but the full VM suite has not been
re-run on the extraction. Earlier desktop use, VM runs, and the measurements in
the benchmark notes predate this extraction. The initial extraction has been
deployed on the original host; the rebuild-preservation change has not. A VM
regression for switching configurations without restarting an instance is
included but has only been evaluated.

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
```

`nix develop` provides build tools; it does not itself isolate execution from
your host. The tests create temporary files and exercise mount-path validation,
instance naming, current-directory translation, and portal request routing.
The PTY smoke test uses a temporary profile catalog and a fake `systemctl`; it
checks the real selector without starting services or invoking a root helper.

`deny.toml` checks the locked Rust dependency graph, including build and test
dependencies, against a license allowlist. An unlisted or missing license fails
the check. The package's Nix build runs the same check offline against its
vendored crates. This covers Cargo dependencies, not the native libraries and
tools supplied by Nix; it is not an automatic legal compatibility proof.

For Miri, install the `miri` component of `nightly-2026-09-17` using rustup, then
run inside the sandbox:

```sh
unset RUSTC RUSTDOC RUSTC_WRAPPER
TMPDIR=/tmp MIRIFLAGS="-Zmiri-disable-isolation -Zmiri-env-forward=TMPDIR" rustup run nightly-2026-09-17 cargo miri test --locked -- --skip supervise::tests::nested_mountpoint_creation_cannot_follow_symlinks
```

Filesystem access is needed for the temporary-directory tests. The skipped
test uses Linux `openat`, which Miri does not implement; it runs in the native
test suite instead. Use a stable `TMPDIR` when building Miri artifacts. If cached
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
