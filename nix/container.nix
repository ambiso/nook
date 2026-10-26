{
  pkgs,
  config,
  lib,
  username,
  uid,
  gid,
  home,
  shellPackage,
  appPackage,
  sessionTools,
  activation,
  environment,
  startX,
  podman,
  portals,
  inheritAppearance,
  nixBuildLogs,
  dnsSearch,
  launcher,
  devicePatterns,
  resourceLimits,
  hostTimeZone,
  nixEnabled,
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
      "${appPackage}/share"
      "${pkgs.gsettings-desktop-schemas}/share/gsettings-schemas/${pkgs.gsettings-desktop-schemas.name}"
      "${pkgs.gtk3}/share/gsettings-schemas/${pkgs.gtk3.name}"
      "${pkgs.gtk4}/share/gsettings-schemas/${pkgs.gtk4.name}"
      "/run/current-system/sw/share"
    ];
  };
  sessionVariableNames =
    builtins.attrNames (
      backendDefaults // appearanceDefaults // portalDefaults // requiredEnvironment // environment
    )
    ++ [
      "QT_STYLE_OVERRIDE"
      "XCURSOR_THEME"
      "XCURSOR_SIZE"
    ];
  # Login shells expand these NixOS profile placeholders; systemd manager
  # environments do not. Resolve the known user paths without evaluating shell
  # code, retaining the standard home and profile cursor directories.
  expandVariables =
    variables: value:
    lib.concatMapStrings (
      part:
      if builtins.isList part then
        let
          token = builtins.head part;
        in
        variables.${token} or token
      else
        part
    ) (builtins.split "([$][{][A-Za-z_][A-Za-z0-9_]*[}]|[$][A-Za-z_][A-Za-z0-9_]*)" value);
  # Match complete variable names: replacing "$HOME" as a substring would
  # corrupt unrelated names such as "$HOMELESS". Unknown names stay literal.
  userVariables = {
    "$HOME" = home;
    "\${HOME}" = home;
    "$USER" = username;
    "\${USER}" = username;
  };
  stateHome = expandVariables userVariables (
    config.environment.variables.XDG_STATE_HOME or "${home}/.local/state"
  );
  expandUserPaths = expandVariables (
    userVariables
    // {
      "$XDG_STATE_HOME" = stateHome;
      "\${XDG_STATE_HOME}" = stateHome;
    }
  );
  # NixOS normally adds these while sourcing /etc/profile. Nook commands are
  # transient user services and deliberately do not source shell startup
  # files, so provide the same values to every command. This matters for GUI
  # launchers and scripts just as much as it does for interactive shells.
  standardCommandEnvironment = lib.filterAttrs (_: value: value != null) (
    {
      NIXPKGS_CONFIG = config.environment.variables.NIXPKGS_CONFIG;
      NIX_USER_PROFILE_DIR = "/nix/var/nix/profiles/per-user/${username}";
      NIX_PROFILES = lib.concatStringsSep " " (
        map expandUserPaths (lib.reverseList config.environment.profiles)
      );
    }
    // lib.optionalAttrs nixEnabled {
      NIX_REMOTE = config.environment.variables.NIX_REMOTE;
    }
  );
  cursorSuffixes = config.environment.profileRelativeEnvVars.XCURSOR_PATH or [ ];
  cursorPath = expandUserPaths (
    lib.concatStringsSep ":" (
      lib.optional (config.environment.variables ? XCURSOR_PATH) config.environment.variables.XCURSOR_PATH
      ++ lib.concatMap (
        profile: map (suffix: "${profile}${suffix}") cursorSuffixes
      ) config.environment.profiles
    )
  );
  # Read the final option values for both manager environments. This also
  # respects variables explicitly unset with null in a guest module.
  sessionEnvironment =
    lib.filterAttrs (name: _: lib.elem name sessionVariableNames) config.environment.variables
    // standardCommandEnvironment
    // lib.optionalAttrs (cursorPath != "") { XCURSOR_PATH = cursorPath; }
    // {
      # OpenSSH's FIDO signing path uses askpass as a passive touch/PIN
      # notifier. Its compiled-in libexec path is not part of the Nix package.
      SSH_ASKPASS = "${pkgs.openssh-askpass}/libexec/gtk-ssh-askpass";
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
    appPackage
    pkgs.dbus
    pkgs.systemd
    pkgs.coreutils
    pkgs.nix
    pkgs.adwaita-icon-theme
    pkgs.shared-mime-info
  ];
  environment.variables =
    lib.mapAttrs (_: lib.mkDefault) applicationDefaults
    // requiredEnvironment
    // builtins.removeAttrs environment [ "XCURSOR_PATH" ];
  # Cursor paths are lists in NixOS. Treat a profile's explicit search path as
  # preferred directories, while keeping the usual home/profile fallbacks.
  environment.sessionVariables = lib.optionalAttrs (environment ? XCURSOR_PATH) {
    XCURSOR_PATH = lib.mkBefore (lib.splitString ":" environment.XCURSOR_PATH);
  };
  # Bash sources the generated NixOS environment when started interactively,
  # replacing the wrapper-first PATH and values supplied by Nook's command
  # entrypoint. Restore them after normal shell initialization. This also
  # applies to other shells enabled by guest modules.
  environment.shellInit = lib.mkAfter ''
    case ":$PATH:" in
      *":${sessionTools}/bin:"*) ;;
      *) export PATH=${sessionTools}/bin:${appPackage}/bin:$PATH ;;
    esac
    ${lib.concatStringsSep "\n" (
      lib.mapAttrsToList (
        name: value: "export ${name}=${lib.escapeShellArg (toString value)}"
      ) sessionEnvironment
    )}
  '';
  # Entries run as transient user services without sourcing /etc/profile.
  # Seed the user manager's inherited environment as well as login shells.
  systemd.globalEnvironment = sessionEnvironment;
  # PAM supplies its own XDG defaults when user@ starts. Set the user
  # manager's defaults explicitly as well, so transient services retain the
  # guest's schema/icon paths. Use NixOS's usual JSON quoting for unit values.
  systemd.user.settings.Manager.DefaultEnvironment = lib.mapAttrsToList (
    name: value: builtins.toJSON "${name}=${lib.replaceStrings [ "%" ] [ "%%" ] value}"
  ) sessionEnvironment;
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
    requires = [
      "user@${toString uid}.service"
      "sandbox-procfs.service"
    ];
    after = [
      "user@${toString uid}.service"
      "sandbox-procfs.service"
    ];
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
      TimeoutStartSec = 90;
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
    environment.DBUS_SESSION_BUS_ADDRESS = "unix:path=/run/user/${toString uid}/bus";
    serviceConfig = {
      ExecStart = "${launcher}/bin/nk --portal-bridge unix:path=/run/host/portal-bus";
      Type = "dbus";
      BusName = "org.freedesktop.portal.Desktop";
      Restart = "on-failure";
      RestartSec = 1;
    };
  };
  systemd.user.services.sandbox-appearance = lib.mkIf (inheritAppearance && !portals) {
    description = "Read host appearance settings on the sandbox session bus";
    wantedBy = [ "default.target" ];
    after = [ "dbus.socket" ];
    environment.DBUS_SESSION_BUS_ADDRESS = "unix:path=/run/user/${toString uid}/bus";
    serviceConfig = {
      ExecStart = "${launcher}/bin/nk --appearance-bridge unix:path=/run/host/appearance-bus";
      Type = "dbus";
      BusName = "org.freedesktop.portal.Desktop";
      Restart = "on-failure";
      RestartSec = 1;
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
