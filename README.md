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

### 1. Add the flake input

Add Nook alongside your existing `nixpkgs` and `home-manager` flake inputs:

```nix
inputs.nook = {
  url = "github:ambiso/nook/stable";
  inputs.nixpkgs.follows = "nixpkgs";
  inputs.home-manager.follows = "home-manager";
};
```

### 2. Import the NixOS module and connect Home Manager

Add `nook` to your flake's outputs arguments, then put its **NixOS module** in
the system's `modules` list. For example, in `flake.nix`:

```nix
outputs = { nixpkgs, nook, ... }: {
  nixosConfigurations.my-host = nixpkgs.lib.nixosSystem {
    system = "x86_64-linux"; # or aarch64-linux
    modules = [
      ./configuration.nix
      nook.nixosModules.default
      {
        home-manager.useGlobalPkgs = true;
        home-manager.users.alice = import ./home.nix;
      }
    ];
  };
};
```

Use your existing system name, username, and configuration files in place of
`my-host`, `alice`, and these paths. The module includes Home Manager's NixOS
module, registers Nook's Home Manager options, and installs the host services
and authorization rules. Importing only `nook.homeManagerModules.default` does
not install those services.

Host UIDs and primary GIDs may be automatically assigned by NixOS. The launcher
resolves the configured owner account when starting an instance; guest IDs
default to 1000/100 independently of the host. Remove any old
`programs.nook.host.uid` or `.gid` overrides when updating from the numeric-ID
implementation.

For these desktop and development examples, enable unfree packages and graphics
on the host and use the host's package set in Home Manager. Steam and many games
need the 32-bit drivers on x86_64:

```nix
nixpkgs.config.allowUnfree = true;
home-manager.useGlobalPkgs = true;
hardware.graphics = {
  enable = true;
  enable32Bit = true;
};
```

### 3. Define profiles and rebuild NixOS

In `home.nix` (your Home Manager user configuration), the following defines five
profiles. GPU access, audio, networking, and persistent homes are enabled by
default. Each GUI profile installs an application entry in your desktop launcher.

```nix
{ pkgs, lib, ... }:
let
  controllers = [ "/dev/input" "/dev/uinput" "/dev/bus/usb" ];
in
{
  programs.nook = {
    enable = true;
    defaults = {
      shell = lib.mkDefault pkgs.bashInteractive;
      hostSessionBus = lib.mkDefault false;
    };
    sandboxes.dev = {
      packages = with pkgs; [ git ripgrep rustup qemu ];
      nix = true;
      podman = true;
      portals = true;
      startX = true;
      devices = [
        "/dev/kvm"
        # "/dev/kfd" # AMD ROCm compute; gpu = true already exposes /dev/dri.
        # NVIDIA CUDA device access (driver libraries are also needed):
        # "/dev/nvidia0" # Select the GPU; use nvidia1, etc. for other GPUs.
        # "/dev/nvidiactl"
        # "/dev/nvidia-uvm"
      ];
    };
    sandboxes.zotero = {
      packages = [ pkgs.zotero ];
      guiEntryPoint = "/bin/zotero";
      desktopFile = "zotero.desktop";
      portals = true;
      audio = false;
      environment.MOZ_ENABLE_WAYLAND = "1";
      # Let a host browser's Zotero Connector reach this instance.
      networkArgs = [
        "--no-map-gw" "--dns-forward" "169.254.1.1"
        "--tcp-ports" "127.0.0.1/23119" "--host-lo-to-ns-lo"
        "--udp-ports" "none" "--tcp-ns" "none" "--udp-ns" "none"
      ];
    };
    sandboxes.steam = {
      packages = [ pkgs.steam ];
      homeManagerModules = [
        {
          home.file.".icons/default".source = "${pkgs.kdePackages.breeze}/share/icons/breeze_cursors";
        }
      ];
      guiEntryPoint = "/bin/steam";
      desktopFile = "steam.desktop";
      startX = true;
      portals = true;
      devices = controllers;
      devicePatterns = [ "/dev/hidraw*" ];
    };
    sandboxes.heroic = {
      packages = [ pkgs.heroic ];
      guiEntryPoint = "/bin/heroic";
      desktopFile = "com.heroicgameslauncher.hgl.desktop";
      startX = true;
      portals = true;
      devices = controllers;
      devicePatterns = [ "/dev/hidraw*" ];
    };
    sandboxes.signal = {
      packages = [ pkgs.signal-desktop ];
      guiEntryPoint = "/bin/signal-desktop";
      desktopFile = "signal.desktop";
      portals = true;
      hostSessionBus = true;
      sessionBusPolicies."org.freedesktop.secrets" = "talk";
    };
  };
}
```

