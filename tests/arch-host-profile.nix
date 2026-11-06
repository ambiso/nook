{
  pkgs,
  home-manager,
  enabled ? true,
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
          sandboxes = pkgs.lib.optionalAttrs enabled {
            dev = {
              nix = true;
              gpu = false;
              audio = false;
              inheritAppearance = false;
              portals = false;
              network = false;
              hostSessionBus = false;
            };
          };
        };
      }
    ];
  };
in
assert home.config.home.activation ? installNookHost;
home.config.programs.nook.profiles
