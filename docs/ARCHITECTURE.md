# Architecture Overview

## High-Level Design

wgpu-video is designed as a layered architecture that abstracts platform-specific video decoding APIs while providing seamless integration with wgpu's rendering pipeline.

```
┌─────────────────────────────────────────────────────────────┐
│                    Application Layer                         │
│                  (User's wgpu application)                   │
└─────────────────────────┬───────────────────────────────────┘
                          │
                          ↓
┌─────────────────────────────────────────────────────────────┐
│                      Public API Layer                        │
│  VideoDecoder, VideoDecoderBuilder, DecoderCapabilities     │
└─────────────────────────┬───────────────────────────────────┘
                          │
                          ↓
┌─────────────────────────────────────────────────────────────┐
│                    Core Abstraction Layer                    │
│    Trait definitions, common types, backend selection       │
└─────────────────────────┬───────────────────────────────────┘
                          │
          ┌───────────────┼───────────────┐
          ↓               ↓               ↓
┌──────────────┐  ┌──────────────┐  ┌──────────────┐
│   Platform   │  │   Platform   │  │Cross-platform│
│   Backend    │  │   Backend    │  │   Backend    │
│  (Windows)   │  │   (Linux)    │  │ (Vk-Video)   │
└──────┬───────┘  └──────┬───────┘  └──────┬───────┘
       │                 │                 │
       ↓                 ↓                 ↓
┌──────────────┐  ┌──────────────┐  ┌──────────────┐
│ Media        │  │   VA-API     │  │   Vulkan     │
│ Foundation   │  │   libva      │  │   Video      │
└──────────────┘  └──────────────┘  └──────────────┘
```

## Core Components

### 1. Public API Layer

**Responsibilities:**
- Provide user-facing API for decoder creation and frame decoding
- Handle builder pattern for configuration
- Expose capability querying
- Manage decoder lifecycle

**Key Types:**
- `VideoDecoder`: Main decoder handle
- `VideoDecoderBuilder`: Fluent API for decoder configuration
- `DecodedFrame`: Wrapper around wgpu texture with metadata
- `DecoderCapabilities`: Query support for codecs and formats

### 2. Core Abstraction Layer

**Responsibilities:**
- Define traits for backend implementations
- Implement backend selection logic
- Handle wgpu backend detection
- Provide common utilities and types
- Coordinate texture creation and format conversion

**Key Traits:**
- `DecoderBackend`: Core trait all backends implement
- `TextureConverter`: Handle format conversion if needed
- `CapabilityProvider`: Query backend capabilities

**Key Modules:**
- `backend_selector`: Logic to choose best backend
- `wgpu_interop`: wgpu integration utilities
- `common_types`: Shared types across backends
- `error`: Error types and handling

### 3. Platform Backend Layer

**Responsibilities:**
- Implement platform-specific decoding
- Handle native API initialization and cleanup
- Manage GPU memory and texture interop
- Provide format negotiation

**Windows Backend (MediaFoundation):**
- Use IMFTransform for hardware decoding
- D3D11/D3D12 texture sharing with wgpu
- DXGI surface handling

**Linux Backend (VA-API):**
- Use libva for hardware acceleration
- DRM/DMA-BUF for zero-copy texture sharing
- Vulkan/OpenGL interop

**macOS Backend (VideoToolbox):**
- Use VideoToolbox framework
- IOSurface for texture sharing with Metal
- CVPixelBuffer handling

**Cross-platform Backend (Vulkan Video):**
- Pure Vulkan Video extension support
- Works on any platform with Vulkan Video drivers
- Direct VkImage output

### 4. Texture Interop Layer

**Responsibilities:**
- Handle texture sharing between decoder and wgpu
- Manage synchronization primitives
- Format conversion when necessary
- Handle color space conversions (YUV to RGB)

**Strategies:**
- Zero-copy: Direct texture sharing (preferred)
- Copy: Fallback when sharing is not possible
- Compute shader conversion: For format mismatches

## Data Flow

### Initialization Flow

```
1. User creates VideoDecoderBuilder
   ↓
2. Builder collects configuration (wgpu device, format, codec)
   ↓
3. Backend selector queries wgpu device for backend type
   ↓
4. Backend selector chooses optimal decoder backend
   ↓
5. Selected backend initializes native decoder
   ↓
6. Backend negotiates output format with wgpu
   ↓
7. VideoDecoder handle returned to user
```

### Decoding Flow

```
1. User calls decoder.decode_frame(data)
   ↓
2. Data passed to backend implementation
   ↓
3. Backend submits to native decoder API
   ↓
4. Native API decodes to GPU texture
   ↓
5. Backend wraps native texture handle
   ↓
6. Texture interop creates/imports wgpu texture
   ↓
7. DecodedFrame returned with wgpu::Texture
   ↓
8. User binds texture in render pass
```

## Backend Selection Logic

The backend selector uses a priority-based system:

```
Priority 1: Platform-native + wgpu backend match
  - Windows + DX12 → MediaFoundation (D3D12)
  - Linux + Vulkan → VA-API (Vulkan interop)
  - macOS + Metal → VideoToolbox

Priority 2: Cross-platform with wgpu backend match
  - Any platform + Vulkan → Vulkan Video (if available)

Priority 3: Platform-native with conversion
  - Windows + Vulkan → MediaFoundation (copy to Vulkan)
  - Linux + OpenGL → VA-API (with interop)

Priority 4: Software fallback
  - Any platform → Software decoder (libavcodec)
```