Steam and Heroic use private Xwayland for games; they do not receive the host's
X11 socket. Controller access includes the selected USB and input devices, so
trim those lists to suit your hardware. Signal's secrets policy allows it to
use the host keyring. Remove it and disable `hostSessionBus` if that access is
unwanted. These examples keep app data in each profile's own home. Moving an
existing installation's data requires copying it or adding an explicit mount.
Zotero's Connector port can be published by only one instance at a time.

Keep your existing `home.stateVersion` setting; if this is a new Home Manager
configuration, set it before rebuilding. Activate the complete NixOS
configuration, then launch a profile:

```sh
sudo nixos-rebuild switch --flake .#my-host
nk launch dev
```

A standalone `home-manager switch` installs launchers but cannot install the
required host services. If the service is missing despite the NixOS import,
check that the system and Home Manager use the same Nook version and profile
configuration, then rebuild NixOS. On the old numeric-ID implementation, forcing
only `programs.nook.host.uid` could make the launcher request a different service
from the one generated by NixOS.

Running instances are not restarted by a NixOS rebuild. When the host-supervisor
contract is unchanged, Nook reloads the matching sandbox services: the guest runs
`switch-to-configuration switch`, then activates its new Home Manager generation
and repins the running profile. Changing bind mounts, devices, network, D-Bus
proxies, host access, persistent-home placement, UID mapping, or other launch-time
settings skips that instance's reload and logs `restart required` without failing
the host switch. Stop and launch the instance to apply those changes. Shells
are package options: set `sandboxes.dev.shell = pkgs.fish`, for example. The
default shell is Fish.

`defaults` and each sandbox are Nix modules: imports, merged definitions,
`lib.mkDefault`, and `lib.mkForce` work normally. Add guest system services with
`nixosModules` and guest user configuration with `homeManagerModules`.

For example, load a Home Manager module into dev, together with an inline setting:

```nix
programs.nook.sandboxes.dev.homeManagerModules = [
  ./dev-home.nix
  { programs.git.enable = true; }
];
```

Create `dev-home.nix` next to the file defining that profile:

```nix
{ pkgs, ... }:
{
  home.packages = [ pkgs.neovim ];
  home.sessionVariables.EDITOR = "nvim";
}
```

These settings apply inside dev, including its named and ephemeral instances.
The host's Home Manager settings are not copied into the guest. Nook supplies
the username, home directory, and state version automatically and activates the
modules once per successful generation in each instance's home.
Guest Home Manager modules can also set `dconf.settings`: Nook provides the
native guest dconf service. This configures the guest's database, separately
from the Settings portal used to follow the host's appearance.

Backend and portal environment settings are defaults. Override them in a guest
NixOS module when an application needs a different choice:

```nix
programs.nook.sandboxes.dev.nixosModules = [
  ({ lib, ... }: {
    environment.variables.QT_QPA_PLATFORM = "xcb"; # Requires startX = true.
    environment.sessionVariables.XCURSOR_PATH = lib.mkBefore [ "/path/to/icons" ];
    fonts.fontconfig.enable = false; # Optional; enabled by default.
  })
];
```

Cursor lookup includes the guest's `~/.icons`, `~/.local/share/icons`, and NixOS
profile icon directories. Setting a profile's `environment.XCURSOR_PATH` adds
preferred directories before those fallbacks. Guest Qt and cursor settings also
reach apps launched as transient systemd services.
Cursor search paths expand `$HOME`, `$USER`, and `$XDG_STATE_HOME` (also in
`${…}` form) for that guest. Other variable names remain literal; shell commands
and parameter expressions are not evaluated.
Portal environment flags control application behavior; the profile's `portals`
option controls whether Nook grants portal access.

