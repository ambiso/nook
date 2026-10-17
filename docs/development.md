# Development

The Rust launcher lives in `src/`. `nix/home-manager.nix` defines profiles and
their guest configurations; `nix/nixos.nix` installs the host services and
authorization rules; `nix/container.nix` configures the guest system.

## Verification of the extraction

On 2026-10-17, the extracted Rust code passed all 15 native tests, strict Clippy,
and 14 Miri tests. The Linux `openat` test was skipped under Miri and passed
natively. A PTY smoke test exercised the new `nk` binary's naming prompt,
saved-instance selection, template selection, ephemeral creation, and quit.
These checks ran inside the existing dev sandbox; its older launcher was used
only to enter that environment.

`nix flake check --no-build --all-systems` passed, and the original host's full
configuration evaluated using the extracted module. The VM test derivations
were evaluated for both architectures, but the full VM suite has not been
re-run on the extraction. Earlier desktop use, VM runs, and the measurements in
the benchmark notes predate this extraction. The extracted modules have not yet
been deployed to the host.

## Rust checks

Run these commands from the repository root inside a development sandbox:

```sh
nix develop
cargo fmt --all -- --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo build --locked
python3 tests/tui-smoke.py target/debug/nk
```

`nix develop` provides build tools; it does not itself isolate execution from
your host. The tests create temporary files and exercise mount-path validation,
instance naming, current-directory translation, and portal request routing.
The PTY smoke test uses a temporary profile catalog and a fake `systemctl`; it
checks the real selector without starting services or invoking a root helper.

For Miri, install the `miri` component of `nightly-2026-09-17` using rustup, then
run inside the sandbox:

```sh
unset RUSTC RUSTDOC RUSTC_WRAPPER
MIRIFLAGS=-Zmiri-disable-isolation cargo +nightly-2026-09-17 miri test --locked -- --skip supervise::tests::nested_mountpoint_creation_cannot_follow_symlinks
```

Filesystem access is needed for the temporary-directory tests. The skipped
test uses Linux `openat`, which Miri does not implement; it runs in the native
test suite instead.

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
