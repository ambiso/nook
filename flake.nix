{
  description = "Nook: NixOS sandboxes managed by systemd, with the nk launcher";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    home-manager = {
      url = "github:nix-community/home-manager";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      home-manager,
    }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
      pkgsFor = system: import nixpkgs { inherit system; };
    in
    {
      nixosModules.default = import ./nix/nixos.nix { inherit home-manager; };
      homeManagerModules.default = import ./nix/home-manager.nix { inherit home-manager; };
      overlays.default = final: _: { nook = final.callPackage ./. { }; };

      packages = forAllSystems (
        system:
        let
          pkgs = pkgsFor system;
        in
        {
          nook = pkgs.callPackage ./. { };
          default = self.packages.${system}.nook;
        }
      );
      apps = forAllSystems (system: {
        default = {
          type = "app";
          program = "${self.packages.${system}.nook}/bin/nk";
          meta = self.packages.${system}.nook.meta;
        };
      });
      checks = forAllSystems (system: {
        package = self.packages.${system}.nook;
        integration = import ./tests/sandbox-systemd.nix {
          pkgs = pkgsFor system;
          inherit home-manager;
        };
      });
      formatter = forAllSystems (system: (pkgsFor system).nixfmt);
      devShells = forAllSystems (
        system:
        let
          pkgs = pkgsFor system;
        in
        {
          default = pkgs.mkShell {
            packages = with pkgs; [
              cargo
              rustc
              rustfmt
              clippy
              pkg-config
              dbus.dev
            ];
            # Pin the compiler as well as Cargo, even when the user's PATH also
            # contains rustup shims or a dependency has its own toolchain file.
            RUSTC = "${pkgs.rustc}/bin/rustc";
            RUSTDOC = "${pkgs.rustc}/bin/rustdoc";
          };
        }
      );
    };
}