Nook forwards common desktop identity variables such as
`XDG_CURRENT_DESKTOP` automatically. Extend that allow-list per sandbox when an
application needs another value from the host session:

```nix
programs.nook.sandboxes.dev.hostEnvironment = [ "MY_EDITOR_INTEGRATION" ];
```

The current value is passed only when launching a command and is not stored in
the Nix store. Guest `environment` and NixOS environment settings still define
the final value seen by the application.

## Use

```sh
nk                                    # interactive selector
nk launch dev                         # start or join dev's main instance
nk launch dev --name my-project        # independent persistent instance
nk launch dev --ephemeral              # new instance with a temporary home
nk launch dev -- git --version         # run a command
nk launch dev --join -- git status    # require an already running instance
nk gui zotero                         # launch the configured GUI application
nk list
nk stop dev --name my-project
nk stop dev --all                         # stop every running instance of dev
nk stop --all                             # stop every running sandbox
nk delete dev --name my-project        # confirm permanent removal of saved data
```

If Home Manager refuses to replace an existing file, Nook keeps the sandbox
running and prints a recovery warning before entering it. Inside the sandbox,
run `nook-home-manager switch -b backup` to preserve each conflict with a
`.backup` suffix, then exit, stop the instance, and launch it again.

`launch` also accepts the aliases `l`, `start`, and `s`: `nk s dev` and
`nk start dev` do the same thing as `nk launch dev`. Add `--join` to fail if the
selected instance is stopped. This flag also works with `gui` and `shell`;
it cannot be combined with `--ephemeral`, which always creates a new instance.

In the selector, use arrows or **j/k** to select a row, **Enter** to launch/enter,
**n** to create a named instance from that profile, **e** for an ephemeral
instance, **s** to stop, **d** to delete a stopped named instance after confirmation,
and **q** to quit. Running instances have a green **●** to their left. Named
instances remain listed when stopped. Names accept 1–24 lowercase letters,
digits, or hyphens.

New instances reuse their profile's configuration and permissions, starting
with a clean home. They do not copy projects, credentials, or caches from the
main instance. Main homes persist by default; `persistent = false` makes only
the main home temporary. Named homes always persist. Exiting a shell leaves the
instance running; stopping an ephemeral instance deletes its home.
`nk list` shows its ID so you can rejoin
or stop it with `--instance e-…`. `--name project` is shorthand for
`--instance n-project`. `nk stop PROFILE --all` stops every running instance of
that profile, while `nk stop --all` stops every running instance in the profile
catalog. Both commands skip already stopped instances and report each stop
request; if one request fails, remaining instances are still stopped and the
command exits nonzero.

Guest hostnames default to the template and instance name: `dev-main`,
`dev-project` for `--name project`, or `dev-e-…` for an ephemeral instance.
These are set at launch; instances reuse the same NixOS build. Set
`networking.hostName` in `nixosModules` to use a fixed hostname for a profile.

Homes are stored under `${config.xdg.stateHome}/nook/<profile>/home` for main
instances and `${config.xdg.stateHome}/nook/<profile>/instances/n-<name>/home`
for named ones. Home Manager fixes this path when it evaluates the profile, so
changing the runtime `XDG_STATE_HOME` alone does not move an installed profile's
state. Set a profile's `stateDirectory` to an absolute path to override it.
Relative values remain supported under the home directory. Ephemeral homes live
on the host's `/run` tmpfs. Running `nk-dev` from inside dev's backing home opens
the corresponding guest directory; host symlink aliases work too.
See [the migration guide](docs/migration-xdg-state.md) before changing existing
profiles from their previous `~/.sandbox` paths.
GUI entry points also appear in desktop application search.

Bash, Zsh, and Fish completions are installed with the package and exported by
the Home Manager launchers. They use Clap's CLI definitions for subcommands and
flags, and discover configured profiles and saved/running instances when you
press Tab. Both `nk` and `sandbox` are supported. Completion respects
`NOOK_PROFILES` and an explicit `--profiles` catalog.

