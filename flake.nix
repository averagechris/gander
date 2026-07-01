{
  description = "gander: take a gander at your jj changes in a fast review TUI";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = {
    self,
    nixpkgs,
    flake-utils,
  }:
    flake-utils.lib.eachDefaultSystem (system: let
      pkgs = import nixpkgs {inherit system;};
      cargoToml = pkgs.lib.importTOML ./Cargo.toml;
      nativeBuildInputs = with pkgs; [pkg-config];
      buildInputs = with pkgs; lib.optionals stdenv.isDarwin [apple-sdk_15];
    in {
      packages.default = pkgs.rustPlatform.buildRustPackage {
        pname = cargoToml.package.name;
        inherit (cargoToml.package) version;
        src = self;
        cargoLock.lockFile = ./Cargo.lock;
        inherit nativeBuildInputs buildInputs;

        meta = {
          inherit (cargoToml.package) description homepage;
          license = with pkgs.lib.licenses; [mit asl20];
          mainProgram = cargoToml.package.name;
        };
      };

      apps.default = {
        type = "app";
        program = pkgs.lib.getExe self.packages.${system}.default;
        meta.description = cargoToml.package.description;
      };

      devShells.default = pkgs.mkShell {
        packages = with pkgs; [
          alejandra
          cargo
          cargo-audit
          cargo-machete
          cargo-deny
          cargo-nextest
          cargo-sort
          clippy
          deadnix
          jujutsu
          nil
          rust-analyzer
          rustc
          rustfmt
          statix
          taplo
          typos
        ];
        inherit nativeBuildInputs buildInputs;
        RUST_BACKTRACE = "1";
      };
    });
}
