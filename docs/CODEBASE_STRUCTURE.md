# Codebase Structure

This document outlines the detailed file and module structure for the wgpu-video library.

## Project Root Structure

```
wgpu-video/
├── src/
│   ├── lib.rs                 # Library root, public API exports
│   ├── builder.rs             # VideoDecoderBuilder implementation
│   ├── decoder.rs             # VideoDecoder implementation
│   ├── error.rs               # Error types and Result aliases
│   ├── types.rs               # Common types (Codec, Format, etc.)
│   ├── capabilities.rs        # Capability querying
│   │
│   ├── core/                  # Core abstraction layer
│   │   ├── mod.rs
│   │   ├── backend_trait.rs   # DecoderBackend trait definition
│   │   ├── backend_selector.rs # Backend selection logic
│   │   ├── texture_interop.rs # Texture sharing utilities
│   │   ├── sync.rs            # Synchronization primitives
│   │   └── frame.rs           # DecodedFrame implementation
│   │
│   ├── wgpu_interop/          # wgpu integration utilities
│   │   ├── mod.rs
│   │   ├── device_info.rs     # Detect wgpu backend type
│   │   ├── texture_factory.rs # Create wgpu textures from native handles
│   │   ├── format_mapping.rs  # Map decoder formats to wgpu formats
│   │   └── color_conversion.rs # YUV to RGB conversion shaders
│   │
│   ├── backends/              # Platform-specific implementations
│   │   ├── mod.rs             # Backend registry and selection
│   │   │
│   │   ├── media_foundation/  # Windows MediaFoundation backend
│   │   │   ├── mod.rs
│   │   │   ├── decoder.rs     # MF decoder implementation
│   │   │   ├── transform.rs   # IMFTransform wrapper
│   │   │   ├── d3d11_interop.rs # D3D11 texture sharing
│   │   │   ├── d3d12_interop.rs # D3D12 texture sharing
│   │   │   ├── dxgi.rs        # DXGI utilities
│   │   │   └── capabilities.rs # Query MF capabilities
│   │   │
│   │   ├── vaapi/             # Linux VA-API backend
│   │   │   ├── mod.rs
│   │   │   ├── decoder.rs     # VA-API decoder implementation
│   │   │   ├── display.rs     # VADisplay management
│   │   │   ├── surface.rs     # VASurface handling
│   │   │   ├── drm_interop.rs # DRM/DMA-BUF for zero-copy
│   │   │   ├── vulkan_interop.rs # Vulkan image import
│   │   │   └── capabilities.rs # Query VA-API capabilities
│   │   │
│   │   ├── videotoolbox/      # macOS VideoToolbox backend
│   │   │   ├── mod.rs
│   │   │   ├── decoder.rs     # VideoToolbox decoder
│   │   │   ├── session.rs     # Decompression session
│   │   │   ├── iosurface.rs   # IOSurface handling
│   │   │   ├── metal_interop.rs # Metal texture sharing
│   │   │   └── capabilities.rs # Query VT capabilities
│   │   │
│   │   ├── vulkan_video/      # Cross-platform Vulkan Video
│   │   │   ├── mod.rs
│   │   │   ├── decoder.rs     # Vulkan Video decoder
│   │   │   ├── session.rs     # Video session management
│   │   │   ├── dpb.rs         # Decoded picture buffer
│   │   │   ├── extensions.rs  # Query Vulkan Video extensions
│   │   │   └── capabilities.rs # Query codec support
│   │   │
│   │   └── software/          # Software fallback (future)
│   │       ├── mod.rs
│   │       └── decoder.rs     # CPU decoder (libavcodec wrapper)
│   │
│   ├── codec/                 # Codec-specific utilities
│   │   ├── mod.rs
│   │   ├── h264.rs            # H.264/AVC specific parsing
│   │   ├── h265.rs            # H.265/HEVC specific parsing
│   │   ├── vp9.rs             # VP9 specific parsing
│   │   ├── av1.rs             # AV1 specific parsing
│   │   └── parser.rs          # Common parsing utilities
│   │
│   └── utils/                 # Utility modules
│       ├── mod.rs
│       ├── pool.rs            # Resource pooling
│       ├── logging.rs         # Logging setup
│       └── version.rs         # Version checking utilities
│
├── examples/                  # Example applications
│   ├── basic_playback.rs      # Simple video playback
│   ├── multi_decoder.rs       # Multiple concurrent decoders
│   ├── benchmark.rs           # Performance testing
│   └── format_conversion.rs   # Format handling examples
│
├── tests/                     # Integration tests
│   ├── common/                # Shared test utilities
│   │   ├── mod.rs
│   │   ├── test_files.rs      # Test video file management
│   │   └── mock_device.rs     # Mock wgpu device for testing
│   │
│   ├── decoder_tests.rs       # Core decoder tests
│   ├── backend_selection.rs   # Backend selection tests
│   ├── texture_interop.rs     # Texture sharing tests
│   └── error_handling.rs      # Error path tests
│
├── benches/                   # Benchmarks
│   ├── decode_performance.rs  # Decode speed benchmarks
│   └── memory_usage.rs        # Memory profiling
│
├── docs/                      # Documentation
│   ├── README.md
│   ├── ARCHITECTURE.md
│   ├── CODEBASE_STRUCTURE.md  # This file
│   ├── PLATFORM_BACKENDS.md
│   ├── API_DESIGN.md
│   ├── DEVELOPMENT_GUIDE.md
│   └── INTEGRATION_GUIDE.md
│
├── .github/                   # GitHub specific files
│   └── workflows/
│       ├── ci.yml             # CI pipeline
│       └── release.yml        # Release automation
│
├── Cargo.toml                 # Project manifest
├── Cargo.lock                 # Dependency lock file
├── build.rs                   # Build script (if needed)
├── README.md                  # Project README
├── LICENSE                    # License file
└── .gitignore                 # Git ignore rules
```

