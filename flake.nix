{
  description = "Development environment for wgpu-video";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay.url = "github:oxalica/rust-overlay";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, rust-overlay, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs {
          inherit system overlays;
        };

        rustToolchain = pkgs.rust-bin.stable.latest.default.override {
          extensions = [ "rust-src" "rust-analyzer" ];
        };

        # Libraries needed for wgpu and winit on Linux
        buildInputs = with pkgs; [
          rustToolchain

          # Build tools
          pkg-config
          clang
          llvmPackages.libclang

          # Graphics and video libraries
          vulkan-loader
          vulkan-headers
          vulkan-tools

          # Winit dependencies
          libX11
          libXcursor
          libXrandr
          libXi
          libxkbcommon
          wayland

          # Video codec libraries
          libva
          libdrm
          ffmpeg
        ];

        nativeBuildInputs = with pkgs; [
          pkg-config
        ];

      in
      {
        devShells.default = pkgs.mkShell {
          inherit buildInputs nativeBuildInputs;

          # Environment variables
          LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath buildInputs;
          LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";

          shellHook = ''
            echo "wgpu-video development environment"
            echo "Rust version: $(rustc --version)"
            echo ""
            echo "Available commands:"
            echo "  cargo build    - Build the project"
            echo "  cargo run      - Run the project"
            echo "  cargo test     - Run tests"
            echo "  cargo clippy   - Run linter"
          '';
        };
      }
    );
}
