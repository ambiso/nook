{
  pkgs,
  config,
  lib,
  username,
  uid,
  gid,
  home,
  shellPackage,
  packages,
  sessionTools,
  homeManagerServiceName,
  activation,
  environment,
  startX,
  podman,
  portals,
  inheritAppearance,
  nixBuildLogs,
  nixEnabled,
  dnsSearch,
  launcher,
  devicePatterns,
  resourceLimits,
  hostTimeZone,
  ...
}:
let
  backendDefaults = {
    MOZ_ENABLE_WAYLAND = "1";
    QT_QPA_PLATFORM = "wayland";
    # Electron 39+ removed ELECTRON_OZONE_PLATFORM_HINT and chooses its
    # default Ozone backend from the session type instead.
    XDG_SESSION_TYPE = "wayland";
    NIXOS_OZONE_WL = "1";
  };
  appearanceDefaults = {
    GDK_DEBUG = "portals";
    QT_QPA_PLATFORMTHEME = "xdgdesktopportal";
  };
  portalDefaults = {
    GTK_USE_PORTAL = "1";
    GIO_USE_PORTALS = "1";
    NOTIFY_FORCE_PORTAL = "1";
    NIXOS_XDG_OPEN_USE_PORTAL = "1";
  };
  applicationDefaults =
    backendDefaults
    // lib.optionalAttrs (portals || inheritAppearance) appearanceDefaults
    // lib.optionalAttrs portals portalDefaults;
  requiredEnvironment = {
    WAYLAND_DISPLAY = "wayland-0";
    XDG_DATA_DIRS = lib.concatStringsSep ":" [
      "${pkgs.gsettings-desktop-schemas}/share/gsettings-schemas/${pkgs.gsettings-desktop-schemas.name}"
      "${pkgs.gtk3}/share/gsettings-schemas/${pkgs.gtk3.name}"
      "${pkgs.gtk4}/share/gsettings-schemas/${pkgs.gtk4.name}"
      "/run/current-system/sw/share"
    ];
  };
  # Without a brokered host daemon, the container default (NIX_REMOTE=daemon
  # from NixOS container-config) points at a socket that does not exist. Use
  # Nix's dummy store instead: pure evaluation and Home Manager's activation
  # sanity check (`nix-build --expr '{}'`, `nix-env -q`) succeed without
  # writing anything, while anything that needs a store fails plainly rather
  # than filling the guest runtime tmpfs with unusable store paths.
  storeEnvironment = lib.optionalAttrs (!nixEnabled) {
    NIX_REMOTE = lib.mkOverride 60 "dummy://";
  };
