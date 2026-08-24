{
  description = "Floating cross-session Codex agent navigator for Zellij";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      fenix,
      ...
    }:
    let
      forAllSystems = nixpkgs.lib.genAttrs [
        "x86_64-linux"
        "aarch64-linux"
      ];
    in
    {
      lib = import ./nix/lib.nix {
        inherit self;
        inherit (nixpkgs) lib;
      };

      homeManagerModules.default = import ./nix/home-manager.nix { inherit self; };

      packages = forAllSystems (
        system:
        let
          pkgs = import nixpkgs {
            inherit system;
            overlays = [ fenix.overlays.default ];
          };
        in
        rec {
          zellij-agent-deck = pkgs.callPackage ./default.nix { };
          default = zellij-agent-deck;
        }
      );

      devShells = forAllSystems (
        system:
        let
          pkgs = import nixpkgs {
            inherit system;
            overlays = [ fenix.overlays.default ];
          };
          rustToolchain = pkgs.fenix.combine [
            pkgs.fenix.latest.rustc
            pkgs.fenix.latest.cargo
            pkgs.fenix.latest.clippy
            pkgs.fenix.latest.rustfmt
            pkgs.fenix.targets.wasm32-wasip1.latest.rust-std
          ];
        in
        {
          default = pkgs.mkShell {
            packages = [
              rustToolchain
              pkgs.gitleaks
              pkgs.mypy
              pkgs.nixfmt
              pkgs.openssl
              pkgs.pkg-config
              pkgs.pre-commit
              pkgs.python3
              pkgs.ruff
            ];
            LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath [
              pkgs.openssl
              pkgs.zlib
            ];
          };
        }
      );

      checks = forAllSystems (
        system:
        let
          pkgs = import nixpkgs {
            inherit system;
            overlays = [ fenix.overlays.default ];
          };
          manifest = builtins.fromTOML (builtins.readFile ./Cargo.toml);
          source = pkgs.lib.cleanSource ./.;
          rustToolchain = pkgs.fenix.combine [
            pkgs.fenix.latest.rustc
            pkgs.fenix.latest.cargo
          ];
          rustPlatform = pkgs.makeRustPlatform {
            cargo = rustToolchain;
            rustc = rustToolchain;
          };
        in
        {
          package = self.packages.${system}.default;

          nix-library =
            let
              requirements = self.lib.mkCodexRequirements {
                agentDeckCommand = "agent-deck-test";
                resurrectionCommand = "resurrection-test";
              };
            in
            pkgs.runCommand "zellij-agent-deck-nix-library-check" { } ''
              test ${pkgs.lib.escapeShellArg self.lib.requiredZellijVersion} = 0.45.0
              printf '%s' ${pkgs.lib.escapeShellArg (builtins.toJSON requirements)} > $out
              grep -q agent-deck-test $out
              grep -q resurrection-test $out
            '';

          python = pkgs.stdenvNoCC.mkDerivation {
            pname = "zellij-agent-deck-python-checks";
            inherit (manifest.package) version;
            src = source;
            nativeBuildInputs = [
              pkgs.mypy
              pkgs.python3
              pkgs.ruff
            ];
            dontConfigure = true;
            dontBuild = true;
            doCheck = true;
            checkPhase = ''
              runHook preCheck
              export HOME=$TMPDIR
              export MYPY_CACHE_DIR=$TMPDIR/mypy
              export RUFF_CACHE_DIR=$TMPDIR/ruff
              ruff check agent_deck.py codex_resurrection.py tests
              ruff format --check agent_deck.py codex_resurrection.py tests
              mypy agent_deck.py codex_resurrection.py
              python3 -m unittest discover -s tests -v
              runHook postCheck
            '';
            installPhase = ''
              touch $out
            '';
          };

          rust-tests = rustPlatform.buildRustPackage {
            pname = "zellij-agent-deck-rust-tests";
            inherit (manifest.package) version;
            src = source;
            cargoLock.lockFile = ./Cargo.lock;
            nativeBuildInputs = [ pkgs.pkg-config ];
            buildInputs = [ pkgs.openssl ];
            LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath [
              pkgs.openssl
              pkgs.zlib
            ];
            buildPhase = ''
              runHook preBuild
              cargo test --all-targets
              runHook postBuild
            '';
            doCheck = false;
            installPhase = ''
              touch $out
            '';
          };
        }
      );

      formatter = forAllSystems (
        system:
        let
          pkgs = import nixpkgs { inherit system; };
        in
        pkgs.nixfmt
      );
    };
}
