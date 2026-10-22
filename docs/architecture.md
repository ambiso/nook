# Architecture and configuration

Nook uses systemd-nspawn to boot a small NixOS system for each instance. The host
system manager owns its lifetime, networking helpers, and D-Bus proxies. The
first terminal is just a client: closing it does not shut down the instance.

## Modules and profiles

Import `nook.nixosModules.default` in a NixOS system. It includes the Home Manager
NixOS module and makes `programs.nook` available in each Home Manager user
configuration. Set `programs.nook.enable = true` and define profiles under
`programs.nook.sandboxes`.

`programs.nook.defaults` is a module applied to every profile. Profiles are typed
submodules, so imports and definitions from several files merge normally. Use
`lib.mkDefault` for defaults that individual profiles should be able to override:

```nix
programs.nook = {
  enable = true;
  defaults = { lib, ... }: {
    shell = lib.mkDefault pkgs.bashInteractive;
    audio = lib.mkDefault false;
  };
  sandboxes.dev = {
    packages = [ pkgs.git pkgs.postgresql ];
    podman = true;
    nix = true;
    hostSessionBus = false;
    homeManagerModules = [ ./development-home.nix ];
    nixosModules = [ ./development-services.nix ];
  };
};
```

The shell is a package with a main executable, such as `pkgs.fish` or
`pkgs.bashInteractive`. It is configured per profile and needs no
`extraSpecialArgs`. Guest Home Manager modules receive the profile's `name`
argument for configuration that needs to identify its template.

`nook.homeManagerModules.default` is also exported for standalone Home Manager
use. It can generate profiles and launchers, but cannot install the privileged
host services by itself. Normal NixOS installations should use the NixOS module,
which installs each owner's profiles by username. Host numeric IDs are resolved
at launch, so NixOS may assign them automatically during activation.

The flake exports a launcher package, app, development shell, and optional
`overlays.default`. An overlay is not required. `programs.nook.package` can
replace the launcher for a user; their host services and guest helpers use that
same package.

## Homes and configuration activation

A profile describes packages, guest modules, and host permissions. Its main,
named, and ephemeral instances reuse that description, each with a separate home:

| Instance | Home on the host | Lifetime |
| --- | --- | --- |
| Main, default | `${config.xdg.stateHome}/nook/<profile>/home` | Survives shutdown |
| Named | `${config.xdg.stateHome}/nook/<profile>/instances/n-<name>/home` | Survives shutdown |
| Ephemeral | Beneath `/run/nook-<owner-key>-<profile>-<instance>/image/root` | Deleted at shutdown |

`persistent = false` makes the main home temporary. Explicitly named homes
always persist. Ephemeral roots and homes use the host's `/run` tmpfs, which can
spill into swap. The guest root is temporary for every instance, including main
and named ones. Persist system-service data deliberately; a database under
`/var/lib` will otherwise disappear at shutdown. User services storing data in
the home naturally follow that home's lifetime.

The default host state root follows Home Manager's `config.xdg.stateHome` at
profile evaluation time (normally `~/.local/state`). Runtime changes to
`XDG_STATE_HOME` do not relocate an already installed profile. Profiles can
override `stateDirectory` with an absolute path; relative overrides remain
home-relative for compatibility.

Home Manager activates after the native guest user manager is ready. A success
marker in `~/.local/state/sandbox/home-generation` caches activation by
generation. Boot still recreates runtime sockets and starts user services.
Fresh homes, changed generations, and failed activations run activation again.
Use systemd services for work needed at every boot. Remove the success marker
before restarting to force reactivation.

Home Manager keeps its normal collision protection. If activation fails, the
sandbox remains running in recovery mode and Nook prints the recovery steps.
Enter it with `nk shell PROFILE`, then run
`nook-home-manager switch -b backup` to preserve conflicts beside their original
paths. Stop and relaunch the instance after activation succeeds.

