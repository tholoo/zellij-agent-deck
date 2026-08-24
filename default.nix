{
  fenix,
  gh,
  git,
  iproute2,
  lib,
  makeRustPlatform,
  makeWrapper,
  python3,
  zellij,
}:
let
  rustToolchain = fenix.combine [
    fenix.latest.rustc
    fenix.latest.cargo
    fenix.targets.wasm32-wasip1.latest.rust-std
  ];
  rustPlatform = makeRustPlatform {
    cargo = rustToolchain;
    rustc = rustToolchain;
  };
in
rustPlatform.buildRustPackage {
  pname = "zellij-agent-deck";
  version = "0.1.0";

  src = lib.cleanSource ./.;

  cargoLock.lockFile = ./Cargo.lock;
  doCheck = false; # Zellij's SDK pulls host-only libraries for native tests.

  nativeBuildInputs = [ makeWrapper ];

  # The standard nixpkgs Cargo hook always adds the host target. Zellij plugins
  # are WASI-only, so avoid a second host build (and its OpenSSL dependency).
  buildPhase = ''
    runHook preBuild
    cargo build --offline --release --target wasm32-wasip1
    runHook postBuild
  '';

  installPhase = ''
    runHook preInstall

    install -Dm644 target/wasm32-wasip1/release/zellij-agent-deck.wasm \
      $out/share/zellij/plugins/agent-deck.wasm
    install -Dm755 agent_deck.py $out/bin/zellij-agent-deck
    install -Dm755 codex_resurrection.py $out/bin/zellij-agent-deck-codex
    patchShebangs $out/bin/zellij-agent-deck $out/bin/zellij-agent-deck-codex
    wrapProgram $out/bin/zellij-agent-deck \
      --prefix PATH : ${
        lib.makeBinPath [
          gh
          git
          iproute2
          python3
          zellij
        ]
      }

    runHook postInstall
  '';

  meta = {
    description = "Floating cross-session Codex agent navigator for Zellij";
    license = lib.licenses.mit;
    mainProgram = "zellij-agent-deck";
    platforms = lib.platforms.linux;
  };
}
