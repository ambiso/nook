{ pkgs, home-manager }:
pkgs.testers.runNixOSTest {
  name = "nook-interactive-pty";
  nodes.machine = {
    imports = [ (import ../nix/nixos.nix { inherit home-manager; }) ];
    nixpkgs.pkgs = pkgs.lib.mkForce pkgs;
    system.stateVersion = "26.05";
    users.users.alice = {
      isNormalUser = true;
      uid = 1002;
      linger = true;
    };
    home-manager.useGlobalPkgs = true;
    home-manager.useUserPackages = true;
    home-manager.users.alice = {
      home.stateVersion = "26.05";
      programs.nook = {
        enable = true;
        guest.uid = 1234;
        sandboxes.check = {
          stateDirectory = ".sandbox/check";
          shell = pkgs.bashInteractive;
          hostEnvironment = [ "NOOK_TEST_ENTRY" ];
          gpu = false;
          audio = false;
          network = false;
          inheritAppearance = false;
          homeManagerModules = [
            {
              programs.bash.enable = true;
              programs.bash.initExtra = "PS1='NOOK-GUEST-PROMPT> '";
            }
          ];
        };
      };
    };
  };
  testScript = ''
    machine.start()
    machine.wait_for_unit("home-manager-alice.service")
    machine.wait_for_unit("user@1002.service")
    machine.succeed("${pkgs.python3}/bin/python3 ${./interactive-pty.py}")
  '';
}
