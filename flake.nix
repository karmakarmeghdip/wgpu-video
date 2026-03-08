{
  description = "Development environment for wgpu-video";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay.url = "github:oxalica/rust-overlay";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs =
    {
      self,
      nixpkgs,
      rust-overlay,
      flake-utils,
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs {
          inherit system overlays;
        };

        rustToolchain = pkgs.rust-bin.stable.latest.default.override {
          extensions = [
            "rust-src"
            "rust-analyzer"
          ];
          targets = [ "x86_64-pc-windows-msvc" ];
        };

        # Libraries needed for wgpu and winit on Linux
        buildInputs = with pkgs; [
          rustToolchain

          # Build tools
          pkg-config
          clang
          llvmPackages.libclang
          cargo-xwin

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

          # GBM and Mesa (required for wgpu DRM/KMS backend)
          libgbm

          # Video codec libraries
          libva
          libdrm
          ffmpeg

          # Audio libraries
          alsa-lib
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
          RUSTFLAGS = "-L ${pkgs.libgbm}/lib";

          # Windows MSVC cross-compilation via cargo-xwin
          # cargo-xwin downloads the MSVC SDK/CRT automatically; point it at clang-cl/lld-link
          CC_x86_64_pc_windows_msvc = "${pkgs.llvmPackages.clang-unwrapped}/bin/clang-cl";
          CXX_x86_64_pc_windows_msvc = "${pkgs.llvmPackages.clang-unwrapped}/bin/clang-cl";
          CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER = "${pkgs.llvmPackages.lld}/bin/lld-link";

          shellHook = ''
            echo "wgpu-video development environment"
            echo "Rust version: $(rustc --version)"
            echo ""
            echo "Available commands:"
            echo "  cargo build    - Build the project"
            echo "  cargo run      - Run the project"
            echo "  cargo test     - Run tests"
            echo "  cargo clippy   - Run linter"
            echo "  cargo xwin build --target x86_64-pc-windows-msvc - Cross-compile for Windows"
          '';
        };
      }
    );
}
