{
  rustPlatform,
  makeWrapper,
  lib,
  systemd,
  coreutils,
  util-linux,
  iproute2,
  passt,
  xdg-dbus-proxy,
  dbus,
  pkg-config,
  cargo-deny,
  bubblewrap,
  bindfs,
  nix,
}:
rustPlatform.buildRustPackage {
  pname = "nook";
  version = "0.1.0";
  src = lib.fileset.toSource {
    root = ./.;
    fileset = lib.fileset.unions [
      ./Cargo.toml
      ./Cargo.lock
      ./LICENSE
      ./deny.toml
      ./src
    ];
  };
  cargoLock.lockFile = ./Cargo.lock;
  nativeBuildInputs = [
    makeWrapper
    pkg-config
  ];
  buildInputs = [ dbus ];
  nativeCheckInputs = [ cargo-deny ];
  postCheck = ''
    cargo deny --offline --locked check licenses
  '';
  postInstall = ''
    install -Dm644 LICENSE "$out/share/licenses/nook/LICENSE"
    wrapProgram "$out/bin/nk" --prefix PATH : ${
      lib.makeBinPath [
        systemd
        coreutils
        util-linux
        iproute2
        passt
        xdg-dbus-proxy
        bubblewrap
        bindfs
        nix
      ]
    }
  '';
  meta = {
    description = "NixOS sandboxes managed by systemd, with the nk launcher";
    homepage = "https://github.com/ambiso/nook";
    license = lib.licenses.eupl12;
    mainProgram = "nk";
    platforms = lib.platforms.linux;
  };
}
