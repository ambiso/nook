{ pkgs, home-manager }:
let
  lib = pkgs.lib;
  evaluateWith =
    packageSet: extra:
    (import "${packageSet.path}/nixos/lib/eval-config.nix" {
      system = packageSet.stdenv.hostPlatform.system;
      modules = [
        (import ../nix/nixos.nix { inherit home-manager; })
        {
          nixpkgs.pkgs = packageSet;
          system.stateVersion = "26.05";
          users.users.alice = {
            isNormalUser = true;
            group = "alice";
          };
          users.groups.alice = { };
          home-manager.useGlobalPkgs = true;
          home-manager.users.alice = {
            home.stateVersion = "26.05";
            programs.nook = {
              enable = true;
              sandboxes.dev = {
                gpu = false;
                audio = false;
                inheritAppearance = false;
              };
            };
          };
        }
        extra
      ];
    }).config;
  evaluate = evaluateWith pkgs;
  automatic = evaluate { };
  explicit = evaluate {
    users.users.alice.uid = 4242;
    users.groups.alice.gid = 4343;
    home-manager.users.alice.programs.nook.sandboxes.dev.nixosModules = [
      { networking.hostName = "workstation"; }
    ];
    home-manager.users.alice.programs.nook.sandboxes.dev.hostEnvironment = [ "CUSTOM_HINT" ];
    home-manager.users.alice.programs.nook.sandboxes.dev.extraSystemCalls = [ "perf_event_open" ];
    home-manager.users.alice.programs.nook.sandboxes.dev.resourceLimits = {
      CORE = 0;
      MEMLOCK = "64M";
    };
  };
  nixDefaults = evaluate {
    home-manager.users.alice.programs.nook.sandboxes.dev.nix = true;
  };
  nixLogsDisabled = evaluate {
    home-manager.users.alice.programs.nook.sandboxes.dev = {
      nix = true;
      nixBuildLogs = false;
    };
  };
  removedOverride = evaluate {
    home-manager.users.alice.programs.nook.host.uid = lib.mkForce 1002;
  };
  reservedStaticEnvironment = evaluate {
    home-manager.users.alice.programs.nook.sandboxes.dev.environment.HOME = "/tmp/escape";
  };
  reservedForwardedEnvironment = evaluate {
    home-manager.users.alice.programs.nook.sandboxes.dev.hostEnvironment = [ "DISPLAY" ];
  };
  staleNook = pkgs.writeShellScriptBin "nk" "exit 99";
  stalePackageSet = pkgs.extend (_: _: { nook = staleNook; });
  stalePackageDefault =
    (evaluateWith stalePackageSet { }).home-manager.users.alice.programs.nook.package;
  appearanceGuest =
    extra:
    (evaluate {
      home-manager.users.alice.programs.nook.sandboxes.dev = {
        inheritAppearance = lib.mkForce true;
      }
      // extra;
    }).home-manager.users.alice.programs.nook.sandboxes.dev._container;
  portalTheme = appearanceGuest { };
  qtTheme = appearanceGuest {
    nixosModules = [
      {
        qt.enable = true;
        qt.platformTheme = "qt5ct";
      }
    ];
  };
  explicitTheme = appearanceGuest {
    environment.QT_QPA_PLATFORMTHEME = "custom";
  };
  preferenceOverrides = {
    MOZ_ENABLE_WAYLAND = "0";
    QT_QPA_PLATFORM = "xcb";
    NIXOS_OZONE_WL = "0";
    XDG_SESSION_TYPE = "x11";
    GDK_DEBUG = "fatal-warnings";
    GTK_USE_PORTAL = "0";
    GIO_USE_PORTALS = "0";
    NOTIFY_FORCE_PORTAL = "0";
    NIXOS_XDG_OPEN_USE_PORTAL = "0";
    QT_STYLE_OVERRIDE = "breeze";
    XCURSOR_THEME = "breeze_cursors";
    XCURSOR_SIZE = "32";
  };
  configuredPreferences = appearanceGuest {
    portals = true;
    nixosModules = [
      {
        environment.variables = preferenceOverrides // {
          XCURSOR_SIZE = 32;
          XDG_STATE_HOME = "$HOME/state/$USER";
        };
        environment.sessionVariables.XCURSOR_PATH = lib.mkBefore [ "/custom/100%/icons" ];
        environment.profileRelativeSessionVariables.XCURSOR_PATH = [ "/custom-cursors" ];
        fonts.fontconfig.enable = false;
      }
    ];
  };
  profilePreferences = appearanceGuest {
    portals = true;
    environment = preferenceOverrides // {
      XCURSOR_PATH = "/preferred/icons:/more/icons";
    };
  };
  unsetPreferences = appearanceGuest {
    nixosModules = [
      { environment.variables.GDK_DEBUG = null; }
    ];
  };
  disabledAppearance = automatic.home-manager.users.alice.programs.nook.sandboxes.dev._container;
  themeReachesApplications = guest: theme: guest.environment.variables.QT_QPA_PLATFORMTHEME == theme;
  service = "sandbox-ualice-dev@";
  guest = automatic.home-manager.users.alice.programs.nook;
