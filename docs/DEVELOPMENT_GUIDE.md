# Development Guide

This guide covers how to build, test, and contribute to wgpu-video.

## Table of Contents

- [Getting Started](#getting-started)
- [Building](#building)
- [Testing](#testing)
- [Code Organization](#code-organization)
- [Adding New Features](#adding-new-features)
- [Debugging](#debugging)
- [Performance Profiling](#performance-profiling)
- [Contributing](#contributing)
- [Release Process](#release-process)

---

## Getting Started

### Prerequisites

**All Platforms:**
- Rust 1.70+ (stable)
- Git
- A GPU with hardware video decoding support

**Windows:**
- Windows 10 or later
- Visual Studio 2019+ with C++ tools
- Windows SDK 10.0.19041.0 or later

**Linux:**
- GCC or Clang
- libva-dev
- libdrm-dev
- Vulkan SDK (optional, for Vulkan Video)

```bash
# Ubuntu/Debian
sudo apt install libva-dev libdrm-dev vulkan-sdk

# Fedora
sudo dnf install libva-devel libdrm-devel vulkan-loader-devel

# Arch
sudo pacman -S libva libdrm vulkan-headers
```

**macOS:**
- Xcode Command Line Tools
- macOS 10.13+ for VideoToolbox support

```bash
xcode-select --install
```

### Clone the Repository

```bash
git clone https://github.com/yourusername/wgpu-video.git
cd wgpu-video
```

### IDE Setup

**VS Code:**
```bash
# Install recommended extensions
code --install-extension rust-lang.rust-analyzer
code --install-extension vadimcn.vscode-lldb
```

Recommended settings (`.vscode/settings.json`):
```json
{
  "rust-analyzer.cargo.features": "all",
  "rust-analyzer.checkOnSave.command": "clippy",
  "rust-analyzer.checkOnSave.extraArgs": ["--all-targets"]
}
```

**CLion/IntelliJ:**
- Install Rust plugin
- Import as Cargo project
- Enable Clippy in settings

---

## Building

### Basic Build

```bash
# Build with default features
cargo build

# Build with all features
cargo build --all-features

# Build optimized (release)
cargo build --release
```

### Feature Flags

```bash
# Windows only (MediaFoundation)
cargo build --features media-foundation

# Linux only (VA-API)
cargo build --features vaapi

# macOS only (VideoToolbox)
cargo build --features videotoolbox

# Cross-platform (Vulkan Video)
cargo build --features vulkan-video

# Software fallback
cargo build --features software

# Build with specific backends
cargo build --features "media-foundation,vulkan-video"
```

### Platform-Specific Builds

**Windows:**
```powershell
# DX12 support (default on Windows)
cargo build --target x86_64-pc-windows-msvc

# Check available backends
cargo run --example capabilities
```

**Linux:**
```bash
# With VA-API support
cargo build --features vaapi

# With Vulkan Video
cargo build --features vulkan-video

# Both backends
cargo build --features "vaapi,vulkan-video"
```

**macOS:**
```bash
# Intel Mac
cargo build --target x86_64-apple-darwin

# Apple Silicon
cargo build --target aarch64-apple-darwin

# Universal binary
cargo build --target universal2-apple-darwin
```

### Cross-Compilation

**Windows to Linux (requires cross):**
```bash
cargo install cross
cross build --target x86_64-unknown-linux-gnu
```

**Linux to Windows:**
```bash
cargo install cross
cross build --target x86_64-pc-windows-gnu
```

---

## Testing

### Unit Tests

```bash
# Run all tests
cargo test

# Run specific test
cargo test test_backend_selection

# Run with logging
RUST_LOG=debug cargo test

# Run tests for specific backend
cargo test --features media-foundation
```

### Integration Tests

```bash
# Run integration tests
cargo test --test decoder_tests

# Run with real video files
cargo test --test decoder_tests -- --ignored

# Test specific platform
cargo test --features vaapi --test vaapi_tests
```

### Test Coverage

```bash
# Install tarpaulin
cargo install cargo-tarpaulin

# Generate coverage report
cargo tarpaulin --out Html --output-dir coverage

# Open coverage report
open coverage/index.html  # macOS
xdg-open coverage/index.html  # Linux
```

### Platform-Specific Tests

**Windows:**
```powershell
# Test MediaFoundation backend
cargo test --features media-foundation -- --test-threads=1

# Test D3D11 interop
cargo test --features media-foundation test_d3d11

# Test D3D12 interop
cargo test --features media-foundation test_d3d12
```

**Linux:**
```bash
# Test VA-API backend
cargo test --features vaapi

# Test DRM interop (requires privileges)
sudo -E cargo test --features vaapi test_drm_interop

# Test Vulkan interop
cargo test --features "vaapi,vulkan-video" test_vulkan_interop
```

**macOS:**
```bash
# Test VideoToolbox backend
cargo test --features videotoolbox

# Test IOSurface handling
cargo test --features videotoolbox test_iosurface
```

### Test Data

Test videos are not included in the repository. Download test files:

```bash
# Download test videos
./scripts/download-test-videos.sh

# Or manually place test files in tests/data/
mkdir -p tests/data
# Place .h264, .h265, .vp9, .av1 files here
```

Test video requirements:
- Multiple resolutions (480p, 720p, 1080p, 4K)
- Multiple codecs (H.264, H.265, VP9, AV1)
- Different profiles (baseline, main, high)
- With and without B-frames
- HDR content (10-bit)

---

## Code Organization

### File Structure

```
src/
├── lib.rs              - Public API exports
├── builder.rs          - VideoDecoderBuilder
├── decoder.rs          - VideoDecoder implementation
├── error.rs            - Error types
├── types.rs            - Common types
├── capabilities.rs     - Capability querying
├── core/               - Core abstractions
├── wgpu_interop/       - wgpu integration
├── backends/           - Platform backends
├── codec/              - Codec-specific utilities
└── utils/              - Helper utilities
```

### Module Guidelines

**Public API (`lib.rs`, `builder.rs`, `decoder.rs`):**
- Stable API surface
- Comprehensive documentation
- Examples for all public functions
- No breaking changes without major version bump

**Core Layer (`core/`):**
- Define traits and abstractions
- No platform-specific code
- Well-documented trait contracts
- Minimal dependencies

**Backend Implementations (`backends/`):**
- One directory per backend
- Implement `DecoderBackend` trait
- Platform-specific code isolated
- Conditional compilation with feature flags

**Tests:**
- Unit tests alongside implementation
- Integration tests in `tests/`
- Platform-specific tests with `#[cfg]`
- Mock implementations for testing

---

## Adding New Features

### Adding a New Backend

1. **Create backend directory:**
```bash
mkdir src/backends/my_backend
touch src/backends/my_backend/mod.rs
touch src/backends/my_backend/decoder.rs
```

2. **Implement DecoderBackend trait:**
```rust
// src/backends/my_backend/decoder.rs
use crate::core::backend_trait::DecoderBackend;

pub struct MyBackendDecoder {
    // Backend-specific fields
}

impl DecoderBackend for MyBackendDecoder {
    fn initialize(&mut self, config: DecoderConfig) -> Result<()> {
        // Initialize native decoder
        todo!()
    }
    
    fn decode_frame(&mut self, data: &[u8]) -> Result<DecodedFrame> {
        // Decode frame
        todo!()
    }
    
    fn flush(&mut self) -> Result<Vec<DecodedFrame>> {
        todo!()
    }
    
    fn reset(&mut self) -> Result<()> {
        todo!()
    }
    
    fn capabilities(&self) -> &BackendCapabilities {
        todo!()
    }
}
```

3. **Register backend:**
```rust
// src/backends/mod.rs
#[cfg(feature = "my-backend")]
pub mod my_backend;

// src/core/backend_selector.rs
pub fn select_backend(device: &Device, codec: Codec) -> Result<BackendType> {
    // Add selection logic
    #[cfg(feature = "my-backend")]
    if should_use_my_backend(device, codec) {
        return Ok(BackendType::MyBackend);
    }
    // ...
}
```

4. **Add feature flag:**
```toml
# Cargo.toml
[features]
my-backend = ["my-backend-sys"]

[dependencies]
my-backend-sys = { version = "0.1", optional = true }
```

5. **Add tests:**
```rust
// src/backends/my_backend/mod.rs
#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_initialization() {
        // Test backend initialization
    }
    
    #[test]
    fn test_decode_frame() {
        // Test frame decoding
    }
}
```

6. **Update documentation:**
- Add backend to `PLATFORM_BACKENDS.md`
- Update feature list in `README.md`
- Add usage examples

### Adding a New Codec

1. **Add codec variant:**
```rust
// src/types.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    H264,
    H265,
    VP9,
    AV1,
    MyCodec,  // New codec
}
```

2. **Add codec parser (if needed):**
```rust
// src/codec/my_codec.rs
pub struct MyCodecParser {
    // Parser state
}

impl MyCodecParser {
    pub fn parse_header(&mut self, data: &[u8]) -> Result<CodecHeader> {
        // Parse codec-specific headers
        todo!()
    }
}
```

3. **Update backend support:**
```rust
// src/backends/*/capabilities.rs
fn query_codec_support(codec: Codec) -> Result<CodecSupport> {
    match codec {
        Codec::MyCodec => {
            // Check if backend supports MyCodec
            check_my_codec_support()
        }
        // ...
    }
}
```

4. **Add tests:**
```rust
#[test]
fn test_my_codec_decoding() {
    let decoder = VideoDecoderBuilder::new()
        .with_codec(Codec::MyCodec)
        .build()
        .unwrap();
    
    let frame = decoder.decode_frame(&test_data).unwrap();
    assert_eq!(frame.metadata().codec, Codec::MyCodec);
}
```

### Adding Texture Format Support

1. **Add format variant:**
```rust
// src/types.rs
pub enum PixelFormat {
    // ...
    MyFormat,
}
```

2. **Add format mapping:**
```rust
// src/wgpu_interop/format_mapping.rs
pub fn decoder_format_to_wgpu(format: PixelFormat) -> wgpu::TextureFormat {
    match format {
        PixelFormat::MyFormat => wgpu::TextureFormat::MyWgpuFormat,
        // ...
    }
}
```

3. **Implement conversion if needed:**
```rust
// src/wgpu_interop/color_conversion.rs
pub fn convert_my_format(
    device: &Device,
    src: &Texture,
    dst: &Texture,
) -> Result<()> {
    // Implement conversion using compute shader
    todo!()
}
```

---

## Debugging

### Enabling Logs

```bash
# Set log level
export RUST_LOG=wgpu_video=debug

# Specific module
export RUST_LOG=wgpu_video::backends::media_foundation=trace

# Run with logging
cargo run --example basic_playback
```

### Using Debugger

**LLDB (macOS/Linux):**
```bash
# Debug tests
rust-lldb -- cargo test test_name

# Debug example
rust-lldb -- target/debug/examples/basic_playback
```

**GDB (Linux):**
```bash
# Debug with GDB
rust-gdb -- cargo test test_name

# With arguments
rust-gdb --args target/debug/examples/basic_playback video.mp4
```

**Visual Studio (Windows):**
1. Build with debug symbols: `cargo build`
2. Open Visual Studio
3. Debug > Attach to Process
4. Select Rust process
5. Set breakpoints in source

### Platform-Specific Debugging

**Windows (MediaFoundation):**
```rust
// Enable MF debug output
use windows::Win32::Media::MediaFoundation::*;

unsafe {
    MFStartup(MF_VERSION, MFSTARTUP_FULL)?;
    // Set debug level
    // Use Windows SDK tools like mftrace.exe
}
```

**Linux (VA-API):**
```bash
# Enable VA-API logging
export LIBVA_TRACE=/tmp/va_trace.log
export LIBVA_DRIVER_NAME=iHD  # or i965

# Run application
cargo run

# Check trace
cat /tmp/va_trace.log
```

**Vulkan Validation:**
```bash
# Enable Vulkan validation layers
export VK_INSTANCE_LAYERS=VK_LAYER_KHRONOS_validation
export VK_LOADER_DEBUG=all

cargo run
```

### Common Issues

**Issue: Decoder fails to initialize**
```rust
// Solution: Check backend availability
let caps = DecoderCapabilities::query(&device)?;
println!("Available backends: {:?}", caps.supported_codecs);
```

**Issue: Texture import fails**
```rust
// Solution: Verify wgpu backend matches decoder backend
let backend_type = detect_wgpu_backend(&device);
println!("wgpu backend: {:?}", backend_type);
```

**Issue: Corrupted frames**
```rust
// Solution: Ensure proper synchronization
decoder.flush()?;  // Flush before checking output
```

---

## Performance Profiling

### CPU Profiling

**Using cargo-flamegraph:**
```bash
# Install
cargo install flamegraph

# Profile
cargo flamegraph --example basic_playback -- video.mp4

# Open flamegraph.svg
```

**Using perf (Linux):**
```bash
# Record
perf record -g cargo run --release --example basic_playback

# Report
perf report
```

**Using Instruments (macOS):**
```bash
# Build release
cargo build --release --example basic_playback

# Profile with Instruments
instruments -t "Time Profiler" target/release/examples/basic_playback
```

### GPU Profiling

**RenderDoc (Windows/Linux):**
1. Launch RenderDoc
2. Set executable to your binary
3. Capture frame
4. Analyze GPU operations

**PIX (Windows):**
1. Launch PIX
2. Attach to process
3. Capture GPU operations
4. Analyze D3D12 calls

**Xcode Instruments (macOS):**
```bash
# Profile Metal usage
instruments -t "Metal System Trace" target/release/examples/basic_playback
```

### Benchmarking

```bash
# Run benchmarks
cargo bench

# Run specific benchmark
cargo bench decode_performance

# Compare with baseline
cargo bench --bench decode_performance -- --save-baseline main

# After changes
cargo bench --bench decode_performance -- --baseline main
```

**Custom benchmark:**
```rust
// benches/decode_performance.rs
use criterion::{criterion_group, criterion_main, Criterion};
use wgpu_video::*;

fn decode_benchmark(c: &mut Criterion) {
    let device = setup_device();
    let test_data = load_test_data();
    
    c.bench_function("decode_h264_1080p", |b| {
        let mut decoder = create_decoder(&device);
        b.iter(|| {
            decoder.decode_frame(&test_data).unwrap();
        });
    });
}

criterion_group!(benches, decode_benchmark);
criterion_main!(benches);
```

---

## Contributing

### Contribution Workflow

1. **Fork and clone:**
```bash
git clone https://github.com/your-username/wgpu-video.git
cd wgpu-video
git remote add upstream https://github.com/original/wgpu-video.git
```

2. **Create branch:**
```bash
git checkout -b feature/my-feature
```

3. **Make changes:**
- Write code
- Add tests
- Update documentation
- Run `cargo fmt`
- Run `cargo clippy`

4. **Test thoroughly:**
```bash
cargo test --all-features
cargo clippy --all-targets --all-features
cargo fmt -- --check
```

5. **Commit:**
```bash
git add .
git commit -m "feat: add new feature"
```

Commit message format:
- `feat:` New feature
- `fix:` Bug fix
- `docs:` Documentation only
- `test:` Adding tests
- `refactor:` Code refactoring
- `perf:` Performance improvement
- `chore:` Maintenance tasks

6. **Push and create PR:**
```bash
git push origin feature/my-feature
```

Create Pull Request on GitHub with:
- Clear description of changes
- Link to related issues
- Screenshots/videos if applicable
- Test results on different platforms

### Code Style

**Format code:**
```bash
cargo fmt
```

**Check lints:**
```bash
cargo clippy --all-targets --all-features -- -D warnings
```

**Custom clippy config (`.clippy.toml`):**
```toml
cognitive-complexity-threshold = 30
```

### Documentation

**Document public APIs:**
```rust
/// Decodes a single video frame.
///
/// # Arguments
///
/// * `data` - Encoded frame data (NAL unit, OBU, etc.)
///
/// # Returns
///
/// A `DecodedFrame` containing the decoded texture and metadata.
///
/// # Errors
///
/// Returns `DecoderError::CorruptedData` if the input is invalid.
///
/// # Example
///
/// ```
/// # use wgpu_video::*;
/// # let mut decoder = create_test_decoder();
/// let data = vec![0, 0, 1, /* ... */];
/// let frame = decoder.decode_frame(&data)?;
/// ```
pub fn decode_frame(&mut self, data: &[u8]) -> Result<DecodedFrame> {
    // ...
}
```

**Build docs:**
```bash
cargo doc --all-features --no-deps --open
```

### Testing Requirements

For all contributions:
- [ ] Unit tests for new code
- [ ] Integration tests for new features
- [ ] Documentation for public APIs
- [ ] Examples if adding major features
- [ ] Updated CHANGELOG.md

### PR Review Process

1. Automated checks run (CI)
2. Code review by maintainers
3. Address feedback
4. Approval from maintainer
5. Merge

---

## Release Process

### Version Numbering

Follow [Semantic Versioning](https://semver.org/):
- MAJOR: Breaking API changes
- MINOR: New features, backward compatible
- PATCH: Bug fixes

### Release Checklist

1. **Update version:**
```toml
# Cargo.toml
[package]
version = "0.2.0"
```

2. **Update CHANGELOG.md:**
```markdown
## [0.2.0] - 2024-01-15

### Added
- New backend support for XYZ

### Changed
- Improved performance of ABC

### Fixed
- Fixed issue with DEF
```

3. **Test all platforms:**
```bash
# Run full test suite
cargo test --all-features

# Test on all platforms (CI should cover this)
```

4. **Build documentation:**
```bash
cargo doc --all-features --no-deps
```

5. **Create git tag:**
```bash
git tag -a v0.2.0 -m "Release v0.2.0"
git push origin v0.2.0
```

6. **Publish to crates.io:**
```bash
cargo publish --dry-run
cargo publish
```

7. **Create GitHub release:**
- Go to GitHub releases
- Create new release from tag
- Copy CHANGELOG entry
- Upload any assets (examples, benchmarks)

---

## Additional Resources

### Documentation
- [wgpu Documentation](https://docs.rs/wgpu)
- [Rust Book](https://doc.rust-lang.org/book/)
- [Cargo Book](https://doc.rust-lang.org/cargo/)

### Video Codec Resources
- [H.264 Specification](https://www.itu.int/rec/T-REC-H.264)
- [FFmpeg Documentation](https://ffmpeg.org/documentation.html)
- [Vulkan Video Specification](https://www.khronos.org/vulkan/)

### Platform APIs
- [MediaFoundation Docs](https://docs.microsoft.com/en-us/windows/win32/medfound/)
- [VA-API Documentation](https://01.org/vaapi)
- [VideoToolbox Framework](https://developer.apple.com/documentation/videotoolbox)

### Community
- [GitHub Discussions](https://github.com/your-org/wgpu-video/discussions)
- [Discord Server](#)
- [Issue Tracker](https://github.com/your-org/wgpu-video/issues)

---

## Getting Help

If you encounter issues:

1. Check the [documentation](./README.md)
2. Search [existing issues](https://github.com/your-org/wgpu-video/issues)
3. Ask in [discussions](https://github.com/your-org/wgpu-video/discussions)
4. Create a [new issue](https://github.com/your-org/wgpu-video/issues/new)

When reporting issues, include:
- Rust version (`rustc --version`)
- wgpu-video version
- Operating system and version
- GPU model and driver version
- Minimal reproduction code
- Error messages and logs