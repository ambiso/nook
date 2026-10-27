{ home-manager }:
{
  config,
  lib,
  pkgs,
  ...
}:
let
  ownerKey = import ./owner-key.nix { inherit lib; };
  users = lib.filterAttrs (
    _: user: user.programs.nook.enable && user.programs.nook.sandboxes != { }
  ) config.home-manager.users;
  profiles = lib.concatLists (
    lib.mapAttrsToList (
      username: user:
      lib.mapAttrsToList (name: profile: {
        inherit username name profile;
        launcher = user.programs.nook.package;
        owner = ownerKey username;
        # Keep old numeric templates installed during the identity transition.
        legacyUid = config.users.users.${username}.uid;
      }) user.programs.nook.sandboxes
    ) users
  );
  xdg-dbus-proxy = import ./xdg-dbus-proxy.nix { inherit pkgs; };
  unitName = p: "sandbox-${p.owner}-${p.name}@";

in
{
  imports = [ home-manager.nixosModules.home-manager ];
  config = {
    home-manager.sharedModules = [
      (import ./home-manager.nix { inherit home-manager; })
    ];
    systemd.services = lib.listToAttrs (
      lib.concatMap (
        p:
        let
          service = {
            description = "Sandbox ${p.name} (%i), owned by ${p.username}";
            # A rebuild publishes the next boot's definition. Compatible running
            # guests switch in place; incompatible guests must stop and restart.
            restartIfChanged = false;
            # A host switch asks a compatible running guest to switch in place.
            # Any launch-time supervisor change is refused by nook-root reload and
            # still requires an explicit stop and launch.
            reloadIfChanged = true;
            # Let systemd activate machined through its socket. Starting the
            # service directly makes it own the socket itself, after which a
            # configuration switch cannot activate systemd-machined.socket.
            # Wants also avoids propagating daemon restarts into sandboxes.
            wants = [ "systemd-machined.socket" ];
            after = [ "systemd-machined.socket" ];
            # Removing a profile publishes no future launcher, but it must not
            # terminate an instance that the user has not explicitly stopped.
            unitConfig.X-StopOnRemoval = false;
            path = [
              pkgs.systemd
              pkgs.coreutils
              pkgs.util-linux
              pkgs.iproute2
              pkgs.passt
              xdg-dbus-proxy
              pkgs.bubblewrap
              pkgs.bindfs
              pkgs.nix
            ];
            serviceConfig = {
              Type = "notify";
              NotifyAccess = "main";
              ExecStart = "${p.launcher}/bin/nook-root serve ${p.profile._profile} %i";
              ExecReload = "${p.launcher}/bin/nook-root reload ${p.profile._profile} %i";
              RuntimeDirectory = "nook-${p.owner}-${p.name}-%i";
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
            }
            // lib.mapAttrs' (limit: value: lib.nameValuePair "Limit${limit}" value) (
              lib.filterAttrs (_: value: value != null) p.profile.resourceLimits
            );
          };
        in
        [ (lib.nameValuePair (unitName p) service) ]
        ++ lib.optional (p.legacyUid != null) (
          lib.nameValuePair "sandbox-${toString p.legacyUid}-${p.name}@" (
            service
            // {
              environment.NOOK_LEGACY_RUNTIME = "1";
              serviceConfig = service.serviceConfig // {
                RuntimeDirectory = "sbx-${toString p.legacyUid}-${p.name}-%i";
              };
            }
          )
        )
      ) profiles
    );
    environment.etc = lib.listToAttrs (
      map (
        p: lib.nameValuePair "sandbox/profiles/${p.owner}/${p.name}" { source = p.profile._profile; }
      ) profiles
      ++ lib.mapAttrsToList (
        username: user:
        lib.nameValuePair "sandbox/helpers/${ownerKey username}" {
          source = "${user.programs.nook.package}/bin/.nook-root-wrapped";
        }
      ) users
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
    # Only the narrow privileged helper is authorized. It checks PKEXEC_UID
    # against the immutable installed profile and exposes only narrow entry and
    # stopped-instance deletion operations.
    security.polkit.extraConfig =
      lib.concatMapStrings (username: ''
        polkit.addRule(function(action, subject) {
          if (subject.user === ${builtins.toJSON username}
              && action.id === "org.freedesktop.policykit.exec"
              && (action.lookup("program") === "/etc/sandbox/helpers/${ownerKey username}"
                  || action.lookup("program") === "${
                    users.${username}.programs.nook.package
                  }/bin/.nook-root-wrapped")) {
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
              builtins.toJSON (
                "^sandbox-("
                + p.owner
                + lib.optionalString (p.legacyUid != null) ("|" + toString p.legacyUid)
                + ")-${p.name}@(main|n-[a-z0-9-]{1,24}|e-[a-f0-9]{16})\\.service$"
              )
            }).test(unit)
                && (verb === "start" || verb === "stop" || verb === "restart")) return polkit.Result.YES;
          }
        });
      '') profiles;
  };
}
