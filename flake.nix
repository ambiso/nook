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
          screenshot =
            pkgs.runCommand "nook-tui.png"
              {
                nativeBuildInputs = with pkgs; [
                  bubblewrap
                  dbus
                  feh
                  gnome-console
                  imagemagick
                  python3
                  xcompmgr
                  xdotool
                  xvfb-run
                ];
                FONTCONFIG_FILE = pkgs.makeFontsConf {
                  fontDirectories = with pkgs; [
                    adwaita-fonts
                    dejavu_fonts
                  ];
                };
              }
              ''
                export HOME="$TMPDIR/home" XDG_RUNTIME_DIR="$TMPDIR/runtime"
                mkdir -p "$HOME"
                mkdir -m 0700 "$XDG_RUNTIME_DIR"
                xvfb-run -a -s '-screen 0 1440x900x24 -nolisten tcp -dpi 96' \
                  dbus-run-session --config-file=${pkgs.dbus}/share/dbus-1/session.conf \
                  -- python3 ${./scripts/render-screenshot.py} \
                  ${self.packages.${system}.nook}/bin/.nk-wrapped "$out"
              '';
          arch-host = pkgs.runCommand "nook-arch-host" { } ''
            mkdir -p "$out/bin"
            ln -s ${self.packages.${system}.nook}/bin/nook-host-install "$out/bin/nook-host-install"
          '';
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
        module-identity = import ./tests/module-identity.nix {
          pkgs = pkgsFor system;
          inherit home-manager;
        };
        arch-host-profile = import ./tests/arch-host-profile.nix {
          pkgs = pkgsFor system;
          inherit home-manager;
        };
        arch-host-activation = import ./tests/arch-host-profile.nix {
          pkgs = pkgsFor system;
          inherit home-manager;
          output = "activation";
        };
        arch-host-empty-activation = import ./tests/arch-host-profile.nix {
          pkgs = pkgsFor system;
          inherit home-manager;
          enabled = false;
          output = "activation";
        };
        integration = import ./tests/sandbox-systemd.nix {
          pkgs = pkgsFor system;
          inherit home-manager;
        };
        portal-uid-matched = import ./tests/portal-uid.nix {
          pkgs = pkgsFor system;
          inherit home-manager;
        };
        proxy-client-uid-matched = import ./tests/proxy-client-uid.nix {
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
              cargo-deny
              bubblewrap
              pkg-config
              dbus.dev
              dbus
              dconf
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
