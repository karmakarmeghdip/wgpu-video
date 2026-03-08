# wgpu-video Documentation

Welcome to the wgpu-video library documentation. This library provides cross-platform GPU-accelerated video decoding with seamless integration into the wgpu rendering pipeline.

## Overview

wgpu-video is a Rust library that abstracts platform-specific hardware video decoding APIs and provides decoded frames as wgpu textures. The library automatically selects the best decoding backend based on the platform and wgpu backend being used.

## Key Features

- **Cross-platform support**: Windows (MediaFoundation), Linux (VA-API), macOS (VideoToolbox), and platform-agnostic (Vulkan Video)
- **wgpu integration**: Seamless integration with wgpu rendering pipeline
- **Zero-copy where possible**: Direct GPU texture output without CPU roundtrips
- **Backend-aware**: Automatically selects optimal decoder based on wgpu backend (Vulkan, DX12, Metal, etc.)
- **Extensible architecture**: Easy to add new platform-specific or codec-specific decoders

## Documentation Structure

- **[Architecture Overview](./ARCHITECTURE.md)** - High-level system design and component interaction
- **[Codebase Structure](./CODEBASE_STRUCTURE.md)** - Detailed module organization and file structure
- **[Platform Backends](./PLATFORM_BACKENDS.md)** - Platform-specific decoder implementations
- **[API Design](./API_DESIGN.md)** - Public API surface and usage patterns
- **[Development Guide](./DEVELOPMENT_GUIDE.md)** - How to build, test, and contribute
- **[Integration Guide](./INTEGRATION_GUIDE.md)** - How to integrate wgpu-video into your application

## Quick Start

```rust
use wgpu_video::{VideoDecoder, VideoDecoderBuilder, DecoderFormat};

// Create decoder from wgpu device
let decoder = VideoDecoderBuilder::new()
    .with_wgpu_device(&device)
    .with_format(DecoderFormat::H264)
    .build()?;

// Decode video frames
let texture = decoder.decode_frame(&encoded_data)?;

// Use texture in your render pipeline
render_pass.set_bind_group(0, &texture_bind_group, &[]);
```

## Supported Platforms

| Platform | Backend | API | Status |
|----------|---------|-----|--------|
| Windows | DX12 | MediaFoundation | Planned |
| Windows | Vulkan | Vulkan Video / MediaFoundation | Planned |
| Linux | Vulkan | VA-API / Vulkan Video | Planned |
| macOS | Metal | VideoToolbox | Planned |
| Cross-platform | Vulkan | Vulkan Video | Planned |

## Supported Codecs

- H.264 / AVC
- H.265 / HEVC
- VP9
- AV1

## Design Principles

1. **Platform-native first**: Use native hardware acceleration APIs where available
2. **Fallback strategy**: Graceful degradation to cross-platform or software decoders
3. **Zero-copy optimization**: Minimize CPU-GPU data transfers
4. **Type safety**: Leverage Rust's type system for API safety
5. **Async-ready**: Support for async/await patterns
6. **Minimal dependencies**: Keep the dependency tree lean

## License

[Your chosen license]

## Contributing

See [DEVELOPMENT_GUIDE.md](./DEVELOPMENT_GUIDE.md) for contribution guidelines.
