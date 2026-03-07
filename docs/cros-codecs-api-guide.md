# `cros-codecs` Decoding API Guide

This guide provides a detailed tutorial on how to use the `@third_party/cros-codecs` crate for hardware-accelerated video decoding. Specifically, it covers how to take packet metadata and data from a demuxer (like `src/demuxer/mod.rs`), and route it through the `cros-codecs` ecosystem for zero-copy hardware decoding using `libva` (in `src/backend/libva/decoder.rs`).

---

## 1. Architectural Overview

The `cros-codecs` crate is structured in three major layers to achieve stateless video decoding:

1. **Parsers (`cros_codecs::codec::*`)**: Extract bitstream syntax units (e.g., NALUs for H.264/H.265, OBUs for AV1) and parse them into structured metadata (Sequence Parameter Sets, Picture Parameter Sets, Slice Headers, etc.).
2. **Stateless Decoders (`cros_codecs::decoder::stateless::*`)**: The high-level orchestrators. A `StatelessDecoder` maintains the Decoded Picture Buffer (DPB), processes format changes, and keeps track of reference frames. It dictates *when* a picture should be decoded or output.
3. **Backends (`cros_codecs::backend::*`)**: Talk to the actual hardware. `cros-codecs` provides a pre-built `VaapiBackend` that natively maps the decoder's state into `libva` calls (allocating `VASurfaceID`s, creating `VAPictureParameterBuffer`s, executing `vaBeginPicture`/`vaRenderPicture`/`vaEndPicture`).

**Key Takeaway:** You **do not** need to manually call `libva` functions to decode the bitstream. `cros-codecs` already provides `VaapiBackend` which implements the entire `libva` decode pipeline for you. Your job is to feed the demuxed bitstream into the `StatelessDecoder` and manage the allocation of frame memory.

---

## 2. Using the Codec Parsers Directly (Optional)

If you only want to parse metadata (e.g., in `src/demuxer/mod.rs`) to inspect the stream without decoding it, you can use the raw parsers.

For H.264, the bitstream is split into Annex-B NAL units:

```rust
use cros_codecs::codec::h264::nalu::Nalu;
use cros_codecs::codec::h264::parser::NaluHeader;
use std::io::Cursor;

pub fn extract_nalus(packet_data: &[u8]) {
    let mut cursor = Cursor::new(packet_data);
    
    // Nalu::next automatically finds the next Annex-B start code (0x00000001)
    while let Ok(nalu) = Nalu::<'_, NaluHeader>::next(&mut cursor) {
        println!("Found NALU of type: {:?}", nalu.header.type_);
        println!("NALU data size: {} bytes", nalu.size);
        
        // If you need to parse deeper (e.g., SPS/PPS):
        // let mut parser = cros_codecs::codec::h264::parser::Parser::default();
        // let sps = parser.parse_sps(&nalu).unwrap();
    }
}
```

*Note: You don't have to do this if you intend to decode the video. The `StatelessDecoder` handles NALU extraction and parsing internally.*

---

## 3. The Decoding Pipeline: Setup and Initialization

To perform hardware decoding via `libva`, we combine a codec (like `H264`) with the `VaapiBackend`.

### 3.1. Initialize the VAAPI Decoder

In your `src/backend/libva/decoder.rs`:

```rust
use cros_codecs::decoder::stateless::h264::H264;
use cros_codecs::decoder::stateless::StatelessDecoder;
use cros_codecs::decoder::BlockingMode;
use cros_codecs::video_frame::generic_dma_video_frame::GenericDmaVideoFrame;
use std::rc::Rc;
use libva::Display;

pub struct VideoPlayer {
    decoder: StatelessDecoder<H264, cros_codecs::backend::vaapi::decoder::VaapiBackend<GenericDmaVideoFrame>>,
}

impl VideoPlayer {
    pub fn new() -> Option<Self> {
        // 1. Open the VA display
        let display = Rc::new(Display::open()?);

        // 2. Instantiate the decoder
        // BlockingMode::Blocking ensures that the decoder blocks until the hardware 
        // finishes rendering the frame. NonBlocking returns the frame immediately, 
        // but you must manually `sync()` it before accessing its pixels.
        let decoder = StatelessDecoder::<H264, _>::new_vaapi(
            display,
            BlockingMode::Blocking
        ).ok()?;

        Some(Self { decoder })
    }
}
```

### 3.2. Implement Frame Memory Allocation

When the decoder decides it's time to decode a picture, it will ask you for a memory buffer to decode into. This is done via an allocation callback `alloc_cb`.

`cros-codecs` requires the returned frame to implement the `cros_codecs::video_frame::VideoFrame` trait. You can use the provided `GbmVideoFrame` or `GenericDmaVideoFrame`, or implement it for your own custom `wgpu` textures.

Using the built-in `FramePool`:

