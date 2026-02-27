# Integration Guide

This guide shows how to integrate wgpu-video into your application for hardware-accelerated video decoding with seamless wgpu texture output.

## Table of Contents

- [Quick Start](#quick-start)
- [Basic Integration](#basic-integration)
- [Video Player Example](#video-player-example)
- [Streaming Video](#streaming-video)
- [Performance Optimization](#performance-optimization)
- [Common Patterns](#common-patterns)
- [Troubleshooting](#troubleshooting)
- [Real-World Examples](#real-world-examples)

---

## Quick Start

### Installation

Add to your `Cargo.toml`:

```toml
[dependencies]
wgpu = "0.18"
wgpu-video = "0.1"

# Optional: for async support
tokio = { version = "1.0", features = ["full"] }
```

Platform-specific features:

```toml
[target.'cfg(windows)'.dependencies]
wgpu-video = { version = "0.1", features = ["media-foundation"] }

[target.'cfg(target_os = "linux")'.dependencies]
wgpu-video = { version = "0.1", features = ["vaapi"] }

[target.'cfg(target_os = "macos")'.dependencies]
wgpu-video = { version = "0.1", features = ["videotoolbox"] }
```

### Minimal Example

```rust
use wgpu_video::{VideoDecoderBuilder, Codec};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Setup wgpu
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::default());
    let adapter = pollster::block_on(instance.request_adapter(&Default::default())).unwrap();
    let (device, queue) = pollster::block_on(adapter.request_device(&Default::default(), None))?;

    // Create video decoder
    let mut decoder = VideoDecoderBuilder::new()
        .with_wgpu_device(&device)
        .with_codec(Codec::H264)
        .with_resolution(1920, 1080)
        .build()?;

    // Decode a frame
    let encoded_data = load_video_frame();
    let frame = decoder.decode_frame(&encoded_data)?;

    // Use the texture
    let texture = frame.texture();
    render_video_frame(&device, &queue, texture);

    Ok(())
}
```

---

## Basic Integration

### Step 1: Initialize wgpu Device

```rust
use wgpu::*;

async fn create_device() -> (Device, Queue) {
    let instance = Instance::new(InstanceDescriptor {
        backends: Backends::all(),
        ..Default::default()
    });

    let adapter = instance
        .request_adapter(&RequestAdapterOptions {
            power_preference: PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: None,
        })
        .await
        .expect("Failed to find adapter");

    adapter
        .request_device(
            &DeviceDescriptor {
                label: Some("Main Device"),
                features: Features::empty(),
                limits: Limits::default(),
            },
            None,
        )
        .await
        .expect("Failed to create device")
}
```

### Step 2: Create Video Decoder

```rust
use wgpu_video::{VideoDecoderBuilder, Codec, PixelFormat, ColorSpace};

fn create_decoder(
    device: &Device,
    codec: Codec,
    width: u32,
    height: u32,
    extra_data: Option<Vec<u8>>,
) -> Result<VideoDecoder, wgpu_video::DecoderError> {
    let mut builder = VideoDecoderBuilder::new()
        .with_wgpu_device(device)
        .with_codec(codec)
        .with_resolution(width, height);

    if let Some(data) = extra_data {
        builder = builder.with_extra_data(data);
    }

    builder.build()
}
```

### Step 3: Set Up Rendering Pipeline

```rust
struct VideoRenderer {
    render_pipeline: RenderPipeline,
    bind_group_layout: BindGroupLayout,
    sampler: Sampler,
}

impl VideoRenderer {
    fn new(device: &Device, surface_format: TextureFormat) -> Self {
        // Create bind group layout for video texture
        let bind_group_layout = device.create_bind_group_layout(&BindGroupLayoutDescriptor {
            label: Some("Video Texture Layout"),
            entries: &[
                BindGroupLayoutEntry {
                    binding: 0,
                    visibility: ShaderStages::FRAGMENT,
                    ty: BindingType::Texture {
                        sample_type: TextureSampleType::Float { filterable: true },
                        view_dimension: TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                BindGroupLayoutEntry {
                    binding: 1,
                    visibility: ShaderStages::FRAGMENT,
                    ty: BindingType::Sampler(SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let sampler = device.create_sampler(&SamplerDescriptor {
            label: Some("Video Sampler"),
            address_mode_u: AddressMode::ClampToEdge,
            address_mode_v: AddressMode::ClampToEdge,
            mag_filter: FilterMode::Linear,
            min_filter: FilterMode::Linear,
            ..Default::default()
        });

        let shader = device.create_shader_module(ShaderModuleDescriptor {
            label: Some("Video Shader"),
            source: ShaderSource::Wgsl(include_str!("video_shader.wgsl").into()),
        });

        let pipeline_layout = device.create_pipeline_layout(&PipelineLayoutDescriptor {
            label: Some("Video Pipeline Layout"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });

        let render_pipeline = device.create_render_pipeline(&RenderPipelineDescriptor {
            label: Some("Video Render Pipeline"),
            layout: Some(&pipeline_layout),
            vertex: VertexState {
                module: &shader,
                entry_point: "vs_main",
                buffers: &[],
            },
            fragment: Some(FragmentState {
                module: &shader,
                entry_point: "fs_main",
                targets: &[Some(ColorTargetState {
                    format: surface_format,
                    blend: Some(BlendState::REPLACE),
                    write_mask: ColorWrites::ALL,
                })],
            }),
            primitive: PrimitiveState::default(),
            depth_stencil: None,
            multisample: MultisampleState::default(),
            multiview: None,
        });

        Self {
            render_pipeline,
            bind_group_layout,
            sampler,
        }
    }

    fn create_bind_group(&self, device: &Device, texture_view: &TextureView) -> BindGroup {
        device.create_bind_group(&BindGroupDescriptor {
            label: Some("Video Bind Group"),
            layout: &self.bind_group_layout,
            entries: &[
                BindGroupEntry {
                    binding: 0,
                    resource: BindingResource::TextureView(texture_view),
                },
                BindGroupEntry {
                    binding: 1,
                    resource: BindingResource::Sampler(&self.sampler),
                },
            ],
        })
    }

    fn render(&self, encoder: &mut CommandEncoder, view: &TextureView, bind_group: &BindGroup) {
        let mut render_pass = encoder.begin_render_pass(&RenderPassDescriptor {
            label: Some("Video Render Pass"),
            color_attachments: &[Some(RenderPassColorAttachment {
                view,
                resolve_target: None,
                ops: Operations {
                    load: LoadOp::Clear(Color::BLACK),
                    store: StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });

        render_pass.set_pipeline(&self.render_pipeline);
        render_pass.set_bind_group(0, bind_group, &[]);
        render_pass.draw(0..6, 0..1); // Full-screen quad
    }
}
```

### Step 4: Video Shader (WGSL)

Create `video_shader.wgsl`:

```wgsl
@group(0) @binding(0)
var video_texture: texture_2d<f32>;

@group(0) @binding(1)
var video_sampler: sampler;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) tex_coords: vec2<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    var out: VertexOutput;
    
    // Full-screen quad
    let x = f32((vertex_index << 1u) & 2u);
    let y = f32(vertex_index & 2u);
    
    out.position = vec4<f32>(x * 2.0 - 1.0, 1.0 - y * 2.0, 0.0, 1.0);
    out.tex_coords = vec2<f32>(x, y);
    
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    return textureSample(video_texture, video_sampler, in.tex_coords);
}
```

### Step 5: Main Render Loop

```rust
fn render_video_frame(
    device: &Device,
    queue: &Queue,
    surface: &Surface,
    renderer: &VideoRenderer,
    decoded_frame: &DecodedFrame,
) {
    let surface_texture = surface.get_current_texture().unwrap();
    let view = surface_texture.texture.create_view(&TextureViewDescriptor::default());

    let video_view = decoded_frame.create_view();
    let bind_group = renderer.create_bind_group(device, &video_view);

    let mut encoder = device.create_command_encoder(&CommandEncoderDescriptor {
        label: Some("Render Encoder"),
    });

    renderer.render(&mut encoder, &view, &bind_group);

    queue.submit(Some(encoder.finish()));
    surface_texture.present();
}
```

---

## Video Player Example

Complete example of a simple video player:

```rust
use wgpu_video::{VideoDecoderBuilder, Codec, DecodedFrame};
use std::fs::File;
use std::io::Read;

struct VideoPlayer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    decoder: wgpu_video::VideoDecoder,
    renderer: VideoRenderer,
    frame_buffer: Vec<DecodedFrame>,
}

impl VideoPlayer {
    fn new(
        device: wgpu::Device,
        queue: wgpu::Queue,
        surface_format: wgpu::TextureFormat,
        codec: Codec,
        width: u32,
        height: u32,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let decoder = VideoDecoderBuilder::new()
            .with_wgpu_device(&device)
            .with_codec(codec)
            .with_resolution(width, height)
            .with_pool_size(6) // Buffer more frames
            .build()?;

        let renderer = VideoRenderer::new(&device, surface_format);

        Ok(Self {
            device,
            queue,
            decoder,
            renderer,
            frame_buffer: Vec::new(),
        })
    }

    fn decode_frame(&mut self, data: &[u8]) -> Result<(), Box<dyn std::error::Error>> {
        let frame = self.decoder.decode_frame(data)?;
        self.frame_buffer.push(frame);
        Ok(())
    }

    fn render_next_frame(&mut self, surface: &wgpu::Surface) {
        if let Some(frame) = self.frame_buffer.first() {
            render_video_frame(
                &self.device,
                &self.queue,
                surface,
                &self.renderer,
                frame,
            );
        }
    }

    fn advance_frame(&mut self) {
        if !self.frame_buffer.is_empty() {
            self.frame_buffer.remove(0);
        }
    }

    fn seek(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        self.decoder.reset()?;
        self.frame_buffer.clear();
        Ok(())
    }

    fn flush(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let buffered = self.decoder.flush()?;
        self.frame_buffer.extend(buffered);
        Ok(())
    }
}

// Usage
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (device, queue) = pollster::block_on(create_device());
    
    let mut player = VideoPlayer::new(
        device,
        queue,
        wgpu::TextureFormat::Bgra8UnormSrgb,
        Codec::H264,
        1920,
        1080,
    )?;

    // Decode and play video
    let video_data = load_video_file("video.h264")?;
    for chunk in video_data.chunks(4096) {
        player.decode_frame(chunk)?;
    }

    player.flush()?;

    // Render loop
    loop {
        player.render_next_frame(&surface);
        player.advance_frame();
        std::thread::sleep(std::time::Duration::from_millis(33)); // ~30 FPS
    }

    Ok(())
}
```

---

## Streaming Video

### HTTP Live Streaming (HLS)

```rust
use wgpu_video::{VideoDecoderBuilder, Codec};
use reqwest;

struct HlsPlayer {
    decoder: wgpu_video::VideoDecoder,
    current_segment: usize,
    segments: Vec<String>,
}

impl HlsPlayer {
    async fn new(
        device: &wgpu::Device,
        manifest_url: &str,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        // Parse M3U8 manifest
        let manifest = reqwest::get(manifest_url).await?.text().await?;
        let segments = Self::parse_manifest(&manifest);

        let decoder = VideoDecoderBuilder::new()
            .with_wgpu_device(device)
            .with_codec(Codec::H264)
            .build()?;

        Ok(Self {
            decoder,
            current_segment: 0,
            segments,
        })
    }

    fn parse_manifest(manifest: &str) -> Vec<String> {
        manifest
            .lines()
            .filter(|line| !line.starts_with('#') && !line.is_empty())
            .map(|s| s.to_string())
            .collect()
    }

    async fn fetch_next_segment(&mut self) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        if self.current_segment >= self.segments.len() {
            return Err("End of stream".into());
        }

        let url = &self.segments[self.current_segment];
        let data = reqwest::get(url).await?.bytes().await?.to_vec();
        self.current_segment += 1;

        Ok(data)
    }

    async fn decode_next_segment(&mut self) -> Result<Vec<DecodedFrame>, Box<dyn std::error::Error>> {
        let segment_data = self.fetch_next_segment().await?;
        
        let mut frames = Vec::new();
        
        // Parse segment and decode frames
        for nal_unit in parse_h264_nalus(&segment_data) {
            let frame = self.decoder.decode_frame(&nal_unit)?;
            frames.push(frame);
        }

        Ok(frames)
    }
}

fn parse_h264_nalus(data: &[u8]) -> Vec<Vec<u8>> {
    // Simple NAL unit parser (start codes: 0x00 0x00 0x00 0x01)
    let mut nalus = Vec::new();
    let mut start = 0;

    for i in 0..data.len() - 3 {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 0 && data[i + 3] == 1 {
            if i > start {
                nalus.push(data[start..i].to_vec());
            }
            start = i + 4;
        }
    }

    if start < data.len() {
        nalus.push(data[start..].to_vec());
    }

    nalus
}
```

### WebRTC Integration

```rust
use wgpu_video::{VideoDecoderBuilder, Codec};
use webrtc::media::Sample;

struct WebRtcDecoder {
    decoder: wgpu_video::VideoDecoder,
}

impl WebRtcDecoder {
    fn new(device: &wgpu::Device) -> Result<Self, Box<dyn std::error::Error>> {
        let decoder = VideoDecoderBuilder::new()
            .with_wgpu_device(device)
            .with_codec(Codec::H264)
            .build()?;

        Ok(Self { decoder })
    }

    fn on_rtp_packet(&mut self, sample: Sample) -> Result<DecodedFrame, Box<dyn std::error::Error>> {
        // Decode RTP payload
        let frame = self.decoder.decode_frame(&sample.data)?;
        Ok(frame)
    }
}

// Usage with WebRTC track
async fn handle_video_track(
    track: Arc<TrackRemote>,
    decoder: Arc<Mutex<WebRtcDecoder>>,
) {
    while let Some(sample) = track.read_sample().await {
        let mut decoder = decoder.lock().await;
        match decoder.on_rtp_packet(sample) {
            Ok(frame) => {
                // Render frame
            }
            Err(e) => eprintln!("Decode error: {}", e),
        }
    }
}
```

---

## Performance Optimization

### Frame Buffering

```rust
use std::collections::VecDeque;
use wgpu_video::DecodedFrame;

struct FrameBuffer {
    frames: VecDeque<DecodedFrame>,
    max_size: usize,
}

impl FrameBuffer {
    fn new(max_size: usize) -> Self {
        Self {
            frames: VecDeque::with_capacity(max_size),
            max_size,
        }
    }

    fn push(&mut self, frame: DecodedFrame) -> bool {
        if self.frames.len() >= self.max_size {
            return false; // Buffer full
        }
        self.frames.push_back(frame);
        true
    }

    fn pop(&mut self) -> Option<DecodedFrame> {
        self.frames.pop_front()
    }

    fn is_full(&self) -> bool {
        self.frames.len() >= self.max_size
    }

    fn len(&self) -> usize {
        self.frames.len()
    }
}
```

### Async Decoding Pipeline

```rust
use tokio::sync::mpsc;
use wgpu_video::{VideoDecoder, DecodedFrame};

async fn async_decode_pipeline(
    mut decoder: VideoDecoder,
    mut rx: mpsc::Receiver<Vec<u8>>,
    tx: mpsc::Sender<DecodedFrame>,
) {
    while let Some(data) = rx.recv().await {
        match decoder.decode_frame_async(&data).await {
            Ok(frame) => {
                let _ = tx.send(frame).await;
            }
            Err(e) => {
                eprintln!("Decode error: {}", e);
            }
        }
    }
}

// Usage
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (encode_tx, encode_rx) = mpsc::channel(32);
    let (decode_tx, mut decode_rx) = mpsc::channel(32);

    let device = create_device().await.0;
    let decoder = VideoDecoderBuilder::new()
        .with_wgpu_device(&device)
        .with_codec(Codec::H264)
        .build()?;

    // Spawn decoder task
    tokio::spawn(async_decode_pipeline(decoder, encode_rx, decode_tx));

    // Send encoded data
    tokio::spawn(async move {
        for chunk in video_chunks {
            encode_tx.send(chunk).await.unwrap();
        }
    });

    // Receive decoded frames
    while let Some(frame) = decode_rx.recv().await {
        render_frame(&frame);
    }

    Ok(())
}
```

### Zero-Copy Optimization

```rust
use wgpu_video::{VideoDecoderBuilder, Codec};

fn create_optimized_decoder(
    device: &wgpu::Device,
) -> Result<wgpu_video::VideoDecoder, Box<dyn std::error::Error>> {
    // Query backend to ensure zero-copy path
    let caps = wgpu_video::DecoderCapabilities::query(device)?;
    
    println!("Backend: {:?}", caps.backend);
    println!("Hardware accelerated: {}", caps.hardware_accelerated);

    // Create decoder optimized for the current backend
    let decoder = VideoDecoderBuilder::new()
        .with_wgpu_device(device)
        .with_codec(Codec::H264)
        .with_pool_size(4) // Smaller pool for lower latency
        .build()?;

    Ok(decoder)
}
```

---

## Common Patterns

### Pattern 1: Codec Detection

```rust
use wgpu_video::{Codec, VideoDecoder};

fn detect_codec(file_path: &str) -> Result<Codec, Box<dyn std::error::Error>> {
    // Read file header
    let mut file = std::fs::File::open(file_path)?;
    let mut header = [0u8; 16];
    file.read_exact(&mut header)?;

    // Simple detection (extend as needed)
    let codec = if &header[4..8] == b"ftyp" {
        // MP4 container
        detect_mp4_codec(&file)?
    } else if header[0..3] == [0, 0, 1] {
        // Annex B format
        Codec::H264
    } else {
        return Err("Unknown codec".into());
    };

    Ok(codec)
}

fn create_decoder_from_file(
    device: &wgpu::Device,
    file_path: &str,
) -> Result<VideoDecoder, Box<dyn std::error::Error>> {
    let codec = detect_codec(file_path)?;
    
    if !VideoDecoder::is_codec_supported(codec) {
        return Err(format!("Codec {:?} not supported", codec).into());
    }

    VideoDecoderBuilder::new()
        .with_wgpu_device(device)
        .with_codec(codec)
        .build()
        .map_err(Into::into)
}
```

### Pattern 2: Graceful Fallback

```rust
use wgpu_video::{VideoDecoderBuilder, Codec, BackendType, DecoderError};

fn create_decoder_with_fallback(
    device: &wgpu::Device,
    codec: Codec,
) -> Result<VideoDecoder, Box<dyn std::error::Error>> {
    // Try hardware-accelerated first
    match VideoDecoderBuilder::new()
        .with_wgpu_device(device)
        .with_codec(codec)
        .hardware_accelerated(true)
        .build()
    {
        Ok(decoder) => {
            println!("Using hardware acceleration");
            return Ok(decoder);
        }
        Err(e) => {
            eprintln!("Hardware decoder failed: {}, trying software", e);
        }
    }

    // Fallback to software
    VideoDecoderBuilder::new()
        .with_wgpu_device(device)
        .with_codec(codec)
        .hardware_accelerated(false)
        .build()
        .map_err(Into::into)
}
```

### Pattern 3: Multi-Resolution Support

```rust
use wgpu_video::{VideoDecoderBuilder, Codec};
use std::collections::HashMap;

struct AdaptivePlayer {
    decoders: HashMap<u32, wgpu_video::VideoDecoder>,
    current_quality: u32,
    device: wgpu::Device,
}

impl AdaptivePlayer {
    fn new(device: wgpu::Device) -> Self {
        Self {
            decoders: HashMap::new(),
            current_quality: 1080,
            device,
        }
    }

    fn switch_quality(&mut self, quality: u32, codec: Codec) -> Result<(), Box<dyn std::error::Error>> {
        if !self.decoders.contains_key(&quality) {
            let (width, height) = Self::quality_to_resolution(quality);
            
            let decoder = VideoDecoderBuilder::new()
                .with_wgpu_device(&self.device)
                .with_codec(codec)
                .with_resolution(width, height)
                .build()?;
            
            self.decoders.insert(quality, decoder);
        }

        self.current_quality = quality;
        Ok(())
    }

    fn decode_frame(&mut self, data: &[u8]) -> Result<DecodedFrame, Box<dyn std::error::Error>> {
        let decoder = self.decoders.get_mut(&self.current_quality)
            .ok_or("No decoder for current quality")?;
        
        Ok(decoder.decode_frame(data)?)
    }

    fn quality_to_resolution(quality: u32) -> (u32, u32) {
        match quality {
            360 => (640, 360),
            480 => (854, 480),
            720 => (1280, 720),
            1080 => (1920, 1080),
            1440 => (2560, 1440),
            2160 => (3840, 2160),
            _ => (1920, 1080),
        }
    }
}
```

### Pattern 4: Frame Timing and Synchronization

```rust
use std::time::{Duration, Instant};
use wgpu_video::DecodedFrame;

struct TimedFrame {
    frame: DecodedFrame,
    pts: Duration, // Presentation timestamp
}

struct VideoSynchronizer {
    start_time: Instant,
    frame_queue: VecDeque<TimedFrame>,
}

impl VideoSynchronizer {
    fn new() -> Self {
        Self {
            start_time: Instant::now(),
            frame_queue: VecDeque::new(),
        }
    }

    fn add_frame(&mut self, frame: DecodedFrame, pts: Duration) {
        self.frame_queue.push_back(TimedFrame { frame, pts });
    }

    fn get_current_frame(&mut self) -> Option<&DecodedFrame> {
        let elapsed = self.start_time.elapsed();
        
        // Remove frames that are too old
        while let Some(front) = self.frame_queue.front() {
            if front.pts < elapsed.saturating_sub(Duration::from_millis(100)) {
                self.frame_queue.pop_front();
            } else {
                break;
            }
        }

        // Return current frame
        self.frame_queue
            .front()
            .filter(|f| f.pts <= elapsed)
            .map(|f| &f.frame)
    }

    fn reset(&mut self) {
        self.start_time = Instant::now();
        self.frame_queue.clear();
    }
}
```

---

## Troubleshooting

### Common Issues and Solutions

#### Issue 1: Decoder Initialization Fails

```rust
use wgpu_video::{VideoDecoderBuilder, Codec, DecoderCapabilities};

// Check capabilities first
let caps = DecoderCapabilities::query(&device)?;

if !caps.supported_codecs.contains(&Codec::H264) {
    eprintln!("H.264 not supported on this platform");
    eprintln!("Available codecs: {:?}", caps.supported_codecs);
    eprintln!("Backend: {:?}", caps.backend);
}

// Verify resolution is supported
let (width, height) = (3840, 2160);
if width > caps.max_resolution.0 || height > caps.max_resolution.1 {
    eprintln!("Resolution {}x{} exceeds maximum {}x{}",
        width, height,
        caps.max_resolution.0,
        caps.max_resolution.1
    );
}
```

#### Issue 2: Corrupted Frame Output

```rust
// Ensure proper decoder reset after seeking
decoder.reset()?;

// Decode from keyframe after reset
let keyframe_data = find_next_keyframe(&stream)?;
let frame = decoder.decode_frame(&keyframe_data)?;

// Flush decoder periodically
if frame_count % 300 == 0 {
    let _ = decoder.flush()?;
}
```

#### Issue 3: Performance Problems

```rust
use std::time::Instant;

fn profile_decode(decoder: &mut VideoDecoder, data: &[u8]) {
    let start = Instant::now();
    
    match decoder.decode_frame(data) {
        Ok(frame) => {
            let elapsed = start.elapsed();
            println!("Decode time: {:?}", elapsed);
            
            if elapsed > Duration::from_millis(33) {
                eprintln!("WARNING: Decode slower than 30fps target");
            }
        }
        Err(e) => eprintln!("Decode error: {}", e),
    }
}

// Check if hardware acceleration is actually being used
let info = decoder.info();
if !info.hardware_accelerated {
    eprintln!("WARNING: Using software decoding");
}
```

#### Issue 4: Memory Leaks

```rust
// Ensure frames are dropped properly
{
    let frame = decoder.decode_frame(&data)?;
    render_frame(&frame);
    // frame automatically dropped here, returns to pool
}

// Don't hold frames longer than necessary
let mut frame_cache = Vec::new();
for data in chunks {
    let frame = decoder.decode_frame(&data)?;
    frame_cache.push(frame);
    
    // Limit cache size
    if frame_cache.len() > 10 {
        frame_cache.remove(0);
    }
}
```

---

## Real-World Examples

### Example 1: Video Editor Preview

```rust
struct VideoEditor {
    decoder: VideoDecoder,
    timeline: Vec<VideoClip>,
    current_time: Duration,
}

struct VideoClip {
    start_frame: usize,
    end_frame: usize,
    data: Vec<Vec<u8>>,
}

impl VideoEditor {
    fn seek_to(&mut self, time: Duration) -> Result<DecodedFrame, Box<dyn std::error::Error>> {
        self.current_time = time;
        self.decoder.reset()?;
        
        // Find frame at timestamp
        let frame_index = (time.as_secs_f32() * 30.0) as usize;
        let clip = self.find_clip_at_frame(frame_index)?;
        
        // Decode from nearest keyframe
        let keyframe_idx = self.find_previous_keyframe(clip, frame_index);
        
        for i in keyframe_idx..=frame_index {
            let frame = self.decoder.decode_frame(&clip.data[i])?;
            if i == frame_index {
                return Ok(frame);
            }
        }
        
        Err("Failed to seek".into())
    }

    fn find_clip_at_frame(&self, frame: usize) -> Result<&VideoClip, Box<dyn std::error::Error>> {
        self.timeline
            .iter()
            .find(|clip| frame >= clip.start_frame && frame <= clip.end_frame)
            .ok_or_else(|| "Frame not in timeline".into())
    }

    fn find_previous_keyframe(&self, clip: &VideoClip, from: usize) -> usize {
        // Simple: assume keyframe every 30 frames
        (from / 30) * 30
    }
}
```

### Example 2: Security Camera DVR

```rust
use std::sync::Arc;
use tokio::sync::RwLock;

struct CameraFeed {
    id: String,
    decoder: VideoDecoder,
    latest_frame: Option<DecodedFrame>,
}

struct DVRSystem {
    cameras: Arc<RwLock<HashMap<String, CameraFeed>>>,
    device: wgpu::Device,
}

impl DVRSystem {
    async fn add_camera(&self, camera_id: String, codec: Codec) -> Result<(), Box<dyn std::error::Error>> {
        let decoder = VideoDecoderBuilder::new()
            .with_wgpu_device(&self.device)
            .with_codec(codec)
            .with_resolution(1920, 1080)
            .build()?;

        let feed = CameraFeed {
            id: camera_id.clone(),
            decoder,
            latest_frame: None,
        };

        self.cameras.write().await.insert(camera_id, feed);
        Ok(())
    }

    async fn process_frame(&self, camera_id: &str, data: Vec<u8>) -> Result<(), Box<dyn std::error::Error>> {
        let mut cameras = self.cameras.write().await;
        
        if let Some(feed) = cameras.get_mut(camera_id) {
            let frame = feed.decoder.decode_frame(&data)?;
            feed.latest_frame = Some(frame);
        }

        Ok(())
    }

    async fn get_latest_frame(&self, camera_id: &str) -> Option<DecodedFrame> {
        let cameras = self.cameras.read().await;
        cameras.get(camera_id)
            .and_then(|feed| feed.latest_frame.as_ref())
            .cloned()
    }
}
```

### Example 3: Game Cutscene Player

```rust
struct CutscenePlayer {
    decoder: VideoDecoder,
    audio_sync: AudioSynchronizer,
    is_playing: bool,
    is_skippable: bool,
}

impl CutscenePlayer {
    fn new(device: &wgpu::Device, video_file: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let decoder = VideoDecoderBuilder::new()
            .with_wgpu_device(device)
            .with_codec(Codec::H264)
            .build()?;

        Ok(Self {
            decoder,
            audio_sync: AudioSynchronizer::new(),
            is_playing: false,
            is_skippable: false,
        })
    }

    fn play(&mut self) {
        self.is_playing = true;
        self.audio_sync.play();
    }

    fn pause(&mut self) {
        self.is_playing = false;
        self.audio_sync.pause();
    }

    fn skip(&mut self) {
        if self.is_skippable {
            self.is_playing = false;
            self.audio_sync.stop();
        }
    }

    fn update(&mut self, dt: Duration) -> Option<DecodedFrame> {
        if !self.is_playing {
            return None;
        }

        // Sync with audio
        if self.audio_sync.should_decode_next_frame() {
            match self.decode_next() {
                Ok(frame) => Some(frame),
                Err(_) => {
                    self.is_playing = false; // End of cutscene
                    None
                }
            }
        } else {
            None
        }
    }

    fn decode_next(&mut self) -> Result<DecodedFrame, Box<dyn std::error::Error>> {
        // Implementation depends on video source
        todo!()
    }
}

struct AudioSynchronizer {
    // Audio sync implementation
}

impl AudioSynchronizer {
    fn new() -> Self {
        Self {}
    }
    fn play(&mut self) {}
    fn pause(&mut self) {}
    fn stop(&mut self) {}
    fn should_decode_next_frame(&self) -> bool {
        true
    }
}
```

---

## Additional Resources

- [API Documentation](./API_DESIGN.md)
- [Architecture Overview](./ARCHITECTURE.md)
- [Platform Backends](./PLATFORM_BACKENDS.md)
- [Development Guide](./DEVELOPMENT_GUIDE.md)

## Support

For questions and issues:
- GitHub Issues: [Report bugs](https://github.com/your-org/wgpu-video/issues)
- Discussions: [Ask questions](https://github.com/your-org/wgpu-video/discussions)
- Examples: [Browse examples](../examples/)