## Module Organization

### `lib.rs` - Library Root

**Purpose:** Define public API surface and re-exports

**Contents:**
- Public exports of main types
- Feature flags configuration
- Module declarations
- Prelude module for convenient imports

**Example Structure:**
```rust
// Feature flags
#[cfg(windows)]
pub mod backends::media_foundation;

// Public API exports
pub use builder::VideoDecoderBuilder;
pub use decoder::VideoDecoder;
pub use core::frame::DecodedFrame;
pub use types::{Codec, Format, ColorSpace};
pub use error::{DecoderError, Result};
pub use capabilities::DecoderCapabilities;

// Convenience prelude
pub mod prelude {
    pub use crate::{VideoDecoder, VideoDecoderBuilder, DecodedFrame};
}
```

### `builder.rs` - Decoder Builder

**Purpose:** Fluent API for configuring and creating decoders

**Key Types:**
- `VideoDecoderBuilder`: Main builder struct
- Builder methods for configuration
- Validation logic before build

**Responsibilities:**
- Collect decoder configuration
- Validate configuration compatibility
- Invoke backend selection
- Create VideoDecoder instance

### `decoder.rs` - Main Decoder

**Purpose:** Primary user-facing decoder handle

**Key Types:**
- `VideoDecoder`: Main struct wrapping backend implementation
- Public methods: `decode_frame()`, `reset()`, `flush()`, etc.

**Responsibilities:**
- Delegate to backend implementation
- Handle synchronization
- Manage decoder state
- Provide user-friendly API

### `error.rs` - Error Handling

**Purpose:** Define comprehensive error types

**Key Types:**
- `DecoderError`: Main error enum
- `Result<T>`: Type alias for `Result<T, DecoderError>`
- Error context and chaining

**Error Categories:**
- Initialization errors
- Decoding errors
- Interop errors
- Configuration errors

### `types.rs` - Common Types

**Purpose:** Shared type definitions

**Key Types:**
- `Codec`: Enum of supported codecs (H264, H265, VP9, AV1)
- `PixelFormat`: Video pixel formats
- `ColorSpace`: Color space information (BT.601, BT.709, BT.2020)
- `Resolution`: Video dimensions
- `FrameMetadata`: Per-frame information

### `capabilities.rs` - Capability Querying

**Purpose:** Query decoder and platform capabilities

**Key Types:**
- `DecoderCapabilities`: What's supported
- `CodecInfo`: Codec-specific information
- Capability query functions

**Functions:**
- `query_platform_capabilities()`
- `is_codec_supported(codec, backend)`
- `get_supported_formats()`

## Core Module (`core/`)

### `backend_trait.rs` - Backend Trait

**Purpose:** Define interface all backends must implement

**Key Trait:**
```rust
pub trait DecoderBackend: Send + Sync {
    fn initialize(&mut self, config: DecoderConfig) -> Result<()>;
    fn decode_frame(&mut self, data: &[u8]) -> Result<DecodedFrame>;
    fn flush(&mut self) -> Result<Vec<DecodedFrame>>;
    fn reset(&mut self) -> Result<()>;
    fn capabilities(&self) -> &BackendCapabilities;
}
```

### `backend_selector.rs` - Backend Selection

**Purpose:** Choose optimal backend based on platform and wgpu backend

