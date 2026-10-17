{
  rustPlatform,
  makeWrapper,
  installShellFiles,
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
  bash,
  fish,
  zsh,
  python3,
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
      ./tests/completions.py
    ];
  };
  cargoLock.lockFile = ./Cargo.lock;
  nativeBuildInputs = [
    makeWrapper
    installShellFiles
    pkg-config
  ];
  buildInputs = [ dbus ];
  nativeCheckInputs = [ cargo-deny ];
  postCheck = ''
    cargo deny --offline --locked check licenses
  '';
  doInstallCheck = true;
  nativeInstallCheckInputs = [
    bash
    fish
    zsh
    python3
  ];
  installCheckPhase = ''
    runHook preInstallCheck
    python3 tests/completions.py "$out"
    runHook postInstallCheck
  '';
  postInstall = ''
    for name in nk sandbox; do
      COMPLETE=bash NOOK_COMPLETE_BIN=$name $out/bin/nk > $name.bash
      COMPLETE=zsh NOOK_COMPLETE_BIN=$name $out/bin/nk > _$name
      COMPLETE=fish NOOK_COMPLETE_BIN=$name $out/bin/nk > $name.fish
      installShellCompletion --bash --name $name $name.bash
      installShellCompletion --zsh --name _$name _$name
      installShellCompletion --fish --name $name.fish $name.fish
    done
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
