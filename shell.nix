{ pkgs ? import <nixpkgs> {} }:

pkgs.mkShell {
  nativeBuildInputs = with pkgs; [ 
    pkg-config 
    rustc 
    cargo 
    rust-analyzer 
    rustfmt 
    clippy 
  ];

  buildInputs = with pkgs; [
    git
  ];

  RUST_SRC_PATH = pkgs.rustPlatform.rustLibSrc;

  shellHook = ''
    echo "⚔️ Martyr Dev Shell Active"
    echo "Rust Version: $(rustc --version)"
  '';
}