**Key Functions:**
- `select_backend(device: &Device, codec: Codec) -> BackendType`
- `detect_wgpu_backend(device: &Device) -> WgpuBackendType`
- Priority-based selection logic

**Selection Strategy:**
- Query wgpu backend type
- Check platform
- Match optimal backend
- Validate availability
- Return backend type or error

### `texture_interop.rs` - Texture Sharing

**Purpose:** Handle texture sharing between decoder and wgpu

**Key Types:**
- `TextureHandle`: Platform-agnostic texture handle
- `TextureInterop`: Trait for texture import/export

**Responsibilities:**
- Import native textures into wgpu
- Handle synchronization
- Manage texture lifecycle
- Format conversion coordination

### `sync.rs` - Synchronization

**Purpose:** Cross-API synchronization primitives

**Key Types:**
- `DecoderFence`: Platform-agnostic fence
- `SyncPoint`: Synchronization point wrapper

**Platform Implementations:**
- D3D12: Shared fences
- Vulkan: Semaphores/timeline semaphores
- Metal: MTLSharedEvent

### `frame.rs` - Decoded Frame

**Purpose:** Container for decoded video frame

**Key Type:**
```rust
pub struct DecodedFrame {
    texture: wgpu::Texture,
    metadata: FrameMetadata,
    format: PixelFormat,
    timestamp: Option<u64>,
    // Internal: sync primitives, pool handle
}
```

**Methods:**
- `texture() -> &wgpu::Texture`
- `metadata() -> &FrameMetadata`
- `create_view() -> wgpu::TextureView`

## wgpu Interop Module (`wgpu_interop/`)

### `device_info.rs` - Device Information

**Purpose:** Extract information from wgpu device

**Key Functions:**
- `get_backend_type(device: &Device) -> WgpuBackendType`
- `get_adapter_info(device: &Device) -> AdapterInfo`
- `supports_external_texture(device: &Device) -> bool`

### `texture_factory.rs` - Texture Creation

**Purpose:** Create wgpu textures from native handles

**Key Functions:**
- `create_from_d3d11(device: &Device, d3d11_texture: *mut ID3D11Texture2D)`
- `create_from_d3d12(device: &Device, d3d12_resource: *mut ID3D12Resource)`
- `create_from_vkimage(device: &Device, vk_image: VkImage)`
- `create_from_iosurface(device: &Device, iosurface: IOSurfaceRef)`

### `format_mapping.rs` - Format Conversion

**Purpose:** Map between decoder and wgpu formats

**Key Functions:**
- `decoder_format_to_wgpu(format: PixelFormat) -> wgpu::TextureFormat`
- `wgpu_format_to_decoder(format: wgpu::TextureFormat) -> PixelFormat`
- `needs_conversion(from: PixelFormat, to: wgpu::TextureFormat) -> bool`

### `color_conversion.rs` - Color Space Conversion

**Purpose:** YUV to RGB conversion using compute shaders

**Key Components:**
- Compute shader for YUV->RGB
- Conversion matrix management
- Bind group setup

## Backends Module (`backends/`)

### Platform Backend Structure (Example: MediaFoundation)

Each backend follows a similar structure:

#### `mod.rs` - Backend Module Root
- Re-exports
- Platform-specific initialization
- Backend registration

#### `decoder.rs` - Main Decoder Implementation
- Implements `DecoderBackend` trait
- Manages decoder state
- Handles native API calls

#### Platform-Specific Interop Files
- `d3d11_interop.rs` / `d3d12_interop.rs`: Direct3D texture sharing
- `vulkan_interop.rs`: Vulkan image import
- `metal_interop.rs`: Metal texture sharing
- `drm_interop.rs`: DRM/DMA-BUF handling

#### `capabilities.rs` - Capability Queries
- Query what the backend supports
- Codec availability
- Format support
- Hardware feature detection

### Backend-Specific Notes

#### MediaFoundation Backend (`media_foundation/`)
- Uses Windows-rs for COM interop
- IMFTransform for hardware acceleration
- D3D11 for compatibility, D3D12 for performance
- Handle DXGI device management

#### VA-API Backend (`vaapi/`)
- FFI bindings to libva
- VADisplay management per GPU
- VASurface pooling
- DRM PRIME for zero-copy to Vulkan
- EGL interop as fallback

#### VideoToolbox Backend (`videotoolbox/`)
- Objective-C runtime bindings
- VTDecompressionSession management
- CMSampleBuffer handling
- IOSurface for zero-copy to Metal
- PixelBuffer management

#### Vulkan Video Backend (`vulkan_video/`)
- Pure Vulkan implementation
- Query device for video extensions
- Manage video session and parameters
- DPB (Decoded Picture Buffer) management
- Direct VkImage output

