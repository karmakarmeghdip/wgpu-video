# API Design and Usage Guide

This document describes the public API surface of wgpu-video and provides comprehensive usage examples.

## Table of Contents

- [Design Philosophy](#design-philosophy)
- [Core API Types](#core-api-types)
- [Basic Usage](#basic-usage)
- [Advanced Usage](#advanced-usage)
- [Error Handling](#error-handling)
- [API Reference](#api-reference)
- [Migration Guide](#migration-guide)

---

## Design Philosophy

### Principles

1. **Type Safety**: Leverage Rust's type system to prevent misuse
2. **Ergonomic**: Simple things should be simple
3. **Flexible**: Advanced use cases should be possible
4. **Zero-cost abstractions**: No runtime overhead for unused features
5. **Async-first**: Non-blocking operations by default
6. **Fail-fast**: Validate early, propagate errors clearly

### API Goals

- **Single decoder type**: `VideoDecoder` handles all platforms and codecs
- **Builder pattern**: Configure decoders with fluent API
- **Minimal dependencies**: Only require wgpu device
- **Safe defaults**: Automatic backend selection and configuration
- **Escape hatches**: Allow manual backend selection when needed

---

## Core API Types

### VideoDecoder

The main decoder handle that owns all decoding resources.

```rust
pub struct VideoDecoder {
    // Internal implementation hidden
}

impl VideoDecoder {
    /// Decode a single frame from encoded data
    pub fn decode_frame(&mut self, data: &[u8]) -> Result<DecodedFrame>;
    
    /// Decode frame asynchronously
    pub async fn decode_frame_async(&mut self, data: &[u8]) -> Result<DecodedFrame>;
    
    /// Flush buffered frames
    pub fn flush(&mut self) -> Result<Vec<DecodedFrame>>;
    
    /// Reset decoder state (for seeking)
    pub fn reset(&mut self) -> Result<()>;
    
    /// Get decoder information
    pub fn info(&self) -> &DecoderInfo;
    
    /// Query if codec is supported
    pub fn is_codec_supported(codec: Codec) -> bool;
}
```

### VideoDecoderBuilder

Fluent builder for configuring decoders.

```rust
pub struct VideoDecoderBuilder {
    // Internal configuration
}

impl VideoDecoderBuilder {
    /// Create a new builder
    pub fn new() -> Self;
    
    /// Set the wgpu device (required)
    pub fn with_wgpu_device(self, device: &Device) -> Self;
    
    /// Set the codec (required)
    pub fn with_codec(self, codec: Codec) -> Self;
    
    /// Set video resolution
    pub fn with_resolution(self, width: u32, height: u32) -> Self;
    
    /// Set codec extra data (SPS/PPS for H.264, etc.)
    pub fn with_extra_data(self, data: Vec<u8>) -> Self;
    
    /// Set preferred pixel format
    pub fn with_pixel_format(self, format: PixelFormat) -> Self;
    
    /// Set color space
    pub fn with_color_space(self, color_space: ColorSpace) -> Self;
    
    /// Force specific backend (overrides automatic selection)
    pub fn with_backend(self, backend: BackendType) -> Self;
    
    /// Enable/disable hardware acceleration
    pub fn hardware_accelerated(self, enabled: bool) -> Self;
    
    /// Set texture pool size (default: 4)
    pub fn with_pool_size(self, size: usize) -> Self;
    
    /// Build the decoder
    pub fn build(self) -> Result<VideoDecoder>;
}
```

### DecodedFrame

Container for a decoded video frame with metadata.

```rust
pub struct DecodedFrame {
    // Internal fields
}

impl DecodedFrame {
    /// Get the wgpu texture
    pub fn texture(&self) -> &wgpu::Texture;
    
    /// Create a texture view with default settings
    pub fn create_view(&self) -> wgpu::TextureView;
    
    /// Create a texture view with custom descriptor
    pub fn create_view_with_descriptor(&self, desc: &TextureViewDescriptor) -> wgpu::TextureView;
    
    /// Get frame metadata
    pub fn metadata(&self) -> &FrameMetadata;
    
    /// Get presentation timestamp (if available)
    pub fn timestamp(&self) -> Option<Duration>;
    
    /// Get frame number
    pub fn frame_number(&self) -> u64;
    
    /// Get pixel format
    pub fn pixel_format(&self) -> PixelFormat;
    
    /// Get color space information
    pub fn color_space(&self) -> ColorSpace;
}
```

### Supporting Types

```rust
/// Video codec types
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    H264,
    H265,
    VP9,
    AV1,
}

/// Pixel format for decoded frames
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    /// 8-bit YUV 4:2:0 (NV12)
    Yuv420P8,
    /// 10-bit YUV 4:2:0 (P010)
    Yuv420P10,
    /// 8-bit RGBA
    Rgba8,
    /// 8-bit BGRA
    Bgra8,
}

/// Color space information
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorSpace {
    /// BT.601 (SD)
    Bt601,
    /// BT.709 (HD)
    Bt709,
    /// BT.2020 (UHD/HDR)
    Bt2020,
}

/// Backend type
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendType {
    MediaFoundation,
    VaApi,
    VideoToolbox,
    VulkanVideo,
    Software,
}

/// Frame metadata
#[derive(Debug, Clone)]
pub struct FrameMetadata {
    pub width: u32,
    pub height: u32,
    pub is_keyframe: bool,
    pub pixel_format: PixelFormat,
    pub color_space: ColorSpace,
}

/// Decoder information
#[derive(Debug, Clone)]
pub struct DecoderInfo {
    pub codec: Codec,
    pub backend: BackendType,
    pub hardware_accelerated: bool,
    pub pixel_format: PixelFormat,
    pub resolution: (u32, u32),
}

/// Decoder capabilities
pub struct DecoderCapabilities {
    pub supported_codecs: Vec<Codec>,
    pub supported_pixel_formats: Vec<PixelFormat>,
    pub max_resolution: (u32, u32),
    pub backend: BackendType,
    pub hardware_accelerated: bool,
}
```

---

## Basic Usage

### Example 1: Simple Video Decoding

```rust
use wgpu_video::{VideoDecoderBuilder, Codec};

// Assume you have a wgpu device
let device = create_wgpu_device();

// Create decoder
let mut decoder = VideoDecoderBuilder::new()
    .with_wgpu_device(&device)
    .with_codec(Codec::H264)
    .with_resolution(1920, 1080)
    .build()?;

// Decode frames
let encoded_data = read_encoded_frame();
let decoded_frame = decoder.decode_frame(&encoded_data)?;

// Use the texture in rendering
let texture = decoded_frame.texture();
let view = decoded_frame.create_view();

// Bind to render pass
render_pass.set_bind_group(0, &bind_group_with_texture, &[]);
```

### Example 2: H.264 with Extra Data

```rust
use wgpu_video::{VideoDecoderBuilder, Codec};

// H.264 typically needs SPS/PPS
let sps_pps = extract_parameter_sets(&video_header);

let mut decoder = VideoDecoderBuilder::new()
    .with_wgpu_device(&device)
    .with_codec(Codec::H264)
    .with_resolution(1920, 1080)
    .with_extra_data(sps_pps)
    .build()?;

// Decode NAL units
for nal_unit in nal_units {
    let frame = decoder.decode_frame(&nal_unit)?;
    render_frame(&frame);
}
```

### Example 3: Query Capabilities

```rust
use wgpu_video::{VideoDecoder, Codec, DecoderCapabilities};

// Check if H.265 is supported
if VideoDecoder::is_codec_supported(Codec::H265) {
    println!("H.265 is supported!");
}

// Get detailed capabilities
let capabilities = DecoderCapabilities::query(&device)?;
println!("Backend: {:?}", capabilities.backend);
println!("Hardware accelerated: {}", capabilities.hardware_accelerated);
println!("Supported codecs: {:?}", capabilities.supported_codecs);
println!("Max resolution: {}x{}", 
    capabilities.max_resolution.0,
    capabilities.max_resolution.1
);
```

### Example 4: Async Decoding

```rust
use wgpu_video::{VideoDecoderBuilder, Codec};
use tokio; // or any async runtime

#[tokio::main]
async fn main() -> Result<()> {
    let device = create_wgpu_device();
    
    let mut decoder = VideoDecoderBuilder::new()
        .with_wgpu_device(&device)
        .with_codec(Codec::H264)
        .build()?;
    
    // Decode asynchronously
    let encoded_data = read_encoded_frame();
    let frame = decoder.decode_frame_async(&encoded_data).await?;
    
    render_frame(&frame);
    
    Ok(())
}
```

---

## Advanced Usage

### Example 5: Manual Backend Selection

```rust
use wgpu_video::{VideoDecoderBuilder, Codec, BackendType};

// Force Vulkan Video backend
let mut decoder = VideoDecoderBuilder::new()
    .with_wgpu_device(&device)
    .with_codec(Codec::H264)
    .with_backend(BackendType::VulkanVideo)
    .build()?;

// Check what backend was actually used
let info = decoder.info();
println!("Using backend: {:?}", info.backend);
```

### Example 6: HDR Content (10-bit)

```rust
use wgpu_video::{VideoDecoderBuilder, Codec, PixelFormat, ColorSpace};

let mut decoder = VideoDecoderBuilder::new()
    .with_wgpu_device(&device)
    .with_codec(Codec::H265)
    .with_resolution(3840, 2160)
    .with_pixel_format(PixelFormat::Yuv420P10)
    .with_color_space(ColorSpace::Bt2020)
    .build()?;

let frame = decoder.decode_frame(&hdr_data)?;

// Frame will be 10-bit in BT.2020 color space
assert_eq!(frame.pixel_format(), PixelFormat::Yuv420P10);
assert_eq!(frame.color_space(), ColorSpace::Bt2020);
```

### Example 7: Flushing and Seeking

```rust
use wgpu_video::{VideoDecoderBuilder, Codec};

let mut decoder = VideoDecoderBuilder::new()
    .with_wgpu_device(&device)
    .with_codec(Codec::H264)
    .build()?;

// Decode some frames
for data in frame_data {
    decoder.decode_frame(&data)?;
}

// Flush buffered frames (important for B-frames)
let buffered_frames = decoder.flush()?;
for frame in buffered_frames {
    render_frame(&frame);
}

// Seek to new position - reset decoder
decoder.reset()?;

// Start decoding from keyframe
let keyframe_data = read_keyframe();
let frame = decoder.decode_frame(&keyframe_data)?;
```

### Example 8: Multiple Concurrent Decoders

```rust
use wgpu_video::{VideoDecoderBuilder, Codec};
use std::collections::HashMap;

struct MultiStreamDecoder {
    device: wgpu::Device,
    decoders: HashMap<u32, VideoDecoder>,
}

impl MultiStreamDecoder {
    fn new(device: wgpu::Device) -> Self {
        Self {
            device,
            decoders: HashMap::new(),
        }
    }
    
    fn get_or_create_decoder(&mut self, stream_id: u32, codec: Codec) -> Result<&mut VideoDecoder> {
        if !self.decoders.contains_key(&stream_id) {
            let decoder = VideoDecoderBuilder::new()
                .with_wgpu_device(&self.device)
                .with_codec(codec)
                .build()?;
            self.decoders.insert(stream_id, decoder);
        }
        Ok(self.decoders.get_mut(&stream_id).unwrap())
    }
    
    fn decode_frame(&mut self, stream_id: u32, codec: Codec, data: &[u8]) -> Result<DecodedFrame> {
        let decoder = self.get_or_create_decoder(stream_id, codec)?;
        decoder.decode_frame(data)
    }
}

// Usage
let mut multi_decoder = MultiStreamDecoder::new(device);

// Decode from multiple streams
let frame1 = multi_decoder.decode_frame(0, Codec::H264, &stream1_data)?;
let frame2 = multi_decoder.decode_frame(1, Codec::H265, &stream2_data)?;
```

### Example 9: Custom Texture Pool Size

```rust
use wgpu_video::{VideoDecoderBuilder, Codec};

// Increase pool size for better performance with async rendering
let mut decoder = VideoDecoderBuilder::new()
    .with_wgpu_device(&device)
    .with_codec(Codec::H264)
    .with_pool_size(8) // More buffered frames
    .build()?;

// Now you can hold more frames simultaneously
let mut frames = Vec::new();
for data in frame_batch {
    frames.push(decoder.decode_frame(&data)?);
}

// Render all frames
for frame in frames {
    render_frame(&frame);
}
// Frames automatically return to pool when dropped
```

### Example 10: Integration with Media Container

```rust
use wgpu_video::{VideoDecoderBuilder, Codec};
use mp4; // Example container parser

struct VideoPlayer {
    decoder: VideoDecoder,
    mp4_reader: mp4::Mp4Reader<File>,
}

impl VideoPlayer {
    fn new(device: &wgpu::Device, video_path: &str) -> Result<Self> {
        let file = File::open(video_path)?;
        let mp4_reader = mp4::Mp4Reader::read_header(file, file_size)?;
        
        // Get video track
        let track = mp4_reader.tracks()
            .values()
            .find(|t| t.track_type() == mp4::TrackType::Video)
            .ok_or(Error::NoVideoTrack)?;
        
        // Extract codec info
        let codec = match track.media_type()? {
            mp4::MediaType::H264 => Codec::H264,
            mp4::MediaType::H265 => Codec::H265,
            _ => return Err(Error::UnsupportedCodec),
        };
        
        // Get extra data (SPS/PPS)
        let extra_data = track.extra_data()?.to_vec();
        
        // Create decoder
        let decoder = VideoDecoderBuilder::new()
            .with_wgpu_device(device)
            .with_codec(codec)
            .with_resolution(track.width(), track.height())
            .with_extra_data(extra_data)
            .build()?;
        
        Ok(Self {
            decoder,
            mp4_reader,
        })
    }
    
    fn decode_next_frame(&mut self) -> Result<Option<DecodedFrame>> {
        // Read next sample from MP4
        let sample = match self.mp4_reader.read_sample(track_id, sample_id)? {
            Some(s) => s,
            None => return Ok(None), // End of stream
        };
        
        // Decode
        let frame = self.decoder.decode_frame(&sample.bytes)?;
        Ok(Some(frame))
    }
}
```

---

## Error Handling

### Error Types

```rust
#[derive(Debug, thiserror::Error)]
pub enum DecoderError {
    #[error("Failed to initialize decoder: {0}")]
    InitializationFailed(String),
    
    #[error("Codec {0:?} is not supported on this platform")]
    UnsupportedCodec(Codec),
    
    #[error("Backend {0:?} is not available")]
    BackendNotAvailable(BackendType),
    
    #[error("Invalid configuration: {0}")]
    InvalidConfiguration(String),
    
    #[error("Decoding failed: {0}")]
    DecodingFailed(String),
    
    #[error("Corrupted or invalid data")]
    CorruptedData,
    
    #[error("Hardware decoder error: {0}")]
    HardwareError(String),
    
    #[error("Out of memory")]
    OutOfMemory,
    
    #[error("Texture creation failed: {0}")]
    TextureCreationFailed(String),
    
    #[error("Synchronization failed: {0}")]
    SynchronizationFailed(String),
    
    #[error("Invalid decoder state: {0}")]
    InvalidState(String),
    
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, DecoderError>;
```

### Error Handling Patterns

```rust
use wgpu_video::{VideoDecoderBuilder, Codec, DecoderError};

// Pattern 1: Match on specific errors
match VideoDecoderBuilder::new()
    .with_wgpu_device(&device)
    .with_codec(Codec::H264)
    .build()
{
    Ok(decoder) => { /* use decoder */ },
    Err(DecoderError::UnsupportedCodec(codec)) => {
        eprintln!("Codec {:?} not supported, trying fallback", codec);
        // Try different codec or software decoder
    },
    Err(DecoderError::BackendNotAvailable(backend)) => {
        eprintln!("Backend {:?} not available", backend);
        // Try different backend
    },
    Err(e) => {
        eprintln!("Failed to create decoder: {}", e);
        return Err(e);
    }
}

// Pattern 2: Retry with software fallback
fn create_decoder_with_fallback(device: &wgpu::Device, codec: Codec) -> Result<VideoDecoder> {
    // Try hardware first
    match VideoDecoderBuilder::new()
        .with_wgpu_device(device)
        .with_codec(codec)
        .hardware_accelerated(true)
        .build()
    {
        Ok(decoder) => Ok(decoder),
        Err(DecoderError::HardwareError(_)) => {
            // Fallback to software
            VideoDecoderBuilder::new()
                .with_wgpu_device(device)
                .with_codec(codec)
                .hardware_accelerated(false)
                .build()
        },
        Err(e) => Err(e),
    }
}

// Pattern 3: Graceful degradation
fn decode_frame_safe(decoder: &mut VideoDecoder, data: &[u8]) -> Option<DecodedFrame> {
    match decoder.decode_frame(data) {
        Ok(frame) => Some(frame),
        Err(DecoderError::CorruptedData) => {
            eprintln!("Skipping corrupted frame");
            None // Skip frame
        },
        Err(DecoderError::OutOfMemory) => {
            eprintln!("Out of memory, flushing decoder");
            let _ = decoder.flush();
            None
        },
        Err(e) => {
            eprintln!("Decoding error: {}", e);
            None
        }
    }
}
```

---

## API Reference

### VideoDecoder

#### Methods

##### `decode_frame`
```rust
pub fn decode_frame(&mut self, data: &[u8]) -> Result<DecodedFrame>
```

Decode a single frame from encoded data.

**Parameters:**
- `data`: Encoded frame data (NAL unit, OBU, etc.)

**Returns:**
- `Result<DecodedFrame>`: Decoded frame with wgpu texture

**Errors:**
- `DecoderError::CorruptedData`: Invalid or corrupted input
- `DecoderError::DecodingFailed`: Hardware decoder failure
- `DecoderError::OutOfMemory`: Insufficient GPU memory

**Example:**
```rust
let frame = decoder.decode_frame(&encoded_data)?;
```

---

##### `decode_frame_async`
```rust
pub async fn decode_frame_async(&mut self, data: &[u8]) -> Result<DecodedFrame>
```

Decode frame asynchronously without blocking.

**Parameters:**
- `data`: Encoded frame data

**Returns:**
- `Result<DecodedFrame>`: Future that resolves to decoded frame

**Example:**
```rust
let frame = decoder.decode_frame_async(&data).await?;
```

---

##### `flush`
```rust
pub fn flush(&mut self) -> Result<Vec<DecodedFrame>>
```

Flush any buffered frames from the decoder. Important for codecs with frame reordering (B-frames).

**Returns:**
- `Result<Vec<DecodedFrame>>`: All buffered frames in display order

**Example:**
```rust
let buffered_frames = decoder.flush()?;
```

---

##### `reset`
```rust
pub fn reset(&mut self) -> Result<()>
```

Reset decoder state. Use this after seeking to a new position.

**Example:**
```rust
decoder.reset()?;
let keyframe = decoder.decode_frame(&keyframe_data)?;
```

---

##### `info`
```rust
pub fn info(&self) -> &DecoderInfo
```

Get information about the decoder configuration.

**Returns:**
- `&DecoderInfo`: Decoder metadata

**Example:**
```rust
let info = decoder.info();
println!("Backend: {:?}", info.backend);
```

---

##### `is_codec_supported`
```rust
pub fn is_codec_supported(codec: Codec) -> bool
```

Static method to check if a codec is supported on the current platform.

**Parameters:**
- `codec`: Codec to check

**Returns:**
- `bool`: True if supported

**Example:**
```rust
if VideoDecoder::is_codec_supported(Codec::AV1) {
    // Use AV1
}
```

---

### VideoDecoderBuilder

All builder methods return `Self` for chaining except `build()`.

#### Required Methods

- `with_wgpu_device(&Device)`: Set wgpu device (required)
- `with_codec(Codec)`: Set codec type (required)

#### Optional Methods

- `with_resolution(u32, u32)`: Set video dimensions
- `with_extra_data(Vec<u8>)`: Set codec-specific data
- `with_pixel_format(PixelFormat)`: Preferred output format
- `with_color_space(ColorSpace)`: Color space information
- `with_backend(BackendType)`: Force specific backend
- `hardware_accelerated(bool)`: Enable/disable hardware acceleration
- `with_pool_size(usize)`: Set texture pool size

#### Build Method

```rust
pub fn build(self) -> Result<VideoDecoder>
```

Construct the decoder with configured parameters.

---

### DecodedFrame

#### Methods

##### `texture`
```rust
pub fn texture(&self) -> &wgpu::Texture
```

Get the underlying wgpu texture.

---

##### `create_view`
```rust
pub fn create_view(&self) -> wgpu::TextureView
```

Create a texture view with default settings.

---

##### `metadata`
```rust
pub fn metadata(&self) -> &FrameMetadata
```

Get frame metadata (dimensions, format, etc.).

---

##### `timestamp`
```rust
pub fn timestamp(&self) -> Option<Duration>
```

Get presentation timestamp if available.

---

### DecoderCapabilities

```rust
pub fn query(device: &wgpu::Device) -> Result<DecoderCapabilities>
```

Query decoder capabilities for the given device.

**Example:**
```rust
let caps = DecoderCapabilities::query(&device)?;
if caps.hardware_accelerated {
    println!("Hardware acceleration available!");
}
```

---

## Migration Guide

### From FFmpeg

```rust
// FFmpeg-style
let mut codec_context = avcodec_alloc_context3(codec);
avcodec_open2(codec_context, codec, options);
avcodec_send_packet(codec_context, packet);
avcodec_receive_frame(codec_context, frame);

// wgpu-video
let mut decoder = VideoDecoderBuilder::new()
    .with_wgpu_device(&device)
    .with_codec(Codec::H264)
    .build()?;
let frame = decoder.decode_frame(&packet_data)?;
```

### From Platform APIs

```rust
// Windows MediaFoundation (raw)
let transform = create_mf_transform();
transform.ProcessInput(0, sample);
transform.ProcessOutput(0, output_sample);

// wgpu-video (automatic backend selection)
let mut decoder = VideoDecoderBuilder::new()
    .with_wgpu_device(&device)
    .with_codec(Codec::H264)
    .build()?;
let frame = decoder.decode_frame(&data)?;
```

---

## Best Practices

### 1. Resource Management

```rust
// Good: Decoder is dropped automatically
{
    let mut decoder = create_decoder()?;
    decode_video(&mut decoder)?;
} // decoder cleaned up here

// Good: Frames return to pool when dropped
for data in frames {
    let frame = decoder.decode_frame(&data)?;
    render_frame(&frame);
    // frame dropped, returns to pool
}
```

### 2. Error Handling

```rust
// Good: Handle specific errors
match decoder.decode_frame(&data) {
    Ok(frame) => process(frame),
    Err(DecoderError::CorruptedData) => skip_frame(),
    Err(e) => log_and_continue(e),
}

// Avoid: Ignoring errors
let _ = decoder.decode_frame(&data); // DON'T DO THIS
```

### 3. Performance

```rust
// Good: Reuse decoder
let mut decoder = create_decoder()?;
for frame_data in video_stream {
    decoder.decode_frame(&frame_data)?;
}

// Avoid: Creating decoder per frame
for frame_data in video_stream {
    let decoder = create_decoder()?; // WASTEFUL
    decoder.decode_frame(&frame_data)?;
}
```

### 4. Async Operations

```rust
// Good: Parallel decoding
let mut tasks = Vec::new();
for data in frame_batch {
    tasks.push(decoder.decode_frame_async(&data));
}
let frames = futures::future::join_all(tasks).await;

// Or: Streaming
let mut stream = decoder.decode_stream(video_source);
while let Some(frame) = stream.next().await {
    render_frame(frame?);
}
```

---

## Thread Safety

`VideoDecoder` is `Send` but not `Sync`. Each decoder should be used from a single thread.

```rust
// Good: Decoder per thread
std::thread::spawn(move || {
    let mut decoder = create_decoder()?;
    decode_in_thread(&mut decoder);
});

// Avoid: Sharing decoder across threads
let decoder = Arc::new(Mutex::new(create_decoder()?));
// This works but is inefficient
```

For multi-threaded decoding, create separate decoders per thread.

---

## Platform-Specific Notes

### Windows

- Requires Windows 10 or later for best hardware support
- D3D12 backend recommended for lowest latency
- MediaFoundation requires appropriate codec packs

### Linux

- Requires libva and appropriate VAAPI drivers
- DRM permissions needed for DMA-BUF sharing
- Check `/dev/dri/renderD*` device accessibility

### macOS

- Requires macOS 10.13+ for VideoToolbox
- Metal backend only
- Some codecs require specific macOS versions

### Cross-Platform (Vulkan Video)

- Requires recent Vulkan drivers (1.3+)
- Support varies by GPU vendor
- Check extension availability before use

---

## Future API Additions

Planned features for future versions:

```rust
// Hardware encoding
pub struct VideoEncoder { /* ... */ }

// Streaming support
pub struct VideoStream { /* ... */ }
impl Stream for VideoStream { /* ... */ }

// Post-processing
pub struct VideoFilter { /* ... */ }
decoder.with_filter(VideoFilter::Deinterlace)?;

// Advanced frame management
decoder.set_frame_callback(|frame| { /* ... */ });
```