```rust
use cros_codecs::video_frame::frame_pool::{FramePool, PooledVideoFrame};
use cros_codecs::video_frame::gbm_video_frame::{GbmDevice, GbmUsage};
use cros_codecs::decoder::StreamInfo;
use cros_codecs::Fourcc;
use std::sync::{Arc, Mutex};
use std::path::PathBuf;

// Initialize a GBM Device (usually matching the DRM render node)
let gbm_device = Arc::new(GbmDevice::open(PathBuf::from("/dev/dri/renderD128")).unwrap());

// Initialize a frame pool. The closure is called whenever the pool needs to be resized
// or populated based on the stream's requirements.
let framepool = Arc::new(Mutex::new(FramePool::new(move |stream_info: &StreamInfo| {
    gbm_device.clone()
        .new_frame(
            Fourcc::from(b"NV12"), // or stream_info.format.into()
            stream_info.display_resolution,
            stream_info.coded_resolution,
            GbmUsage::Decode,
        )
        .expect("Failed to allocate GBM frame")
        .to_generic_dma_video_frame()
        .expect("Failed to export to DMA")
})));
```

---

## 4. The Decoding Loop (`decode` and `next_event`)

When you extract a packet (a slice of bytes) from the `Demuxer`, you feed it directly into the `StatelessDecoder`.

### 4.1. Feeding the Bitstream

**Crucial detail for H.264 / H.265:** The `decode` method parses exactly **one NAL unit** at a time. It returns the number of bytes consumed. You must loop over your packet's buffer until all bytes are consumed.

```rust
use cros_codecs::decoder::stateless::DecodeError;

pub fn process_packet(&mut self, packet_data: &[u8]) {
    let mut bitstream = packet_data;
    
    // Create the allocation callback that pulls from our framepool
    let mut alloc_cb = || self.framepool.lock().unwrap().alloc();

    while !bitstream.is_empty() {
        match self.decoder.decode(
            current_timestamp, // e.g. PTS from demuxer
            bitstream,
            &mut alloc_cb
        ) {
            Ok(consumed_bytes) => {
                // Advance the bitstream slice by the amount consumed
                bitstream = &bitstream[consumed_bytes..];
            }
            Err(DecodeError::CheckEvents) => {
                // The decoder needs us to process events (like a format change or 
                // bumping ready frames) before it can accept more input.
                self.handle_decoder_events();
            }
            Err(DecodeError::NotEnoughOutputBuffers(needed)) => {
                // The framepool is empty. We must free/return a frame to the pool
                // or wait until a frame is no longer being used by the renderer.
                wait_for_free_buffers();
            }
            Err(e) => panic!("Decode error: {:?}", e),
        }
    }
}
```

### 4.2. Handling Decoder Events

The decoder queues up events as it processes the bitstream. You retrieve them using `next_event()`.

```rust
use cros_codecs::decoder::DecoderEvent;

fn handle_decoder_events(&mut self) {
    while let Some(event) = self.decoder.next_event() {
        match event {
            DecoderEvent::FormatChanged => {
                // The stream resolution, profile, or format has changed.
                // We must resize our frame pool to accommodate the new requirements.
                let stream_info = self.decoder.stream_info().unwrap();
                
                println!("Format changed to {:?} {}x{}", 
                    stream_info.format, 
                    stream_info.coded_resolution.width, 
                    stream_info.coded_resolution.height
                );
                
                self.framepool.lock().unwrap().resize(stream_info);
            }
            DecoderEvent::FrameReady(handle) => {
                // A frame has been fully decoded and is ready for display!
                // `handle.video_frame()` gives you the underlying frame object 
                // (e.g., the GenericDmaVideoFrame we allocated).
                
                let timestamp = handle.timestamp();
                let video_frame = handle.video_frame();
                
                println!("Decoded frame ready! PTS: {}", timestamp);
                
                // If using BlockingMode::NonBlocking, you must sync manually:
                // handle.sync().unwrap();
                
                // You can now export this frame to wgpu, Vulkan, or OpenGL.
                render_frame(video_frame);
            }
        }
    }
}
```

---

## 5. End of Stream and Flushing

When the demuxer signals that the stream has ended, you must flush the decoder to ensure all pending frames in the Decoded Picture Buffer (DPB) are emitted as `FrameReady` events.

```rust
pub fn flush_decoder(&mut self) {
    self.decoder.flush().expect("Failed to flush decoder");
    self.handle_decoder_events(); // Pump out the remaining frames
}
```

## Summary of the Integration Architecture

1. **`src/demuxer/mod.rs`**: Extracts `packet_data` and `timestamp`.
2. **`src/backend/libva/decoder.rs`**:
   - Owns the `StatelessDecoder` and `FramePool`.
   - Loops over `packet_data`, passing it to `decoder.decode(...)`.
   - Consumes `decoder.next_event()`.
   - Resizes `FramePool` on `FormatChanged`.
   - Exports the decoded `VideoFrame` (e.g. DMA-BUF FDs) on `FrameReady` to be ingested by Vulkan/wgpu via `src/backend/libva/vulkan_import.rs`.