in
# This check forces the guest derivation as well as host unit evaluation: an
# accidental dependency on nullable NixOS IDs must fail without booting a VM.
assert lib.any (
  a: !a.assertion && lib.hasInfix "resolved at launch" a.message
) removedOverride.home-manager.users.alice.assertions;
assert lib.any (
  a: !a.assertion && lib.hasInfix "cannot set Nook runtime variables: HOME" a.message
) reservedStaticEnvironment.home-manager.users.alice.assertions;
assert lib.any (
  a: !a.assertion && lib.hasInfix "cannot forward Nook runtime variables: DISPLAY" a.message
) reservedForwardedEnvironment.home-manager.users.alice.assertions;
assert automatic.users.users.alice.uid == null;
assert automatic.users.groups.alice.gid == null;
assert guest.guest.uid == 1000 && guest.guest.gid == 100;
assert lib.elem "XDG_CURRENT_DESKTOP" guest.sandboxes.dev.hostEnvironment;
assert lib.elem "TERM" guest.sandboxes.dev.hostEnvironment;
assert lib.elem "COLORTERM" guest.sandboxes.dev.hostEnvironment;
assert !(lib.elem "NIX_PATH" guest.sandboxes.dev.hostEnvironment);
assert lib.elem "NIX_PATH"
  nixDefaults.home-manager.users.alice.programs.nook.sandboxes.dev.hostEnvironment;
assert nixDefaults.home-manager.users.alice.programs.nook.sandboxes.dev.nixBuildLogs;
assert !nixLogsDisabled.home-manager.users.alice.programs.nook.sandboxes.dev.nixBuildLogs;
assert lib.elem "CUSTOM_HINT"
  explicit.home-manager.users.alice.programs.nook.sandboxes.dev.hostEnvironment;
assert
  explicit.home-manager.users.alice.programs.nook.sandboxes.dev.extraSystemCalls == [
    "perf_event_open"
  ];
assert stalePackageDefault.outPath != staleNook.outPath;
assert themeReachesApplications portalTheme "xdgdesktopportal";
assert themeReachesApplications qtTheme "qt5ct";
assert themeReachesApplications explicitTheme "custom";
assert lib.all (
  name:
  configuredPreferences.environment.variables.${name} == preferenceOverrides.${name}
  && profilePreferences.environment.variables.${name} == preferenceOverrides.${name}
) (builtins.attrNames preferenceOverrides);
assert !configuredPreferences.fonts.fontconfig.enable;
assert portalTheme.fonts.fontconfig.enable;
assert lib.elem pkgs.adwaita-fonts portalTheme.fonts.packages;
assert !(unsetPreferences.environment.variables ? GDK_DEBUG);
assert !(disabledAppearance.environment.variables ? GDK_DEBUG);
assert !(disabledAppearance.environment.variables ? GTK_USE_PORTAL);
assert profilePreferences.environment.variables.XCURSOR_PATH == "/preferred/icons:/more/icons";
assert
  configuredPreferences.environment.etc
    ? "systemd/user-environment-generators/20-nook-session-environment";
assert guest.sandboxes.dev._container.networking.hostName == "";
assert !(guest.sandboxes.dev._container.environment.etc ? hostname);
assert
  explicit.home-manager.users.alice.programs.nook.sandboxes.dev._container.networking.hostName
  == "workstation";
assert guest.sandboxes.dev._container.programs.dconf.enable;
assert lib.elem pkgs.dconf guest.sandboxes.dev._container.services.dbus.packages;
assert lib.elem pkgs.dconf guest.sandboxes.dev._container.systemd.packages;
assert automatic.systemd.services.${service}.serviceConfig.RuntimeDirectory == "nook-ualice-dev-%i";
assert
  automatic.environment.etc."sandbox/helpers/ualice".source
  == "${guest.package}/bin/.nook-root-wrapped";
assert lib.hasInfix "/etc/sandbox/helpers/ualice" automatic.security.polkit.extraConfig;
assert explicit.systemd.services ? ${service};
assert !(automatic.systemd.services ? "sandbox-4242-dev@");
assert explicit.systemd.services."sandbox-4242-dev@".restartIfChanged == false;
assert explicit.systemd.services."sandbox-4242-dev@".requires == [ ];
assert explicit.systemd.services."sandbox-4242-dev@".wants == [ "systemd-machined.socket" ];
assert explicit.systemd.services."sandbox-4242-dev@".after == [ "systemd-machined.socket" ];
assert explicit.systemd.services."sandbox-4242-dev@".unitConfig.X-StopOnRemoval == false;
assert explicit.systemd.services.${service}.serviceConfig.LimitCORE == 0;
assert explicit.systemd.services.${service}.serviceConfig.LimitMEMLOCK == "64M";
pkgs.writeText "nook-module-identity" (
  builtins.toJSON {
    guestProfile = guest.sandboxes.dev._profile.outPath;
    unit = automatic.systemd.services.${service}.serviceConfig.ExecStart;
    catalog = automatic.environment.etc."sandbox/profiles/ualice/dev".source.outPath;
  }
)
