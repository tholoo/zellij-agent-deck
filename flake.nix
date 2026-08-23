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
    { nixpkgs, fenix, ... }:
    let
      forAllSystems = nixpkgs.lib.genAttrs [
        "x86_64-linux"
        "aarch64-linux"
      ];
    in
    {
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

      formatter = forAllSystems (
        system:
        let
          pkgs = import nixpkgs { inherit system; };
        in
        pkgs.nixfmt
      );
    };
}