Rebuilds do not restart running instances (`restartIfChanged = false`). If a
service's launch-time supervisor contract is unchanged, NixOS requests an
`ExecReload`. The narrow `nook-root` helper holds a per-instance reload lock, verifies the
installed and pinned immutable profile identities, runs the guest's new
`switch-to-configuration switch`, activates the new Home Manager generation,
and only then repins the running profile. Joins continue to use that pinned
generation. If either switch fails, the old pin remains active. Bind mounts,
devices, network and proxy setup, host access, persistent-home placement, and
other launch-time settings cannot be changed live; Nook refuses the reload and
logs `restart required`, but does not fail the host configuration switch. The
instance keeps its pinned generation until it is stopped and launched again.
Compatibility compares the actual parsed values,
including ordinary binds, device lines, and device-pattern lines, rather than
hashes. Guest UID/GID, home, and portal app identity also belong to that contract;
entry commands, environment, system, and activation paths may change in place.
Restarting `systemd-machined` or removing a profile from the next configuration
does not stop an existing instance; instances run until explicitly stopped or
the host shuts down.
Profile and environment records use a versioned byte-length framing so tabs and
newlines in values cannot become additional fields. Legacy line records remain
readable for existing generations. The public `nk` CLI contains no supervisor,
reload, or privileged-entry mode. Polkit authorizes only the current `nook-root`
through a root-managed per-owner path under `/etc/sandbox/helpers`; launchers
from the preceding Home Manager generation therefore keep working while a host
switch publishes the new generation. The helper
entry subcommand verifies the caller against the current installed profile.
Its system-service subcommands reject every invocation carrying pkexec
credentials. The unprivileged portal helpers remain in `nk`. The Nix broker
uses Nix's own daemon implementation and both kinds of helper run with the
sandbox owner's credentials.

Shell and command launches translate the host's physical current directory
when it lies within the selected instance's backing home. Symlink aliases work;
paths outside that home fall back to the guest home. GUI entry points start at
home. This translation does not add mounts or expose another instance's files.

## Filesystem and process isolation

Each instance has a delegated cgroup and a disjoint private UID range. Container
root maps to a high host UID, never host root. Native systemd managers own system
and user services inside the guest. Rootless Podman uses subordinate UIDs/GIDs
and native mapping helpers within this private namespace.

Instances share the host kernel. Nspawn supplies namespace, capability and
system-call isolation, but Nook does not turn it into a virtual-machine security
boundary. To support nested application sandboxes, Nook also replaces nspawn's
locked procfs view with a procfs mounted inside the guest namespaces. Nook sets no per-instance
CPU, memory, I/O or process ceilings, so a hostile workload can exhaust host
resources. Apply host systemd limits when those properties are part of the threat
model.

The Nix store is shared read-only. Mutable homes, build output, caches, and
rootless Podman storage are separate. `bindMounts` adds deliberate exceptions:

```nix
programs.nook.sandboxes.dev.bindMounts = [ {
  source = "/home/alice/Exports";
  target = "/home/alice/Exports";
  readOnly = true;
} ];
```

Every instance of that profile receives those same mounts, including ephemeral
ones. A clean home does not make an explicitly shared directory private.
Home mounts do not automatically include host child mounts. Nested targets are
prepared in the enclosing source using the owner's credentials, without
following symlinks or truncating existing files. The supervisor pins source
inodes before mounting and uses owner ID mappings rather than exposing host root.

The launcher forbids broad runtime/device mounts and host X11 sockets. Devices
must be selected with `devices` or top-level character-device patterns such as
`devicePatterns = [ "/dev/hidraw*" ]`. Only nodes accessible to the host owner
are cloned, and the device helper updates them after hotplug or permission
changes. Existing open handles keep the kernel's usual semantics.

## Display, audio, and desktop integration

