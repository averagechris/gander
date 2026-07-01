{
  description = "jj-change-viewer: a fast Rust TUI for reviewing jj changes";

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
      nativeBuildInputs = with pkgs; [pkg-config];
      buildInputs = with pkgs; lib.optionals stdenv.isDarwin [apple-sdk_15];
    in {
      packages.default = pkgs.rustPlatform.buildRustPackage {
        pname = "jj-change-viewer";
        version = "0.1.0";
        src = self;
        cargoLock.lockFile = ./Cargo.lock;
        inherit nativeBuildInputs buildInputs;
      };

      apps.default = {
        type = "app";
        program = "${self.packages.${system}.default}/bin/jj-change-viewer";
        meta.description = "Fast Rust TUI for reviewing jj changes";
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