To remove a named instance, stop it and run `nk delete PROFILE --name NAME`.
The command shows the directory and asks for confirmation; use `--yes` for
scripts. It removes that instance's saved directory and home with your host
permissions. It refuses running instances, symlink roots, and mounted directories,
and does not follow symlinks inside the home. File permission errors are reported;
deletion is not transactional. Bind-mount sources outside the instance's saved
directory are left alone. Main instances belong to their Nix profile and cannot be
deleted this way; ephemeral homes disappear when stopped.

## Isolation and host access

Instances keep build output, sccache, and rootless Podman storage separate. The
Nix store is shared read-only. Container root maps to a high host UID, never
host root. Writable `bindMounts` deliberately grant shared access to their
sources, including to every named and ephemeral instance of that profile.

Only Wayland is forwarded from the host display. `startX = true` starts a private
[`xwayland-satellite`](https://github.com/Supreeeme/xwayland-satellite) inside
each instance; the host's X11 sockets are not exposed.
GPU and audio access are enabled by default and can be disabled with `gpu` and
`audio`. Other devices are selected with `devices` or `devicePatterns`.
The commented CUDA entries show device forwarding only. CUDA also needs
NVIDIA user-space driver libraries compatible with the host driver, plus the
CUDA libraries required by the application; see
[NVIDIA's container documentation](https://docs.nvidia.com/datacenter/cloud-native/container-toolkit/latest/docker-specialized.html#driver-capabilities).
Nook's `gpu` option currently supplies Mesa rather than the NVIDIA driver
libraries. The CUDA example has not been tested on NVIDIA hardware.

For hardware-accelerated VMs, the dev example explicitly forwards `/dev/kvm`.
It is not forwarded by default. The host must provide KVM and the owner must
have read/write access to that device; Nook does not grant missing host access.
Set `programs.nook.defaults.devices = [ "/dev/kvm" ];` to allow it in every profile.
Use `qemu-system-x86_64 -accel kvm …` inside dev. VM disk images stored in its
home remain private to that instance.

Interactive desktop portals are **off by default**. Enabling `portals` permits
host consent dialogs and access to selected host files through the document
portal. Host appearance settings are inherited separately by default; disable
`inheritAppearance` to turn off that separate connection when `portals` is also
disabled. `nix = true` grants untrusted-client access to the host Nix daemon.
The broker strips trusted-user authority even when the owner is trusted by the
host daemon. Adding `homeManagerModules` also enables that connection for
activation, even with `nix = false`. `podman = true` enables rootless Podman
inside the instance.

Networking uses a separate namespace and pasta. Guest loopback is independent
of host loopback, and automatic port forwarding is disabled in both directions.
Use `networkArgs` to publish explicit ports or `network = false` to disable
network access. See [architecture and configuration](docs/architecture.md) for
the D-Bus policies, storage model, permissions, and troubleshooting commands.

## Security

You are responsible for the code you run and the access you give it. Shared
mounts, devices, the host Nix daemon, and desktop services expose parts of the
host to sandboxed programs. Direct Wayland access includes all globals exposed
by the compositor, potentially including clipboard, screen-capture and desktop
control protocols. Direct audio sockets rely on the host PipeWire/PulseAudio
policy. Configuration and environment values are stored in the world-readable
Nix store and must not contain secrets.

Instances share the host kernel, and Nook does not currently install CPU,
memory, I/O or process ceilings for them. Treat hostile workloads as capable of
kernel attack and host resource exhaustion. Review the configuration before
using it for untrusted code, and keep backups of data you care about.

Nook is experimental. I provide no guarantee of security, isolation, correctness,
or protection against data loss. Use it at your own risk; the license's warranty
and liability terms apply.

## Development

See [development and tests](docs/development.md) for the Rust environment, native
tests, Miri, and NixOS VM integration tests, and
[startup measurements](docs/benchmarks.md) for profiling tools and recorded results.

## AI assistance

Most of Nook's code and documentation was written with AI assistance. It grew
out of a personal NixOS configuration, where development included desktop use,
Rust tests, Miri, and headless VM tests. The [development notes](docs/development.md)
distinguish checks on this extraction from earlier testing. Desktop dialogs,
GPU applications, and hardware still need testing on the system where you use them.

## Credits

Nook started as a [NixPak](https://github.com/nixpak/nixpak) configuration. NixPak
provided the foundation for the original setup and shaped how applications,
mounts, and desktop access were configured. Thanks to its authors and contributors.
