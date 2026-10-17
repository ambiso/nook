{ home-manager }:
{
  config,
  lib,
  pkgs,
  ...
}:
let
  outer = config;
  users = lib.filterAttrs (
    _: user: user.programs.nook.enable && user.programs.nook.sandboxes != { }
  ) config.home-manager.users;
  profiles = lib.concatLists (
    lib.mapAttrsToList (
      username: user:
      lib.mapAttrsToList (name: profile: {
        inherit username name profile;
        launcher = user.programs.nook.package;
        uid = config.users.users.${username}.uid;
        gid = config.users.groups.${config.users.users.${username}.group}.gid;
      }) user.programs.nook.sandboxes
    ) users
  );
  unitName = p: "sandbox-${toString p.uid}-${p.name}@";

in
{
  imports = [ home-manager.nixosModules.home-manager ];
  config = {
    home-manager.sharedModules = [
      (import ./home-manager.nix { inherit home-manager; })
      ({ config, ... }: {
        programs.nook.host.uid = lib.mkIf (config.programs.nook.enable) (
          lib.mkDefault outer.users.users.${config.home.username}.uid
        );
        programs.nook.host.gid = lib.mkIf (config.programs.nook.enable) (
          lib.mkDefault outer.users.groups.${outer.users.users.${config.home.username}.group}.gid
        );
      })
    ];
    systemd.services = lib.listToAttrs (
      map (
        p:
        lib.nameValuePair (unitName p) {
          description = "Sandbox ${p.name} (%i), owned by ${p.username}";
          # A rebuild publishes the next boot's definition. Running guests keep
          # their pinned closure until the owner explicitly stops or restarts them.
          restartIfChanged = false;
          requires = [ "systemd-machined.service" ];
          after = [ "systemd-machined.service" ];
          path = [
            pkgs.systemd
            pkgs.coreutils
            pkgs.util-linux
            pkgs.iproute2
            pkgs.passt
            pkgs.xdg-dbus-proxy
            pkgs.bubblewrap
            pkgs.bindfs
            pkgs.nix
          ];
          serviceConfig = {
            Type = "notify";
            NotifyAccess = "main";
            ExecStart = "${p.launcher}/bin/nk --serve ${p.profile._profile} %i";
            RuntimeDirectory = "sbx-${toString p.uid}-${p.name}-%i";
            RuntimeDirectoryMode = "0711";
            StateDirectory = "sandbox/uid-ranges";
            PrivateMounts = true;
            Delegate = true;
            # Leave the delegated root empty so nspawn can enable controllers
            # for its sibling payload cgroup while helpers stay in supervisor.
            DelegateSubgroup = "supervisor";
            KillMode = "mixed";
            TimeoutStartSec = 120;
            TimeoutStopSec = 120;
            Restart = "no";
          };
        }
      ) profiles
    );
    environment.etc = lib.listToAttrs (
      map (
        p: lib.nameValuePair "sandbox/profiles/${toString p.uid}/${p.name}" { source = p.profile._profile; }
      ) profiles
    );
    security.polkit.enable = lib.mkIf (profiles != [ ]) true;
    xdg.portal.enable = lib.mkIf (lib.any (
      p: p.profile.portals || p.profile.inheritAppearance
    ) profiles) true;
    xdg.portal.extraPortals =
      lib.mkIf (lib.any (p: p.profile.portals || p.profile.inheritAppearance) profiles)
        [
          pkgs.xdg-desktop-portal-gtk
        ];
    security.polkit.enablePkexecWrapper = lib.mkIf (profiles != [ ]) true;
    # The Rust helper checks PKEXEC_UID against the immutable, installed
    # profile. Its privileged entry point can only join as that profile's user.
    security.polkit.extraConfig =
      lib.concatMapStrings (username: ''
        polkit.addRule(function(action, subject) {
          if (subject.user === ${builtins.toJSON username}
              && action.id === "org.freedesktop.policykit.exec"
              && action.lookup("program") === "${
                users.${username}.programs.nook.package
              }/bin/.nk-wrapped") {
            return polkit.Result.YES;
          }
        });
      '') (builtins.attrNames users)
      + lib.concatMapStrings (p: ''
        polkit.addRule(function(action, subject) {
          if (subject.user !== ${builtins.toJSON p.username}) return;
          if (action.id === "org.freedesktop.systemd1.manage-units") {
            var unit = action.lookup("unit");
            var verb = action.lookup("verb");
            if (unit && new RegExp(${
              builtins.toJSON ("^" + unitName p + "(main|n-[a-z0-9-]{1,24}|e-[a-f0-9]{16})\\.service$")
            }).test(unit)
                && (verb === "start" || verb === "stop" || verb === "restart")) return polkit.Result.YES;
          }
        });
      '') profiles;
  };
}
