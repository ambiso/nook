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
  };
  removedOverride = evaluate {
    home-manager.users.alice.programs.nook.host.uid = lib.mkForce 1002;
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
  literalCursorPaths = "/literal/$HOMELESS/icons:/literal/$USERNAME/icons:/literal/$XDG_STATE_HOME_EXTRA/icons:/literal/\${HOMELESS}/icons";
  literalCursor = appearanceGuest {
    environment.XCURSOR_PATH = literalCursorPaths;
  };
  disabledAppearance = automatic.home-manager.users.alice.programs.nook.sandboxes.dev._container;
  sessionValueReachesApplications =
    guest: name: value:
    guest.systemd.globalEnvironment.${name} == value
    && lib.elem (builtins.toJSON "${name}=${lib.replaceStrings [ "%" ] [ "%%" ] value}") guest.systemd.user.settings.Manager.DefaultEnvironment;
  cursorPaths = guest: lib.splitString ":" guest.systemd.globalEnvironment.XCURSOR_PATH;
  themeReachesApplications =
    guest: theme:
    guest.environment.variables.QT_QPA_PLATFORMTHEME == theme
    && sessionValueReachesApplications guest "QT_QPA_PLATFORMTHEME" theme;
  service = "sandbox-ualice-dev@";
  guest = automatic.home-manager.users.alice.programs.nook;
in
# This check forces the guest derivation as well as host unit evaluation: an
# accidental dependency on nullable NixOS IDs must fail without booting a VM.
assert lib.any (
  a: !a.assertion && lib.hasInfix "resolved at launch" a.message
) removedOverride.home-manager.users.alice.assertions;
assert automatic.users.users.alice.uid == null;
assert automatic.users.groups.alice.gid == null;
assert guest.guest.uid == 1000 && guest.guest.gid == 100;
assert stalePackageDefault.outPath != staleNook.outPath;
assert themeReachesApplications portalTheme "xdgdesktopportal";
assert themeReachesApplications qtTheme "qt5ct";
assert themeReachesApplications explicitTheme "custom";
assert lib.all (
  name:
  configuredPreferences.environment.variables.${name} == preferenceOverrides.${name}
  && sessionValueReachesApplications configuredPreferences name preferenceOverrides.${name}
  && sessionValueReachesApplications profilePreferences name preferenceOverrides.${name}
) (builtins.attrNames preferenceOverrides);
assert !configuredPreferences.fonts.fontconfig.enable;
assert portalTheme.fonts.fontconfig.enable;
assert !(unsetPreferences.environment.variables ? GDK_DEBUG);
assert !(unsetPreferences.systemd.globalEnvironment ? GDK_DEBUG);
assert
  !lib.any (lib.hasPrefix ''"GDK_DEBUG='') unsetPreferences.systemd.user.settings.Manager.DefaultEnvironment;
assert !(disabledAppearance.systemd.globalEnvironment ? GDK_DEBUG);
assert !(disabledAppearance.systemd.globalEnvironment ? GTK_USE_PORTAL);
assert lib.hasPrefix "/home/alice/.icons:/home/alice/.local/share/icons:"
  portalTheme.systemd.globalEnvironment.XCURSOR_PATH;
assert lib.elem "/run/current-system/sw/share/icons" (cursorPaths portalTheme);
assert lib.elem "/home/alice/.nix-profile/share/icons" (cursorPaths portalTheme);
assert lib.elem "/home/alice/.local/state/nix/profile/share/icons" (cursorPaths portalTheme);
assert lib.elem "/etc/profiles/per-user/alice/share/icons" (cursorPaths portalTheme);
assert lib.hasPrefix "/custom/100%/icons:"
  configuredPreferences.systemd.globalEnvironment.XCURSOR_PATH;
assert lib.elem "/run/current-system/sw/custom-cursors" (cursorPaths configuredPreferences);
assert lib.elem "/home/alice/state/alice/nix/profile/custom-cursors" (
  cursorPaths configuredPreferences
);
assert sessionValueReachesApplications configuredPreferences "XCURSOR_PATH"
  configuredPreferences.systemd.globalEnvironment.XCURSOR_PATH;
assert lib.hasPrefix "/preferred/icons:/more/icons:/home/alice/.icons:"
  profilePreferences.systemd.globalEnvironment.XCURSOR_PATH;
assert sessionValueReachesApplications profilePreferences "XCURSOR_PATH"
  profilePreferences.systemd.globalEnvironment.XCURSOR_PATH;
assert !lib.hasInfix "$" portalTheme.systemd.globalEnvironment.XCURSOR_PATH;
assert lib.hasPrefix "${literalCursorPaths}:" literalCursor.systemd.globalEnvironment.XCURSOR_PATH;
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
assert explicit.systemd.services."sandbox-4242-dev@".wants == [ "systemd-machined.service" ];
assert explicit.systemd.services."sandbox-4242-dev@".unitConfig.X-StopOnRemoval == false;
pkgs.writeText "nook-module-identity" (
  builtins.toJSON {
    guestProfile = guest.sandboxes.dev._profile.outPath;
    unit = automatic.systemd.services.${service}.serviceConfig.ExecStart;
    catalog = automatic.environment.etc."sandbox/profiles/ualice/dev".source.outPath;
  }
)
