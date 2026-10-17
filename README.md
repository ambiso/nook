![WARN: LLM GENERATED](https://img.shields.io/badge/WARN-LLM%20GENERATED-FF6347)

# Nook

Nook runs NixOS sandboxes managed by systemd. Define environments in Nix, then
use **`nk`** to open a shell, launch a desktop application, or create another
instance from the same configuration. Each instance has its own home, user
namespace, network namespace, and systemd managers. Closing a terminal leaves
the instance running.

The launcher and terminal selector are written in Rust. Sandboxes can run
ordinary systemd services, including user services configured with Home Manager.
Named instances keep their homes; ephemeral instances get clean temporary homes.
The guest root is temporary in both cases. A system service that stores data
under `/var/lib` needs an explicit persistent mount for that data.

## Install on NixOS

Nook targets current NixOS unstable, with cgroup v2 and support for ID-mapped
mounts. Desktop applications require a Wayland session. File dialogs and screen
sharing additionally require a working desktop portal backend on the host.

Add Nook alongside your existing `nixpkgs` and `home-manager` flake inputs:

```nix
inputs.nook = {
  url = "github:ambiso/nook";
  inputs.nixpkgs.follows = "nixpkgs";
  inputs.home-manager.follows = "home-manager";
};
```

Include `nook` in your flake's outputs arguments and add
`nook.nixosModules.default` to your system's modules. It includes the Home
Manager NixOS module and installs the host services and authorization rules.
Give sandbox owners explicit numeric UIDs in your NixOS user configuration.

In an existing Home Manager user configuration:

```nix
{ pkgs, lib, ... }:
{
  programs.nook = {
    enable = true;
    defaults = {
      shell = lib.mkDefault pkgs.bashInteractive;
      hostSessionBus = lib.mkDefault false;
    };
    sandboxes.dev = {
      packages = with pkgs; [ git ripgrep ];
      nix = true;
    };
    sandboxes.browser = {
      packages = [ pkgs.firefox ];
      guiEntryPoint = "/bin/firefox";
      portals = true;
    };
  };
}
```

Rebuild using your usual `nixos-rebuild switch` command. Restart running
instances when changing their packages, services, or configuration. The host
module derives the owner's UID and GID automatically. Shells are package options:
set `sandboxes.dev.shell = pkgs.fish`, for example. The default shell is Fish.

`defaults` and each sandbox are Nix modules: imports, merged definitions,
`lib.mkDefault`, and `lib.mkForce` work normally. Add guest system services with
`nixosModules` and guest user configuration with `homeManagerModules`.

## Use

```sh
nk                                    # interactive selector
nk launch dev                         # start or join dev's main instance
nk launch dev --name my-project        # independent persistent instance
nk launch dev --ephemeral              # new instance with a temporary home
nk launch dev -- git --version         # run a command
nk gui browser                        # launch the configured GUI application
nk list
nk stop dev --name my-project
```

In the selector, use arrows or **j/k** to select a row, **Enter** to launch/join,
**n** to create a named instance from that profile, **e** for an ephemeral
instance, **s** to stop, and **q** to quit. Named instances remain listed when
stopped. Names accept 1–24 lowercase letters, digits, or hyphens.

New instances reuse their profile's configuration and permissions, starting
with a clean home. They do not copy projects, credentials, or caches from the
main instance. Main homes persist by default; `persistent = false` makes only
the main home temporary. Named homes always persist. Exiting a shell leaves the
instance running; stopping an ephemeral instance deletes its home.
`nk list` shows its ID so you can rejoin
or stop it with `--instance e-…`. `--name project` is shorthand for
`--instance n-project`.

Homes are stored under `~/.sandbox/<profile>/home` for main instances and
`~/.sandbox/<profile>/instances/n-<name>/home` for named ones. Ephemeral homes
live on the host's `/run` tmpfs. Running `nk-dev` from inside dev's backing home
opens the corresponding guest directory; host symlink aliases work too.
GUI entry points also appear in desktop application search.

## Isolation and host access

Instances keep build output, sccache, and rootless Podman storage separate. The
Nix store is shared read-only. Container root maps to a high host UID, never
host root. Writable `bindMounts` deliberately grant shared access to their
sources, including to every named and ephemeral instance of that profile.

Only Wayland is forwarded from the host display. `startX = true` starts a private
Xwayland satellite inside each instance; the host's X11 sockets are not exposed.
GPU and audio access are enabled by default and can be disabled with `gpu` and
`audio`. Other devices are selected with `devices` or `devicePatterns`.

Interactive desktop portals are **off by default**. Enabling `portals` permits
host consent dialogs and access to selected host files through the document
portal. Host appearance settings are inherited separately by default; disable
`inheritAppearance` to turn off that separate connection when `portals` is also
disabled. `nix = true` grants access to the host Nix daemon with the owner's
permissions. Adding `homeManagerModules` also enables that connection for
activation, even with `nix = false`. `podman = true` enables rootless Podman inside
the instance.

Networking uses a separate namespace and pasta. Guest loopback is independent
of host loopback. Pasta can publish guest listeners on available host ports;
use `networkArgs` to change that behavior or `network = false` to disable
network access. See [architecture and configuration](docs/architecture.md) for
the D-Bus policies, storage model, permissions, and troubleshooting commands.

## Development

See [development and tests](docs/development.md) for the Rust environment, native
tests, Miri, and NixOS VM integration tests, and
[startup measurements](docs/benchmarks.md) for profiling tools and recorded results.

## AI assistance

Most of Nook's code and documentation was written with OpenAI Codex. It grew
out of a personal NixOS configuration, where development included desktop use,
Rust tests, Miri, and headless VM tests. The [development notes](docs/development.md)
distinguish checks on this extraction from earlier testing. Desktop dialogs,
GPU applications, and hardware still need testing on the system where you use them.