The selected Wayland socket is forwarded. The guest entry environment declares
`XDG_SESSION_TYPE=wayland`, which newer Electron versions use to select Ozone's
Wayland backend. Host `DISPLAY`, `XAUTHORITY`, and X11
sockets are excluded. `startX = true` starts a private
[`xwayland-satellite`](https://github.com/Supreeeme/xwayland-satellite) for
applications needing X11; guest `:0` refers to that server. Separate network
namespaces also keep host abstract X11 sockets out of reach.
OpenSSH uses a guest-provided askpass helper for FIDO touch and PIN prompts,
rather than its absent compile-time `libexec/ssh-askpass` path.

The direct Wayland connection exposes every global that the host compositor
offers to that connection. This includes ordinary clipboard protocols: a GUI
application in a sandbox can replace the host clipboard and may read clipboard
data when the compositor grants it selection access. On compositors that expose
unrestricted screencopy, output-management, virtual-input or desktop-shell
extensions, the corresponding capture or control operations are available too.
The read-only socket bind does not restrict messages sent over the socket. Do
not treat the host display or clipboard as a secret boundary while a GUI-capable
sandbox is running; strong isolation requires compositor policy or a Wayland
protocol proxy that filters globals.

Start GUI-capable instances from a Wayland session. An instance first started
without a Wayland socket must be stopped and started from that session to gain
one. GPU and audio access default to enabled and can be disabled per profile.
Audio forwards the host PipeWire and PulseAudio sockets directly; the host media
session's policy determines whether that permits microphone capture or other
graph access. GPU forwarding exposes the host kernel driver to sandbox clients.

A `guiEntryPoint` exports a host desktop launcher and its icons. `desktopFile`
selects an upstream desktop ID when it differs from the executable's name;
`desktopNameSuffix` changes its displayed name. Upstream application IDs and
`StartupWMClass` are preserved so the desktop can associate windows with icons.
D-Bus desktop activation is disabled so launches go through Nook. Named instances
do not export additional desktop files and may be grouped together by the shell.

Host appearance settings come through the Settings portal by default, including
live updates. This does not enable file dialogs or screen capture. Set
`inheritAppearance = false` to disable the appearance connection when interactive
portals are also disabled. Custom GTK themes still need their assets installed
inside the guest.

## Portals, D-Bus, and Nix

`portals` defaults to false. When enabled, the host performs consent checks for
file selection and other portal requests. Selected host files are exported
through the document portal under `/run/user/<uid>/doc`, despite the guest's
separate home. Each instance sees its own grants. Main and named instances keep
a stable portal permission identity across restarts.

With `hostSessionBus = false`, applications use the guest's native user bus.
A Rust bridge forwards opted-in portal requests, signals, sessions, and Unix
file descriptors through a filtered host connection. Only the proxy advertises
portal sandbox metadata; real GUI processes keep their upstream desktop identity.
Bindfs presents selected documents with the guest's shifted ownership without
requiring a host-wide FUSE `user_allow_other` setting.

`hostSessionBus` defaults to true: applications then use a filtered host bus
connection directly. Systemd tools and Podman always use the private bus.
`sessionBusPolicies` and `systemBusPolicies` grant additional named access using
`see`, `talk`, or `own`; the system bus is otherwise not forwarded. Required
portal rules are combined with extra application policies.

`nix = true` exposes a broker backed by the host daemon. Adding
`homeManagerModules` also enables it for activation, even with `nix = false`;
the connection remains available to the instance. The broker connects upstream
as the owner but forces every guest connection to Nix's untrusted-client policy.
Consequently, membership in the host daemon's `trusted-users` list does not pass
into the sandbox. This is a brokered host store, not a separate guest daemon.

## Networking and hostnames

Nspawn sets each guest's hostname at launch: `<profile>-main`,
`<profile>-<name>` for named instances, and `<profile>-e-<id>` for ephemeral
instances. This does not evaluate or build a new NixOS system per instance.
Leading and trailing hyphens are trimmed to make valid hostname labels.
The guest leaves `networking.hostName` empty by default so boot preserves this
runtime hostname. To override it for every instance of a profile:

```nix
programs.nook.sandboxes.dev.nixosModules = [ {
  networking.hostName = "workstation";
} ];
```

Pasta provides networking in a separate namespace. Automatic forwarding is
disabled in both directions, so host listeners do not reserve guest loopback
ports and guest listeners are not published on the host. Nook prepends those
safe defaults to `networkArgs`; a later explicit pasta port option replaces
only its corresponding `none` setting. Use `networkArgs` to configure explicit
mappings or `network = false` for an offline instance. Even offline instances
retain a separate namespace for abstract-socket isolation.

`dnsSearch` configures client-side search domains. Forwarding a DNS zone on the
host does not add a guest search domain. Guest-only hostname overrides use normal
NixOS options and refer to each instance's own loopback:

```nix
programs.nook.sandboxes.dev.nixosModules = [ {
  networking.hosts."127.0.0.1" = [ "project.test" ];
} ];
```

Guests use the host timezone through nspawn's automatic setup.

## Inspecting an instance

```sh
nk list
systemctl status sandbox-uambiso-dev@main.service
journalctl -u sandbox-uambiso-dev@main.service
machinectl list
nk launch dev -- systemctl --user status
```

The host unit is `sandbox-<owner-key>-<profile>@<instance>.service`; the registered
machine is `sbx-<uid>-<profile>-<instance>`. Activation output is copied into the
host unit's journal so errors remain available when a failed guest root is
removed.

The separate `nook-root` entry helper accepts only a bounded profile name, resolves it through the
authenticated caller's root-managed profile registry, and then verifies the immutable profile and
caller's UID before entering its namespaces. It does not forward the caller's environment. It runs commands as the guest
owner through that guest's user manager. Permission to join does not authorize
other root helper modes.

## Compatibility with the earlier sandbox launcher

`nk` and `nk-<profile>` are the primary commands. `sandbox` and
`sandbox-<profile>` remain aliases. Existing service names, profile registry,
UID reservations, portal IDs, and home paths retain their `sandbox` names. These
are persistent identities: renaming them casually can lose grants or subordinate
ownership, or start two instances over the same application data.

The former Nix API now lives under `programs.nook`. Shared modules become
`programs.nook.defaults`, and shell selection becomes a typed profile option.
There is no required `pkgs.home-manager` overlay or shell module argument.

For homes left by the older bwrap setup, initial Home Manager activation keeps
conflicting files beside their originals with a `.pre-systemd` suffix. This
migration is enabled only for a home carrying `.hm-generation` without a native
Home Manager GC root; later activations retain normal collision checks. Stop any
legacy launcher using a home before starting its native instance.

## Owner and guest identities

The configured owner is a host username, resolved through the host account
database at launch. Numeric host IDs are not part of the guest derivation.
`programs.nook.guest.uid` and `.gid` default to 1000 and 100; these are the IDs
inside the guest. Owner-mapped mounts translate the host account to those guest
IDs while preserving each instance's separate subordinate range. Host root is
not included in that mapping.

The catalog is `/etc/sandbox/profiles/<owner-key>`. An owner key starts with `u`,
then the username with every byte except lowercase ASCII letters and digits
escaped as `-xx` hexadecimal: `alice` becomes `ualice`, while `a_b` becomes
`ua-5fb`. Escaping hyphens too prevents collisions. No UID is needed to evaluate
these paths or service definitions.

The privileged supervisor records its resolved host UID/GID in root-owned
runtime state. Entry checks the current account against this record and refuses
to join after an account ID change. Existing numeric service templates remain
installed for explicitly numbered accounts during migration. The launcher can
join their pinned profiles, and stopped persistent instances adopt their old
subordinate-range reservation without changing file ownership. Numeric range
records and their lock inodes remain compatible with older launchers for NixOS
rollbacks; a separate root-owned alias records the stable owner identity. Registered
machine names retain their numeric host UID for compatibility.

A bind source may start with `@host-runtime@/`, expanded by the launcher to the
owner's `/run/user/<host-uid>/`. The target remains an explicit guest path. This
allows sharing a particular runtime socket without putting a host UID in Nix.