### Selection Algorithm

```rust
fn select_backend(wgpu_device: &Device, codec: Codec) -> Result<BackendType> {
    let wgpu_backend = detect_wgpu_backend(wgpu_device);
    let platform = detect_platform();
    
    // Check for optimal path
    if let Some(backend) = check_native_optimal(platform, wgpu_backend, codec) {
        return Ok(backend);
    }
    
    // Check for Vulkan Video (cross-platform)
    if wgpu_backend == BackendType::Vulkan && vulkan_video_available() {
        return Ok(BackendType::VulkanVideo);
    }
    
    // Check for platform native with conversion
    if let Some(backend) = check_native_with_conversion(platform, codec) {
        return Ok(backend);
    }
    
    // Fallback to software
    Ok(BackendType::Software)
}
```

## Memory Management

### Texture Lifecycle

1. **Decoder owns the decode surface pool**: Backend maintains a pool of decode surfaces
2. **Ref-counted wgpu textures**: DecodedFrame holds a reference to the wgpu texture
3. **Automatic cleanup**: When DecodedFrame is dropped, texture returns to pool
4. **Pool recycling**: Decode surfaces are reused to minimize allocation overhead

### Synchronization

- **Fences/Semaphores**: For GPU-GPU synchronization between decoder and wgpu
- **Timeline semaphores**: Preferred for Vulkan backend
- **Shared fences**: For D3D12 interop
- **MTLSharedEvent**: For Metal interop

## Error Handling Strategy

### Error Types

```
DecoderError
├── InitializationError
│   ├── UnsupportedPlatform
│   ├── UnsupportedCodec
│   ├── BackendNotAvailable
│   └── InvalidConfiguration
├── DecodingError
│   ├── CorruptedData
│   ├── HardwareError
│   ├── OutOfMemory
│   └── InvalidState
└── InteropError
    ├── TextureCreationFailed
    ├── SynchronizationFailed
    └── FormatConversionFailed
```

### Recovery Strategy

- Transient errors: Retry with exponential backoff
- Hardware errors: Attempt fallback to software decoder
- Fatal errors: Propagate to user with detailed context

## Threading Model

### Async Design

```
┌──────────────┐
│  User Thread │ (calls decode_frame)
└──────┬───────┘
       │ (async)
       ↓
┌──────────────┐
│ Decode Thread│ (backend-specific work)
└──────┬───────┘
       │
       ↓
┌──────────────┐
│  GPU Work    │ (hardware decoder + texture creation)
└──────────────┘
```

- **Async API**: All decoding operations are async
- **Internal thread pool**: Managed by the library (optional)
- **Callback model**: Alternative for non-async users
- **Thread-safe**: VideoDecoder is Send + Sync where possible

## Extensibility Points

### Adding New Backends

1. Implement `DecoderBackend` trait
2. Add backend variant to `BackendType` enum
3. Update backend selector logic
4. Add platform-specific dependencies in Cargo.toml
5. Implement texture interop for the backend

### Adding New Codecs

1. Add codec variant to `Codec` enum
2. Update capability queries for each backend
3. Implement codec-specific initialization in backends
4. Add codec-specific tests

### Adding New Features

- **HDR Support**: Extend format types, add color space metadata
- **Hardware encoding**: Mirror architecture for encoder
- **Post-processing**: Add optional filter chain before texture output
- **Multi-stream**: Support multiple concurrent decoders

## Performance Considerations

### Optimization Strategies

1. **Zero-copy paths**: Prioritize direct GPU memory sharing
2. **Texture pooling**: Reuse decode surfaces to avoid allocation overhead
3. **Async decoding**: Overlap decode operations with rendering
4. **Batch processing**: Queue multiple frames when possible
5. **Format matching**: Negotiate formats that match both decoder and wgpu capabilities

### Profiling Points

- Backend selection time
- Decoder initialization time
- Frame decode latency
- Texture creation/import time
- Synchronization overhead
- Memory usage (decode surface pool size)

## Testing Strategy

### Unit Tests
- Backend selection logic
- Format negotiation
- Error handling paths
- Capability queries

### Integration Tests
- End-to-end decoding with real video files
- wgpu texture creation and usage
- Multi-threading scenarios
- Error recovery

### Platform-Specific Tests
- Run on CI for each supported platform
- Test hardware availability detection
- Validate zero-copy paths
- Performance benchmarks

## Dependencies

### Core Dependencies
- `wgpu`: GPU abstraction and texture handling
- `raw-window-handle`: For platform-specific interop

### Platform Dependencies
- **Windows**: `windows-rs` for MediaFoundation and D3D
- **Linux**: `libva-sys` for VA-API bindings
- **macOS**: `core-foundation`, `core-video`, `video-toolbox`
- **Vulkan Video**: `ash` for Vulkan bindings

### Optional Dependencies
- `ffmpeg-sys`: Software decoder fallback
- `tokio`/`async-std`: Async runtime support
- `tracing`: Structured logging