in
{
  boot.isContainer = true;
  boot.isNspawnContainer = true;
  system.stateVersion = "26.05";
  # Leave the static hostname unset so nspawn's template/instance hostname
  # survives boot. Named instances share this system closure; their names are
  # only known at launch. A guest module can still set a static hostname.
  networking.hostName = lib.mkDefault "";
  time.timeZone = lib.mkIf (hostTimeZone != null) (lib.mkDefault hostTimeZone);
  networking.useDHCP = false;
  networking.firewall.enable = false;
  networking.nameservers = [ "169.254.1.1" ];
  networking.search = dnsSearch;
  # Pasta configures the separate network namespace before boot. NixOS's
  # network-local-commands service requires CAP_NET_ADMIN, which the guest
  # does not have over that namespace, so write its static DNS settings here.
  networking.resolvconf.enable = false;
  environment.etc."resolv.conf".text =
    lib.optionalString (
      config.networking.search != [ ]
    ) "search ${lib.concatStringsSep " " config.networking.search}\n"
    + lib.concatMapStrings (server: "nameserver ${server}\n") config.networking.nameservers;
  services.resolved.enable = false;
  services.dbus.enable = true;
  # Home Manager's dconf.settings activation writes through the native user
  # bus. Installing dconf in the app profile alone does not register its D-Bus
  # activation file or systemd user service with the guest system.
  programs.dconf.enable = lib.mkDefault true;
  # This root (and its logs) is discarded at shutdown. There is no historical
  # log set to rotate or journal catalog to rebuild for `journalctl -x`.
  services.logrotate.enable = lib.mkDefault false;
  systemd.services.systemd-journal-catalog-update.enable = lib.mkDefault false;
  # Successful unit status lines cross nspawn's console into the host journal.
  # Keep failures visible without copying every boot progress message there.
  systemd.settings.Manager = {
    ShowStatus = lib.mkDefault "error";
  }
  // lib.mapAttrs' (
    limit: value: lib.nameValuePair "DefaultLimit${limit}" (lib.mkDefault value)
  ) resourceLimits;
  # OpenSSH validates included configuration ownership. Copy its system files
  # into the guest instead of including nobody-owned paths from the public,
  # untranslated store mount.
  programs.ssh.systemd-ssh-proxy.enable = false;
  programs.ssh.extraConfig = "Include /etc/ssh/systemd-ssh-proxy.conf";
  environment.etc."ssh/ssh_config".mode = "0644";
  environment.etc."ssh/systemd-ssh-proxy.conf" = {
    source = "${config.systemd.package}/lib/systemd/ssh_config.d/20-systemd-ssh-proxy.conf";
    mode = "0644";
  };

  fonts.fontconfig.enable = lib.mkDefault true;
  # Prefer Japanese glyph variants for the CJK fallback rather than relying
  # on the order of faces in Noto's multilingual collection.
  fonts.fontconfig.defaultFonts = {
    sansSerif = lib.mkDefault [
      "DejaVu Sans"
      "Noto Sans CJK JP"
    ];
    serif = lib.mkDefault [
      "DejaVu Serif"
      "Noto Serif CJK JP"
    ];
    monospace = lib.mkDefault [
      "DejaVu Sans Mono"
      "Noto Sans Mono CJK JP"
    ];
  };
  fonts.packages = with pkgs; [
    # GNOME's Settings portal advertises Adwaita Sans and Adwaita Mono as
    # interface fonts. Keep those names resolvable when appearance settings
    # are inherited from a GNOME host.
    adwaita-fonts
    dejavu_fonts
    liberation_ttf
    gyre-fonts
    source-sans
    source-code-pro
    noto-fonts-color-emoji
    noto-fonts-cjk-sans
    noto-fonts-cjk-serif
  ];
  security.sudo.enable = false;
  users.mutableUsers = false;
  users.allowNoPasswordLogin = true;
  users.groups.users.gid = lib.mkForce gid;
  users.users.${username} = {
    isNormalUser = true;
    inherit uid;
    group = "users";
    home = home;
    shell = shellPackage;
    # The profile's application packages and, through the folded Home
    # Manager integration, the user's home.packages become the per-user
    # profile at /etc/profiles/per-user/<username>, which NixOS includes in
    # PATH and XDG_DATA_DIRS. When Home Manager is enabled it appends its
    # own user environment to this same option.
    packages = packages;
    ignoreShellProgramCheck = true;
    linger = true;
    subUidRanges = [
      {
        startUid = 100000;
        count = 65536;
      }
    ];
    subGidRanges = [
      {
        startGid = 100000;
        count = 65536;
      }
    ];
  };
  environment.systemPackages = [
    pkgs.dbus
    pkgs.coreutils
    pkgs.systemd
    pkgs.nix
    pkgs.adwaita-icon-theme
    pkgs.shared-mime-info
    # Session infrastructure: bash and grep are the base every guest script
    # assumes, xdg-open routes into the portal bridges, ps/pgrep and gio
    # (via glib) round out the desktop session. Anything else belongs in a
    # profile's `packages`.
    pkgs.bashInteractive
    pkgs.gnugrep
    pkgs.procps
    pkgs.xdg-utils
    pkgs.glib
  ];
  environment.variables =
    lib.mapAttrs (_: lib.mkDefault) applicationDefaults
    // requiredEnvironment
    // storeEnvironment
    # The user's session shell, exported like NixOS exports other evaluated
    # environment values; the entry boundary no longer needs to re-set it.
    // {
      SHELL = lib.getExe shellPackage;
    }
    # The profile shorthand is an explicit override of ordinary NixOS
    # defaults. A guest module can still replace it deliberately with mkForce.
    // lib.mapAttrs (_: lib.mkOverride 90) environment;
  # The per-user profile in environment.profiles expands $HOME and $USER at
  # source time, so /etc/set-environment must not conflict with a guest
  # module's plain definition of the same variable: defaults live in
  # extraInit (runs last, conditional), not in the export block.
  environment.extraInit = lib.mkAfter ''
    export XDG_STATE_HOME="''${XDG_STATE_HOME:-$HOME/.local/state}"
    unset DBUS_SYSTEM_BUS_ADDRESS
    # The guest user's session bus and the session variables declared through
    # the folded Home Manager configuration belong to that user only: other
    # accounts, notably root, must not default to a bus the user controls or
    # inherit its PATH (which may start with user-writable directories).
    if [ "$(${pkgs.coreutils}/bin/id -u)" = ${toString uid} ]; then
      export DBUS_SESSION_BUS_ADDRESS="''${DBUS_SESSION_BUS_ADDRESS:-unix:path=/run/user/${toString uid}/bus}"
      ${lib.optionalString (homeManagerServiceName != "") ". /etc/profiles/per-user/${username}/etc/profile.d/hm-session-vars.sh"}
    fi
  '';
  # Application metadata must resolve for every app in the guest: gio
  # launches the desktop entries the per-user profile exposes, and GTK/GIO
  # resolve icons against the same data dirs. NixOS's xdg.menus and xdg.icons
  # modules link these by default; pin them so a guest module disabling those
  # cannot break `nk` GUI launches.
  environment.pathsToLink = [
    "/share/applications"
    "/share/pixmaps"
    "/share/icons"
  ];
  # Shell startup may rebuild PATH from the configured profiles. Keep Nook's
  # wired session tools ahead of them; command boundaries sourced
  # /etc/set-environment already.
  environment.shellInit = lib.mkAfter ''
    case ":$PATH:" in
      *":${sessionTools}/bin:"*) ;;
      *) export PATH=${sessionTools}/bin:/run/wrappers/bin:$PATH ;;
    esac
  '';
  # Seed the user manager before it starts units. The generator sources the
  # same evaluated /etc/set-environment that Nook command boundaries source,
  # so a compatible live reload does not leave new commands with an older
  # manager environment.
  environment.etc."systemd/user-environment-generators/20-nook-session-environment" = {
    source = pkgs.writeShellScript "nook-session-environment-${username}" ''
      # Generators run without session identity; seed the runtime identity
      # that the evaluated profile-relative exports below expand. Start from
      # a clean environment so manager or caller state cannot become ambient
      # authority inside guest units.
      exec ${pkgs.coreutils}/bin/env -i \
        HOME=${lib.escapeShellArg home} \
        USER=${lib.escapeShellArg username} \
        LOGNAME=${lib.escapeShellArg username} \
        XDG_RUNTIME_DIR=/run/user/${toString uid} \
        ${pkgs.bash}/bin/bash -c ${
          lib.escapeShellArg ''
            . /etc/set-environment
            exec ${pkgs.python3}/bin/python3 - <<'PYEOF'
import os
import re
import shlex

ignored = {"OLDPWD", "PWD", "SHLVL", "_"}
for variable, value in sorted(os.environ.items()):
    if variable in ignored or variable.startswith("__"):
        continue
    if re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", variable):
        print(f"{variable}={shlex.quote(value)}")
PYEOF
            ''
        }
    '';
    mode = "0755";
  };
  virtualisation.podman.enable = podman;
  nix.settings.experimental-features = [
    "nix-command"
    "flakes"
  ];
  nix.settings.sandbox = false;
  nix.channel.enable = false;
  systemd.services.nix-daemon.enable = false;
  systemd.sockets.nix-daemon.enable = false;
  systemd.services.sandbox-procfs = {
    description = "Prepare procfs for nested application sandboxes";
    wantedBy = [ "multi-user.target" ];
    before = [ "user@${toString uid}.service" ];
    after = [ "local-fs.target" ];
    # nspawn's masked proc submounts become locked in a child user namespace.
    # Linux then refuses nested proc mounts as "too revealing", breaking both
    # rootless containers and Steam's bubblewrap-based FHS runtime.
    # Mount a fresh procfs inside the guest's own user/PID namespaces, before
    # any user workload starts. Host procfs and its permissions are unaffected.
    serviceConfig = {
      Type = "oneshot";
      RemainAfterExit = true;
      ExecStart = "${pkgs.util-linux}/bin/mount -t proc proc /proc";
    };
  };
  systemd.services.sandbox-ready = {
    description = "Activate the sandbox home after its user manager starts";
    wantedBy = [ "multi-user.target" ];
    requires =
      [
        "user@${toString uid}.service"
        "sandbox-procfs.service"
      ]
      ++ lib.optional (homeManagerServiceName != "") homeManagerServiceName;
    after =
      [
        "user@${toString uid}.service"
        "sandbox-procfs.service"
      ]
      ++ lib.optional (homeManagerServiceName != "") homeManagerServiceName;
    environment = {
      HOME = home;
      USER = username;
      XDG_RUNTIME_DIR = "/run/user/${toString uid}";
      DBUS_SESSION_BUS_ADDRESS = "unix:path=/run/user/${toString uid}/bus";
    };
    path = [
      pkgs.coreutils
      pkgs.nix
      pkgs.systemd
    ];
    serviceConfig = {
      Type = "oneshot";
      RemainAfterExit = true;
      User = username;
      ExecStart = activation;
      # Home Manager activation (which this service waits on) may run its own
      # five-minute budget; never outlast it from the readiness side.
      TimeoutStartSec = 310;
      # The guest root/journal disappears on failure. Forward activation
      # errors through nspawn's console into the host unit's journal as well.
      StandardOutput = "journal+console";
      StandardError = "journal+console";
    };
  };
  # logind mounts /run/user/<uid> during boot. Keep host sockets outside that
  # mount and create links after the native user runtime directory is ready.
  systemd.user.tmpfiles.rules = [
    "L+ %t/wayland-0 - - - - /run/host/wayland"
    "L+ %t/pipewire-0 - - - - /run/host/pipewire"
    "d %t/pulse 0700 - - -"
    "L+ %t/pulse/native - - - - /run/host/pulse"
  ]
  ++ lib.optional portals "L+ %t/doc - - - - /run/host/doc";
  # Nix's read-only local store still expects its standard state-directory
  # skeleton to exist. Keep these empty directories in the ephemeral guest
  # root; only the host database and build logs are mounted read-only.
  systemd.tmpfiles.rules = lib.optionals nixBuildLogs [
    "d /nix/var/nix/profiles 0755 root root -"
    "d /nix/var/nix/profiles/per-user 0755 root root -"
    "d /nix/var/nix/temproots 0755 root root -"
    "d /nix/var/nix/gcroots 0755 root root -"
    "d /nix/var/nix/gcroots/per-user 0755 root root -"
  ];
  systemd.services.sandbox-device-links = lib.mkIf (devicePatterns != [ ]) {
    description = "Publish explicitly selected hotplug character devices";
    wantedBy = [ "multi-user.target" ];
    before = [ "sandbox-ready.service" ];
    path = [ pkgs.coreutils ];
    script = ''
      while true; do
        for source in /run/host/device-patterns/*/*; do
          [ -c "$source" ] || continue
          target="/dev/$(basename "$source")"
          if [ -L "$target" ]; then
            [ "$(readlink "$target")" = "$source" ] || continue
          elif [ -e "$target" ]; then
            continue
          fi
          ln -sfnT -- "$source" "$target"
        done
        sleep 1
      done
    '';
  };
  systemd.user.services.sandbox-portals = lib.mkIf portals {
    description = "Host desktop portals on the sandbox session bus";
    wantedBy = [ "default.target" ];
    after = [ "dbus.socket" ];
    environment = {
      DBUS_SESSION_BUS_ADDRESS = "unix:path=/run/user/${toString uid}/bus";
      RUST_BACKTRACE = "1";
    };
    serviceConfig = {
      ExecStart = "${launcher}/bin/nk --portal-bridge unix:path=/run/host/portal-bus";
      Type = "dbus";
      BusName = "org.freedesktop.portal.Desktop";
      Restart = "on-failure";
      RestartSec = 1;
      # The guest journal is discarded with the container. Copy bridge errors
      # to nspawn's console so the host sandbox unit retains the diagnosis.
      StandardError = "journal+console";
    };
  };
  systemd.user.services.sandbox-appearance = lib.mkIf (inheritAppearance && !portals) {
    description = "Read host appearance settings on the sandbox session bus";
    wantedBy = [ "default.target" ];
    after = [ "dbus.socket" ];
    environment = {
      DBUS_SESSION_BUS_ADDRESS = "unix:path=/run/user/${toString uid}/bus";
      RUST_BACKTRACE = "1";
    };
    serviceConfig = {
      ExecStart = "${launcher}/bin/nk --appearance-bridge unix:path=/run/host/appearance-bus";
      Type = "dbus";
      BusName = "org.freedesktop.portal.Desktop";
      Restart = "on-failure";
      RestartSec = 1;
      StandardError = "journal+console";
    };
  };
  systemd.user.services.sandbox-xwayland = lib.mkIf startX {
    description = "Private Xwayland for sandbox applications";
    wantedBy = [ "default.target" ];
    after = [ "systemd-tmpfiles-setup.service" ];
    unitConfig.ConditionPathIsSocket = "%t/wayland-0";
    environment.WAYLAND_DISPLAY = "wayland-0";
    serviceConfig = {
      ExecStart = "${pkgs.xwayland-satellite}/bin/xwayland-satellite :0";
      # Match the upstream unit's readiness contract. A cached home can be
      # ready quickly enough to launch X11 clients before Xwayland has bound
      # its display unless the user manager waits for this notification.
      Type = "notify";
      NotifyAccess = "all";
      Restart = "on-failure";
      RestartSec = 2;
    };
  };
}