## Codec Module (`codec/`)

### Purpose
Codec-specific parsing and utilities (not full decoding)

### Per-Codec Files

Each codec file contains:
- Parameter set parsing (SPS/PPS for H.264)
- Metadata extraction
- Bitstream utilities
- Codec-specific configuration

**Example for H.264:**
- Parse SPS to get resolution, profile, level
- Extract timing information
- Handle annexB vs AVCC format

## Utils Module (`utils/`)

### `pool.rs` - Resource Pooling

**Purpose:** Pool decode surfaces and textures

**Key Type:**
```rust
pub struct ResourcePool<T> {
    available: Vec<T>,
    in_use: Vec<T>,
    allocator: Box<dyn Fn() -> T>,
}
```

### `logging.rs` - Logging Setup

**Purpose:** Configure structured logging with tracing

### `version.rs` - Version Utilities

**Purpose:** Check API versions, driver versions

## Testing Structure

### Unit Tests
- Located in same file as implementation using `#[cfg(test)]`
- Test individual functions and methods
- Mock external dependencies

### Integration Tests (`tests/`)
- End-to-end testing
- Real decoder usage
- Platform-specific tests with `#[cfg(target_os = "...")]`

### Common Test Utilities (`tests/common/`)
- Shared fixtures
- Test video files
- Mock implementations
- Helper functions

## Build Configuration

### `Cargo.toml` Features

```toml
[features]
default = ["native-backend"]

# Platform backends
media-foundation = ["windows"]
vaapi = ["libva-sys"]
videotoolbox = ["core-video", "video-toolbox"]
vulkan-video = ["ash"]

# Enable native backend for current platform
native-backend = []

# All backends (for testing)
all-backends = ["media-foundation", "vaapi", "videotoolbox", "vulkan-video"]

# Software fallback
software = ["ffmpeg-sys"]
```

### Platform-Specific Dependencies

```toml
[target.'cfg(windows)'.dependencies]
windows = { version = "0.58", features = ["Win32_Media_MediaFoundation", "Win32_Graphics_Direct3D11", "Win32_Graphics_Direct3D12", "Win32_Graphics_Dxgi"] }

[target.'cfg(target_os = "linux")'.dependencies]
libva-sys = "0.5"
drm-sys = "0.1"

[target.'cfg(target_os = "macos")'.dependencies]
core-foundation = "0.9"
core-video-sys = "0.1"
metal = "0.27"
objc = "0.2"
```

## File Naming Conventions

- **Modules:** `snake_case` (e.g., `backend_selector.rs`)
- **Types:** `PascalCase` (e.g., `VideoDecoder`)
- **Functions:** `snake_case` (e.g., `decode_frame()`)
- **Constants:** `SCREAMING_SNAKE_CASE` (e.g., `DEFAULT_POOL_SIZE`)
- **Platform-specific:** `platform_feature.rs` (e.g., `d3d12_interop.rs`)

## Documentation Standards

- **Public API:** Full rustdoc with examples
- **Internal modules:** Brief module-level docs
- **Traits:** Document required behavior and invariants
- **Platform code:** Note platform-specific requirements
- **Safety:** Document all `unsafe` blocks with safety requirements

## Conditional Compilation Strategy

```rust
// Platform selection
#[cfg(target_os = "windows")]
pub use backends::media_foundation;

#[cfg(target_os = "linux")]
pub use backends::vaapi;

#[cfg(target_os = "macos")]
pub use backends::videotoolbox;

// Feature-based
#[cfg(feature = "vulkan-video")]
pub use backends::vulkan_video;

// Backend availability
pub fn available_backends() -> Vec<BackendType> {
    let mut backends = Vec::new();
    
    #[cfg(target_os = "windows")]
    backends.push(BackendType::MediaFoundation);
    
    #[cfg(target_os = "linux")]
    backends.push(BackendType::VaApi);
    
    // ... etc
    
    backends
}
```

## Maintenance Guidelines

### Adding New Backend
1. Create new directory in `backends/`
2. Implement `DecoderBackend` trait
3. Add texture interop module
4. Update backend selector
5. Add feature flag in Cargo.toml
6. Add platform-specific tests
7. Update documentation

### Adding New Codec
1. Add variant to `Codec` enum
2. Create parser in `codec/` if needed
3. Update each backend's capability detection
4. Add codec-specific tests
5. Update documentation

### Refactoring Checklist
- Maintain backward compatibility in public API
- Update all backend implementations
- Run full test suite on all platforms
- Update documentation
- Check performance benchmarks