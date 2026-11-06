{
  pkgs,
  home-manager,
  enabled ? true,
  output ? "profiles",
}:
let
  home = home-manager.lib.homeManagerConfiguration {
    inherit pkgs;
    modules = [
      (import ../nix/home-manager.nix { inherit home-manager; })
      {
        home.username = "alice";
        home.homeDirectory = "/home/alice";
        home.stateVersion = "26.05";
        programs.nook = {
          enable = enabled;
          hostIntegration.enable = true;
          sandboxes.dev = {
            nix = true;
            gpu = false;
            audio = false;
            inheritAppearance = false;
            portals = false;
            network = false;
            hostSessionBus = false;
          };
        };
      }
    ];
  };
in
assert home.config.home.activation ? installNookHost;
assert pkgs.lib.hasInfix "run /usr/bin/pkexec" home.config.home.activation.installNookHost.data;
assert builtins.elem "linkGeneration" home.config.home.activation.installNookHost.after;
assert builtins.elem "installPackages" home.config.home.activation.installNookHost.after;
assert enabled || pkgs.lib.hasInfix "sandbox-profiles-empty" home.config.home.activation.installNookHost.data;
if output == "activation" then home.activationPackage else home.config.programs.nook.profiles
