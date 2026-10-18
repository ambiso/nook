{ pkgs, home-manager }:
let
  lib = pkgs.lib;
  evaluate =
    extra:
    (import "${pkgs.path}/nixos/lib/eval-config.nix" {
      system = pkgs.stdenv.hostPlatform.system;
      modules = [
        (import ../nix/nixos.nix { inherit home-manager; })
        {
          nixpkgs.pkgs = pkgs;
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
assert guest.sandboxes.dev._container.networking.hostName == "";
assert !(guest.sandboxes.dev._container.environment.etc ? hostname);
assert
  explicit.home-manager.users.alice.programs.nook.sandboxes.dev._container.networking.hostName
  == "workstation";
assert guest.sandboxes.dev._container.programs.dconf.enable;
assert lib.elem pkgs.dconf guest.sandboxes.dev._container.services.dbus.packages;
assert lib.elem pkgs.dconf guest.sandboxes.dev._container.systemd.packages;
assert automatic.systemd.services.${service}.serviceConfig.RuntimeDirectory == "nook-ualice-dev-%i";
assert explicit.systemd.services ? ${service};
assert !(automatic.systemd.services ? "sandbox-4242-dev@");
assert explicit.systemd.services."sandbox-4242-dev@".restartIfChanged == false;
pkgs.writeText "nook-module-identity" (
  builtins.toJSON {
    guestProfile = guest.sandboxes.dev._profile.outPath;
    unit = automatic.systemd.services.${service}.serviceConfig.ExecStart;
    catalog = automatic.environment.etc."sandbox/profiles/ualice/dev".source.outPath;
  }
)
