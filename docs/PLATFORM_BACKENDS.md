# Platform Backends Implementation Guide

This document provides detailed implementation guidance for each platform-specific decoder backend in wgpu-video.

## Table of Contents

- [Backend Overview](#backend-overview)
- [Windows MediaFoundation](#windows-mediafoundation)
- [Linux VA-API](#linux-va-api)
- [macOS VideoToolbox](#macos-videotoolbox)
- [Vulkan Video](#vulkan-video)
- [Backend Interface](#backend-interface)
- [Texture Interoperability](#texture-interoperability)
- [Testing Strategy](#testing-strategy)

---

## Backend Overview

Each backend must implement the `DecoderBackend` trait and handle:

1. **Initialization**: Set up native decoder with hardware acceleration
2. **Decoding**: Submit encoded data and retrieve decoded frames
3. **Texture Interop**: Share GPU textures with wgpu
4. **Synchronization**: Coordinate GPU operations between decoder and wgpu
5. **Cleanup**: Release resources properly

### Backend Selection Priority

```
Windows:
  - DX12 wgpu backend → MediaFoundation (D3D12)
  - DX11 wgpu backend → MediaFoundation (D3D11)
  - Vulkan wgpu backend → MediaFoundation (copy) or Vulkan Video

Linux:
  - Vulkan wgpu backend → VA-API (DRM) or Vulkan Video
  - OpenGL wgpu backend → VA-API (EGL)

macOS:
  - Metal wgpu backend → VideoToolbox (IOSurface)

Cross-platform:
  - Vulkan wgpu backend → Vulkan Video (if available)
```

---

## Windows MediaFoundation

### Overview

MediaFoundation (MF) is Windows' native multimedia framework with excellent hardware acceleration support across GPU vendors (Intel QSV, NVIDIA NVDEC, AMD VCE).

### Architecture

```
VideoDecoder
    ↓
MFDecoder (implements DecoderBackend)
    ↓
IMFTransform (Hardware MFT)
    ↓
D3D11/D3D12 Texture
    ↓
wgpu::Texture (via DXGI sharing)
```

### Key Components

#### 1. Decoder Initialization

**Steps:**
1. Initialize COM with `CoInitializeEx`
2. Create DXGI device manager
3. Enumerate and select hardware MFT
4. Configure input media type (codec, resolution, profile)
5. Configure output media type (NV12, P010 for 10-bit)
6. Bind D3D device to decoder
7. Start streaming

**Key Windows APIs:**
- `MFTEnumEx`: Enumerate hardware transforms
- `IMFTransform`: Core decoder interface
- `IMFDXGIDeviceManager`: Manage D3D devices
- `ID3D11VideoDevice`: D3D11 video acceleration
- `ID3D12VideoDevice`: D3D12 video acceleration

#### 2. D3D11 Integration

**Advantages:**
- Simpler API
- Better compatibility across hardware
- Easier texture sharing

**Implementation:**
```rust
// Pseudo-code structure
struct MFDecoderD3D11 {
    transform: IMFTransform,
    d3d11_device: ID3D11Device,
    d3d11_context: ID3D11DeviceContext,
    dxgi_device_manager: IMFDXGIDeviceManager,
    output_samples: Vec<IMFSample>,
}

impl MFDecoderD3D11 {
    fn decode_frame(&mut self, data: &[u8]) -> Result<DecodedFrame> {
        // 1. Create input sample from data
        let input_sample = self.create_mf_sample(data)?;
        
        // 2. Submit to transform
        self.transform.ProcessInput(0, input_sample)?;
        
        // 3. Get output sample
        let output_sample = self.transform.ProcessOutput(...)?;
        
        // 4. Extract D3D11 texture from sample
        let d3d11_texture = self.get_texture_from_sample(output_sample)?;
        
        // 5. Share with wgpu
        let wgpu_texture = self.import_d3d11_texture(d3d11_texture)?;
        
        Ok(DecodedFrame::new(wgpu_texture))
    }
    
    fn import_d3d11_texture(&self, d3d11_tex: ID3D11Texture2D) -> Result<wgpu::Texture> {
        // Get DXGI surface
        let dxgi_surface: IDXGISurface = d3d11_tex.cast()?;
        
        // Get shared handle
        let resource: IDXGIResource1 = dxgi_surface.cast()?;
        let shared_handle = resource.GetSharedHandle()?;
        
        // Import into wgpu using HAL
        // This requires wgpu hal access for external texture creation
        let hal_texture = unsafe {
            <wgpu::dx11::Device as hal::Device>::texture_from_raw(
                shared_handle,
                desc,
            )
        };
        
        Ok(hal_texture)
    }
}
```

#### 3. D3D12 Integration

**Advantages:**
- Better performance
- Lower CPU overhead
- Modern API design

**Additional Considerations:**
- Fence synchronization is critical
- Resource state transitions
- Command queue coordination

**Synchronization Pattern:**
```rust
struct MFDecoderD3D12 {
    // ... decoder fields
    shared_fence: ID3D12Fence,
    fence_value: u64,
}

impl MFDecoderD3D12 {
    fn decode_frame(&mut self, data: &[u8]) -> Result<DecodedFrame> {
        // Decode frame...
        let d3d12_resource = self.process_frame(data)?;
        
        // Signal fence after decode completes
        self.decoder_queue.Signal(&self.shared_fence, self.fence_value)?;
        
        // Import into wgpu with fence
        let wgpu_texture = self.import_with_sync(
            d3d12_resource,
            self.fence_value,
        )?;
        
        self.fence_value += 1;
        
        Ok(DecodedFrame::new(wgpu_texture))
    }
}
```

#### 4. Format Handling

**Supported Formats:**
- `NV12`: 8-bit 4:2:0 (most common)
- `P010`: 10-bit 4:2:0 (HDR)
- `AYUV`: 4:4:4 (rare)

**Conversion Strategy:**
- Prefer native wgpu support for NV12 if available
- Fall back to compute shader YUV→RGB conversion
- Store conversion shader as embedded WGSL

#### 5. Capability Detection

```rust
impl MFDecoderCapabilities {
    fn query_codec_support(codec: Codec) -> Result<CodecSupport> {
        // Enumerate MFTs with MFTEnumEx
        let mfts = enumerate_hardware_mfts(codec)?;
        
        if mfts.is_empty() {
            return Ok(CodecSupport::NotSupported);
        }
        
        // Check for profile support
        let profiles = query_supported_profiles(&mfts[0])?;
        
        Ok(CodecSupport::HardwareAccelerated { profiles })
    }
}
```

### Error Handling

**Common Errors:**
- `MF_E_TRANSFORM_NEED_MORE_INPUT`: Need more data, not an error
- `MF_E_TRANSFORM_STREAM_CHANGE`: Format change, reconfigure
- Hardware failure: Fall back to software MFT

### Testing Considerations

- Test on Intel, NVIDIA, AMD GPUs
- Test with different wgpu backends (DX11, DX12, Vulkan on Windows)
- Test codec support variations across hardware
- Test HDR content (10-bit, different color spaces)

---

## Linux VA-API

### Overview

VA-API (Video Acceleration API) is the standard video acceleration interface on Linux, supported by Intel, AMD, and NVIDIA (via VDPAU bridge) GPUs.

### Architecture

```
VideoDecoder
    ↓
VAAPIDecoder (implements DecoderBackend)
    ↓
VADisplay + VAContext + VASurface
    ↓
DRM PRIME FD (DMA-BUF)
    ↓
VkImage (via VK_EXT_external_memory_dma_buf)
    ↓
wgpu::Texture
```

### Key Components

#### 1. Display and Context Management

**Steps:**
1. Open DRM device (`/dev/dri/renderD128`)
2. Create VADisplay from DRM fd
3. Initialize VA-API with `vaInitialize`
4. Query codec support with `vaQueryConfigEntrypoints`
5. Create VAConfig for codec and profile
6. Create VAContext for video parameters

**Implementation:**
```rust
struct VAAPIDecoder {
    drm_fd: RawFd,
    display: VADisplay,
    config: VAConfigID,
    context: VAContextID,
    surfaces: Vec<VASurfaceID>,
    surface_pool: SurfacePool,
}

impl VAAPIDecoder {
    fn initialize(codec: Codec, resolution: Resolution) -> Result<Self> {
        // 1. Open DRM device
        let drm_fd = open_drm_device()?;
        
        // 2. Create display
        let display = unsafe { vaGetDisplayDRM(drm_fd) };
        
        // 3. Initialize
        let mut major = 0;
        let mut minor = 0;
        unsafe { vaInitialize(display, &mut major, &mut minor) };
        
        // 4. Query entrypoints
        let profile = codec_to_va_profile(codec);
        let entrypoint = VAEntrypointVLD; // Variable Length Decoding
        
        // 5. Create config
        let mut attribs = [
            VAConfigAttrib {
                type_: VAConfigAttribRTFormat,
                value: VA_RT_FORMAT_YUV420,
            }
        ];
        let mut config = 0;
        unsafe {
            vaCreateConfig(
                display,
                profile,
                entrypoint,
                attribs.as_mut_ptr(),
                attribs.len() as i32,
                &mut config,
            )
        };
        
        // 6. Create surfaces
        let surfaces = Self::create_surface_pool(display, resolution)?;
        
        // 7. Create context
        let mut context = 0;
        unsafe {
            vaCreateContext(
                display,
                config,
                resolution.width as i32,
                resolution.height as i32,
                VA_PROGRESSIVE,
                surfaces.as_ptr(),
                surfaces.len() as i32,
                &mut context,
            )
        };
        
        Ok(Self {
            drm_fd,
            display,
            config,
            context,
            surfaces,
            surface_pool: SurfacePool::new(surfaces),
        })
    }
}
```

#### 2. Decoding Process

**Steps:**
1. Parse bitstream for picture parameters
2. Fill VA buffers (picture params, slice params, bitstream)
3. Call `vaBeginPicture`
4. Render buffers with `vaRenderPicture`
5. End picture with `vaEndPicture`
6. Sync with `vaSyncSurface`
7. Export as DMA-BUF
8. Import into Vulkan

**Implementation:**
```rust
impl VAAPIDecoder {
    fn decode_frame(&mut self, data: &[u8]) -> Result<DecodedFrame> {
        // 1. Get free surface from pool
        let surface = self.surface_pool.acquire()?;
        
        // 2. Parse parameters (codec-specific)
        let picture_params = self.parse_picture_params(data)?;
        let slice_params = self.parse_slice_params(data)?;
        
        // 3. Create VA buffers
        let pic_buf = self.create_buffer(
            VAPictureParameterBufferType,
            &picture_params,
        )?;
        let slice_buf = self.create_buffer(
            VASliceParameterBufferType,
            &slice_params,
        )?;
        let data_buf = self.create_buffer(
            VASliceDataBufferType,
            data,
        )?;
        
        // 4. Decode
        unsafe {
            vaBeginPicture(self.display, self.context, surface);
            vaRenderPicture(
                self.display,
                self.context,
                &[pic_buf, slice_buf, data_buf],
            );
            vaEndPicture(self.display, self.context);
            
            // 5. Wait for completion
            vaSyncSurface(self.display, surface);
        }
        
        // 6. Export as DMA-BUF
        let dmabuf_fd = self.export_surface_as_dmabuf(surface)?;
        
        // 7. Import into wgpu
        let wgpu_texture = self.import_dmabuf_to_wgpu(dmabuf_fd)?;
        
        Ok(DecodedFrame::new(wgpu_texture))
    }
}
```

#### 3. DMA-BUF Export

**Zero-Copy Path:**
```rust
impl VAAPIDecoder {
    fn export_surface_as_dmabuf(&self, surface: VASurfaceID) -> Result<RawFd> {
        let mut descriptor = VADRMPRIMESurfaceDescriptor::default();
        
        unsafe {
            vaExportSurfaceHandle(
                self.display,
                surface,
                VA_SURFACE_ATTRIB_MEM_TYPE_DRM_PRIME_2,
                VA_EXPORT_SURFACE_READ_ONLY,
                &mut descriptor,
            )?;
        }
        
        // Return first plane's FD (for NV12, we have 2 planes)
        Ok(descriptor.objects[0].fd)
    }
}
```

#### 4. Vulkan Import

**Using VK_EXT_external_memory_dma_buf:**
```rust
impl VAAPIDecoder {
    fn import_dmabuf_to_wgpu(&self, dmabuf_fd: RawFd) -> Result<wgpu::Texture> {
        // This requires wgpu HAL access
        // Get the Vulkan device from wgpu
        let vk_device = self.wgpu_device.as_hal::<wgpu::vulkan::Api>();
        
        // Create external memory info
        let external_memory_info = vk::ExternalMemoryImageCreateInfo {
            handle_types: vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT,
            ..Default::default()
        };
        
        // Create image with external memory
        let image_info = vk::ImageCreateInfo {
            p_next: &external_memory_info as *const _ as *const _,
            image_type: vk::ImageType::TYPE_2D,
            format: vk::Format::G8_B8R8_2PLANE_420_UNORM, // NV12
            // ... other parameters
            ..Default::default()
        };
        
        let vk_image = unsafe {
            vk_device.create_image(&image_info, None)?
        };
        
        // Import DMA-BUF FD
        let import_fd_info = vk::ImportMemoryFdInfoKHR {
            handle_type: vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT,
            fd: dmabuf_fd,
            ..Default::default()
        };
        
        let memory_requirements = unsafe {
            vk_device.get_image_memory_requirements(vk_image)
        };
        
        let alloc_info = vk::MemoryAllocateInfo {
            p_next: &import_fd_info as *const _ as *const _,
            allocation_size: memory_requirements.size,
            memory_type_index: self.find_memory_type_index(
                memory_requirements.memory_type_bits
            ),
            ..Default::default()
        };
        
        let memory = unsafe {
            vk_device.allocate_memory(&alloc_info, None)?
        };
        
        unsafe {
            vk_device.bind_image_memory(vk_image, memory, 0)?;
        }
        
        // Wrap in wgpu texture
        let hal_texture = unsafe {
            <wgpu::hal::vulkan::Device>::texture_from_raw(
                vk_image,
                &texture_desc,
                None,
            )
        };
        
        Ok(self.wgpu_device.create_texture_from_hal(hal_texture))
    }
}
```

#### 5. Capability Detection

```rust
impl VAAPICapabilities {
    fn query_codec_support(display: VADisplay, codec: Codec) -> Result<CodecSupport> {
        let profile = codec_to_va_profile(codec);
        
        // Query entrypoints
        let mut entrypoints = vec![0; vaMaxNumEntrypoints(display)];
        let mut num_entrypoints = 0;
        
        unsafe {
            vaQueryConfigEntrypoints(
                display,
                profile,
                entrypoints.as_mut_ptr(),
                &mut num_entrypoints,
            )
        };
        
        if num_entrypoints == 0 {
            return Ok(CodecSupport::NotSupported);
        }
        
        // Check for VLD support (hardware decoding)
        let has_vld = entrypoints[..num_entrypoints as usize]
            .contains(&VAEntrypointVLD);
        
        if has_vld {
            Ok(CodecSupport::HardwareAccelerated {
                profiles: query_profiles(display, profile)?,
            })
        } else {
            Ok(CodecSupport::NotSupported)
        }
    }
}
```

### Error Handling

**Common Errors:**
- Display creation failure: Check DRM permissions
- Surface allocation failure: Reduce pool size
- Export failure: Driver doesn't support DMA-BUF
- Import failure: Vulkan doesn't support external memory

### Testing Considerations

- Test on Intel (i965, iHD), AMD (Mesa), NVIDIA (nouveau/proprietary)
- Test with different DRM devices (multi-GPU systems)
- Test DMA-BUF export support
- Verify modifiers are handled correctly

---

## macOS VideoToolbox

### Overview

VideoToolbox is Apple's hardware-accelerated video framework, tightly integrated with Metal and CoreVideo.

### Architecture

```
VideoDecoder
    ↓
VideoToolboxDecoder (implements DecoderBackend)
    ↓
VTDecompressionSession
    ↓
CVPixelBuffer / IOSurface
    ↓
MTLTexture
    ↓
wgpu::Texture
```

### Key Components

#### 1. Decompression Session

**Steps:**
1. Create format description from codec data
2. Configure output attributes (Metal compatibility)
3. Create decompression session with callback
4. Set session properties (hardware acceleration)

**Implementation:**
```rust
use core_video_sys::*;
use video_toolbox_sys::*;
use metal::*;

struct VideoToolboxDecoder {
    session: VTDecompressionSessionRef,
    output_callback: VTDecompressionOutputCallbackRecord,
    format_description: CMFormatDescriptionRef,
    decoded_frames: Arc<Mutex<VecDeque<CVPixelBufferRef>>>,
    metal_device: MTLDevice,
}

impl VideoToolboxDecoder {
    fn initialize(codec: Codec, format_data: &[u8]) -> Result<Self> {
        // 1. Create format description from SPS/PPS (H.264) or similar
        let format_desc = unsafe {
            Self::create_format_description(codec, format_data)?
        };
        
        // 2. Configure output attributes for Metal
        let pixel_buffer_attributes = CFDictionary::from_pairs(&[
            (
                kCVPixelBufferPixelFormatTypeKey,
                kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange, // NV12
            ),
            (
                kCVPixelBufferMetalCompatibilityKey,
                kCFBooleanTrue,
            ),
            (
                kCVPixelBufferIOSurfacePropertiesKey,
                CFDictionary::new(), // Enable IOSurface backing
            ),
        ]);
        
        // 3. Setup callback for decoded frames
        let decoded_frames = Arc::new(Mutex::new(VecDeque::new()));
        let callback_data = Arc::clone(&decoded_frames);
        
        let callback = VTDecompressionOutputCallbackRecord {
            decompressionOutputCallback: Some(decompress_callback),
            decompressionOutputRefCon: Arc::into_raw(callback_data) as *mut _,
        };
        
        // 4. Create session
        let mut session: VTDecompressionSessionRef = ptr::null_mut();
        let status = unsafe {
            VTDecompressionSessionCreate(
                kCFAllocatorDefault,
                format_desc,
                ptr::null(), // decoder_spec (null = auto select)
                pixel_buffer_attributes.as_concrete_TypeRef(),
                &callback,
                &mut session,
            )
        };
        
        if status != noErr {
            return Err(DecoderError::InitializationFailed);
        }
        
        // 5. Enable hardware acceleration
        unsafe {
            VTSessionSetProperty(
                session,
                kVTDecompressionPropertyKey_UsingHardwareAcceleratedVideoDecoder,
                kCFBooleanTrue,
            );
        }
        
        Ok(Self {
            session,
            output_callback: callback,
            format_description: format_desc,
            decoded_frames,
            metal_device: MTLDevice::system_default().unwrap(),
        })
    }
}

// Callback invoked when frame is decoded
extern "C" fn decompress_callback(
    callback_data: *mut c_void,
    source_frame_ref_con: *mut c_void,
    status: OSStatus,
    info_flags: VTDecodeInfoFlags,
    image_buffer: CVImageBufferRef,
    presentation_timestamp: CMTime,
    presentation_duration: CMTime,
) {
    if status != noErr {
        return;
    }
    
    // Store decoded frame
    let frames = unsafe {
        &*(callback_data as *const Mutex<VecDeque<CVPixelBufferRef>>)
    };
    
    let pixel_buffer = image_buffer as CVPixelBufferRef;
    unsafe { CFRetain(pixel_buffer as *const _) };
    
    frames.lock().unwrap().push_back(pixel_buffer);
}
```

#### 2. Decoding Process

**Steps:**
1. Create CMSampleBuffer from encoded data
2. Decode with `VTDecompressionSessionDecodeFrame`
3. Wait for callback
4. Retrieve CVPixelBuffer
5. Get IOSurface from pixel buffer
6. Create Metal texture from IOSurface
7. Import into wgpu

**Implementation:**
```rust
impl VideoToolboxDecoder {
    fn decode_frame(&mut self, data: &[u8], timestamp: u64) -> Result<DecodedFrame> {
        // 1. Create CMBlockBuffer from data
        let block_buffer = unsafe {
            let mut block_buffer: CMBlockBufferRef = ptr::null_mut();
            CMBlockBufferCreateWithMemoryBlock(
                kCFAllocatorDefault,
                data.as_ptr() as *mut _,
                data.len(),
                kCFAllocatorNull,
                ptr::null(),
                0,
                data.len(),
                0,
                &mut block_buffer,
            );
            block_buffer
        };
        
        // 2. Create CMSampleBuffer
        let sample_buffer = unsafe {
            let mut sample_buffer: CMSampleBufferRef = ptr::null_mut();
            CMSampleBufferCreate(
                kCFAllocatorDefault,
                block_buffer,
                true,
                None,
                ptr::null_mut(),
                self.format_description,
                1, // num samples
                0, // num sample timing entries
                ptr::null(),
                0,
                ptr::null(),
                &mut sample_buffer,
            );
            sample_buffer
        };
        
        // 3. Decode frame
        let decode_flags = 0;
        let mut info_flags = 0;
        
        let status = unsafe {
            VTDecompressionSessionDecodeFrame(
                self.session,
                sample_buffer,
                decode_flags,
                ptr::null_mut(), // source frame ref con
                &mut info_flags,
            )
        };
        
        if status != noErr {
            return Err(DecoderError::DecodingFailed);
        }
        
        // 4. Wait for frame (with timeout)
        unsafe { VTDecompressionSessionWaitForAsynchronousFrames(self.session) };
        
        // 5. Get decoded frame from queue
        let pixel_buffer = self.decoded_frames
            .lock()
            .unwrap()
            .pop_front()
            .ok_or(DecoderError::NoFrameAvailable)?;
        
        // 6. Convert to wgpu texture
        let wgpu_texture = self.pixel_buffer_to_wgpu(pixel_buffer)?;
        
        Ok(DecodedFrame::new(wgpu_texture))
    }
}
```

#### 3. IOSurface to Metal Texture

**Zero-Copy Path:**
```rust
impl VideoToolboxDecoder {
    fn pixel_buffer_to_wgpu(&self, pixel_buffer: CVPixelBufferRef) -> Result<wgpu::Texture> {
        // 1. Get IOSurface from pixel buffer
        let iosurface = unsafe {
            CVPixelBufferGetIOSurface(pixel_buffer)
        };
        
        if iosurface.is_null() {
            return Err(DecoderError::NoIOSurface);
        }
        
        // 2. Create Metal texture from IOSurface
        let width = unsafe { IOSurfaceGetWidth(iosurface) };
        let height = unsafe { IOSurfaceGetHeight(iosurface) };
        
        let texture_descriptor = MTLTextureDescriptor::new();
        texture_descriptor.set_texture_type(MTLTextureType::D2);
        texture_descriptor.set_pixel_format(MTLPixelFormat::BGRA8Unorm);
        texture_descriptor.set_width(width as u64);
        texture_descriptor.set_height(height as u64);
        texture_descriptor.set_usage(MTLTextureUsage::ShaderRead);
        
        let metal_texture = unsafe {
            self.metal_device.new_texture_with_descriptor_iosurface(
                &texture_descriptor,
                iosurface,
                0, // plane
            )
        };
        
        // 3. Import into wgpu
        let hal_texture = unsafe {
            <wgpu::hal::metal::Device>::texture_from_raw(
                metal_texture.as_ptr() as *mut _,
                &wgpu_texture_desc,
            )
        };
        
        Ok(self.wgpu_device.create_texture_from_hal(hal_texture))
    }
}
```

#### 4. Format Handling

**Supported Formats:**
- `kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange`: NV12 (8-bit)
- `kCVPixelFormatType_420YpCbCr10BiPlanarVideoRange`: P010 (10-bit)
- `kCVPixelFormatType_32BGRA`: BGRA (after conversion)

**Conversion Strategy:**
- Use Metal compute shader for YUV→RGB if needed
- Or use VideoToolbox's built-in conversion to BGRA

#### 5. Capability Detection

```rust
impl VideoToolboxCapabilities {
    fn query_codec_support(codec: Codec) -> Result<CodecSupport> {
        let codec_type = match codec {
            Codec::H264 => kCMVideoCodecType_H264,
            Codec::H265 => kCMVideoCodecType_HEVC,
            Codec::VP9 => kCMVideoCodecType_VP9,
            _ => return Ok(CodecSupport::NotSupported),
        };
        
        // Check if codec is supported
        let supported = unsafe {
            VTIsHardwareDecodeSupported(codec_type)
        };
        
        if supported {
            Ok(CodecSupport::HardwareAccelerated {
                profiles: query_supported_profiles(codec_type)?,
            })
        } else {
            Ok(CodecSupport::NotSupported)
        }
    }
}
```

### Error Handling

**Common Errors:**
- `kVTVideoDecoderBadDataErr`: Corrupted data
- `kVTVideoDecoderMalfunctionErr`: Hardware error
- No IOSurface: Pixel buffer not backed by IOSurface

### Testing Considerations

- Test on Intel Macs and Apple Silicon
- Test with different Metal feature sets
- Verify IOSurface backing
- Test HDR content support

---

## Vulkan Video

### Overview

Vulkan Video is a cross-platform video decode/encode extension for Vulkan. It's the future of hardware video acceleration on Vulkan-capable platforms.

### Architecture

```
VideoDecoder
    ↓
VulkanVideoDecoder (implements DecoderBackend)
    ↓
VkVideoSessionKHR + VkVideoSessionParametersKHR
    ↓
VkImage (decode output)
    ↓
wgpu::Texture (direct use, same Vulkan instance)
```

### Key Components

#### 1. Extension Requirements

**Required Extensions:**
- `VK_KHR_video_queue`
- `VK_KHR_video_decode_queue`
- Codec-specific: `VK_KHR_video_decode_h264`, `VK_KHR_video_decode_h265`, etc.

**Optional Extensions:**
- `VK_KHR_synchronization2` (recommended)
- `VK_KHR_video_decode_av1` (for AV1)

#### 2. Video Session Creation

**Steps:**
1. Query video capabilities
2. Find video queue family
3. Create video session
4. Allocate session parameters (SPS/PPS for H.264)
5. Allocate DPB (Decoded Picture Buffer) images
6. Allocate output images

**Implementation:**
```rust
use ash::vk;
use ash::extensions::khr::{VideoQueue, VideoDecodeQueue};

struct VulkanVideoDecoder {
    device: ash::Device,
    video_queue: vk::Queue,
    video_session: vk::VideoSessionKHR,
    session_parameters: vk::VideoSessionParametersKHR,
    dpb_images: Vec<vk::Image>,
    output_image_pool: Vec<vk::Image>,
    command_pool: vk::CommandPool,
}

impl VulkanVideoDecoder {
    fn initialize(
        instance: &ash::Instance,
        device: &ash::Device,
        physical_device: vk::PhysicalDevice,
        codec: Codec,
    ) -> Result<Self> {
        // 1. Query video capabilities
        let video_caps = Self::query_video_capabilities(
            instance,
            physical_device,
            codec,
        )?;
        
        // 2. Find video queue family
        let queue_family_index = Self::find_video_queue_family(
            instance,
            physical_device,
        )?;
        
        let video_queue = unsafe {
            device.get_device_queue(queue_family_index, 0)
        };
        
        // 3. Create video session
        let profile = Self::get_video_profile(codec);
        
        let session_create_info = vk::VideoSessionCreateInfoKHR {
            queue_family_index,
            video_profile: &profile,
            picture_format: vk::Format::G8_B8R8_2PLANE_420_UNORM,
            max_coded_extent: video_caps.max_coded_extent,
            reference_picture_format: vk::Format::G8_B8R8_2PLANE_420_UNORM,
            max_dpb_slots: video_caps.max_dpb_slots,
            max_active_reference_pictures: video_caps.max_active_reference_pictures,
            ..Default::default()
        };
        
        let video_session = unsafe {
            device.create_video_session_khr(&session_create_info, None)?
        };
        
        // 4. Allocate memory for session
        let memory_requirements = Self::get_session_memory_requirements(
            device,
            video_session,
        )?;
        
        Self::bind_session_memory(device, video_session, &memory_requirements)?;
        
        // 5. Create session parameters (for H.264: SPS/PPS)
        let session_parameters = Self::create_session_parameters(
            device,
            video_session,
            codec,
        )?;
        
        // 6. Allocate DPB images
        let dpb_images = Self::allocate_dpb_images(
            device,
            &video_caps,
            video_caps.max_dpb_slots as usize,
        )?;
        
        // 7. Allocate output image pool
        let output_image_pool = Self::allocate_output_images(
            device,
            &video_caps,
            4, // pool size
        )?;
        
        // 8. Create command pool for video queue
        let command_pool_info = vk::CommandPoolCreateInfo {
            queue_family_index,
            flags: vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER,
            ..Default::default()
        };
        
        let command_pool = unsafe {
            device.create_command_pool(&command_pool_info, None)?
        };
        
        Ok(Self {
            device: device.clone(),
            video_queue,
            video_session,
            session_parameters,
            dpb_images,
            output_image_pool,
            command_pool,
        })
    }
}
```

#### 3. Decoding Process

**Steps:**
1. Begin video coding scope
2. Bind video session and parameters
3. Decode frame to output image
4. End video coding scope
5. Transition image for wgpu use

**Implementation:**
```rust
impl VulkanVideoDecoder {
    fn decode_frame(&mut self, data: &[u8]) -> Result<DecodedFrame> {
        // 1. Get free output image
        let output_image = self.acquire_output_image()?;
        
        // 2. Parse bitstream (codec-specific)
        let decode_info = self.parse_frame_info(data)?;
        
        // 3. Allocate and record command buffer
        let cmd_buffer = self.allocate_command_buffer()?;
        
        unsafe {
            // Begin command buffer
            self.device.begin_command_buffer(
                cmd_buffer,
                &vk::CommandBufferBeginInfo::default(),
            )?;
            
            // Begin video coding scope
            let coding_control_info = vk::VideoCodingControlInfoKHR::default();
            let begin_info = vk::VideoBeginCodingInfoKHR {
                video_session: self.video_session,
                video_session_parameters: self.session_parameters,
                reference_slot_count: decode_info.reference_slots.len() as u32,
                p_reference_slots: decode_info.reference_slots.as_ptr(),
                ..Default::default()
            };
            
            self.device.cmd_begin_video_coding_khr(cmd_buffer, &begin_info);
            
            // Control coding (if needed)
            self.device.cmd_control_video_coding_khr(
                cmd_buffer,
                &coding_control_info,
            );
            
            // Decode operation
            let decode_info_vk = vk::VideoDecodeInfoKHR {
                src_buffer: decode_info.bitstream_buffer,
                src_buffer_offset: 0,
                src_buffer_range: data.len() as u64,
                dst_picture_resource: vk::VideoPictureResourceInfoKHR {
                    image_view_binding: output_image.view,
                    ..Default::default()
                },
                p_setup_reference_slot: decode_info.setup_reference_slot.as_ref(),
                reference_slot_count: decode_info.reference_slots.len() as u32,
                p_reference_slots: decode_info.reference_slots.as_ptr(),
                ..Default::default()
            };
            
            self.device.cmd_decode_video_khr(cmd_buffer, &decode_info_vk);
            
            // End video coding scope
            let end_info = vk::VideoEndCodingInfoKHR::default();
            self.device.cmd_end_video_coding_khr(cmd_buffer, &end_info);
            
            // Transition image for shader read
            let barrier = vk::ImageMemoryBarrier {
                src_access_mask: vk::AccessFlags::VIDEO_DECODE_WRITE_KHR,
                dst_access_mask: vk::AccessFlags::SHADER_READ,
                old_layout: vk::ImageLayout::VIDEO_DECODE_DST_KHR,
                new_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                image: output_image.image,
                subresource_range: vk::ImageSubresourceRange {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    base_mip_level: 0,
                    level_count: 1,
                    base_array_layer: 0,
                    layer_count: 1,
                },
                ..Default::default()
            };
            
            self.device.cmd_pipeline_barrier(
                cmd_buffer,
                vk::PipelineStageFlags::VIDEO_DECODE_KHR,
                vk::PipelineStageFlags::FRAGMENT_SHADER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[barrier],
            );
            
            // End command buffer
            self.device.end_command_buffer(cmd_buffer)?;
        }
        
        // 4. Submit to video queue
        let submit_info = vk::SubmitInfo {
            command_buffer_count: 1,
            p_command_buffers: &cmd_buffer,
            ..Default::default()
        };
        
        unsafe {
            self.device.queue_submit(
                self.video_queue,
                &[submit_info],
                vk::Fence::null(),
            )?;
        }
        
        // 5. Convert to wgpu texture
        let wgpu_texture = self.image_to_wgpu_texture(output_image.image)?;
        
        Ok(DecodedFrame::new(wgpu_texture))
    }
}
```

#### 4. DPB Management

**Decoded Picture Buffer (DPB):**
- Stores reference frames for inter-frame prediction
- Must maintain frame order and reference relationships
- Size depends on codec profile and level

**Implementation:**
```rust
struct DPBManager {
    slots: Vec<DPBSlot>,
    max_slots: usize,
}

struct DPBSlot {
    image: vk::Image,
    image_view: vk::ImageView,
    in_use: bool,
    frame_number: u64,
}

impl DPBManager {
    fn allocate_reference_slot(&mut self) -> Option<&mut DPBSlot> {
        self.slots.iter_mut().find(|slot| !slot.in_use)
    }
    
    fn mark_as_reference(&mut self, slot_index: usize, frame_number: u64) {
        self.slots[slot_index].in_use = true;
        self.slots[slot_index].frame_number = frame_number;
    }
    
    fn release_old_references(&mut self, current_frame: u64, max_age: u64) {
        for slot in &mut self.slots {
            if slot.in_use && current_frame - slot.frame_number > max_age {
                slot.in_use = false;
            }
        }
    }
}
```

#### 5. Capability Detection

```rust
impl VulkanVideoCapabilities {
    fn query_codec_support(
        instance: &ash::Instance,
        physical_device: vk::PhysicalDevice,
        codec: Codec,
    ) -> Result<CodecSupport> {
        let profile = Self::get_video_profile(codec);
        
        let mut capabilities = vk::VideoCapabilitiesKHR::default();
        let profile_info = vk::VideoProfileInfoKHR {
            video_codec_operation: profile.video_codec_operation,
            chroma_subsampling: profile.chroma_subsampling,
            luma_bit_depth: profile.luma_bit_depth,
            chroma_bit_depth: profile.chroma_bit_depth,
            ..Default::default()
        };
        
        unsafe {
            instance.get_physical_device_video_capabilities_khr(
                physical_device,
                &profile_info,
                &mut capabilities,
            )?;
        }
        
        if capabilities.flags.contains(vk::VideoCapabilityFlagsKHR::SEPARATE_REFERENCE_IMAGES) {
            Ok(CodecSupport::HardwareAccelerated {
                profiles: vec![profile],
            })
        } else {
            Ok(CodecSupport::NotSupported)
        }
    }
}
```

### Error Handling

**Common Errors:**
- Extension not available
- Codec not supported
- DPB exhausted
- Invalid bitstream

### Testing Considerations

- Test driver support (still emerging)
- Test on NVIDIA, AMD, Intel with Vulkan Video support
- Validate DPB management for complex sequences
- Test with different profiles and levels

---

## Backend Interface

### DecoderBackend Trait

All backends must implement this trait:

```rust
pub trait DecoderBackend: Send + Sync {
    /// Initialize the decoder with configuration
    fn initialize(&mut self, config: DecoderConfig) -> Result<()>;
    
    /// Decode a single frame
    fn decode_frame(&mut self, data: &[u8]) -> Result<DecodedFrame>;
    
    /// Flush any buffered frames
    fn flush(&mut self) -> Result<Vec<DecodedFrame>>;
    
    /// Reset decoder state
    fn reset(&mut self) -> Result<()>;
    
    /// Query backend capabilities
    fn capabilities(&self) -> &BackendCapabilities;
    
    /// Get backend type
    fn backend_type(&self) -> BackendType;
}

pub struct DecoderConfig {
    pub codec: Codec,
    pub resolution: Resolution,
    pub pixel_format: PixelFormat,
    pub color_space: ColorSpace,
    pub extra_data: Vec<u8>, // Codec-specific (SPS/PPS, etc.)
}

pub struct BackendCapabilities {
    pub supported_codecs: Vec<Codec>,
    pub supported_formats: Vec<PixelFormat>,
    pub max_resolution: Resolution,
    pub hardware_accelerated: bool,
}
```

---

## Texture Interoperability

### General Strategy

1. **Native decode to GPU texture**
2. **Export native handle** (DXGI handle, DMA-BUF FD, IOSurface, VkImage)
3. **Import into wgpu** using HAL external texture APIs
4. **Synchronize** GPU operations

### wgpu HAL Integration

```rust
// Generic pattern for texture import
fn import_native_texture_to_wgpu<A: hal::Api>(
    device: &wgpu::Device,
    native_handle: A::TextureHandle,
    desc: &TextureDescriptor,
) -> Result<wgpu::Texture> {
    // Get HAL device
    let hal_device = device.as_hal::<A>();
    
    // Create HAL texture from native handle
    let hal_texture = unsafe {
        hal_device.texture_from_raw(native_handle, desc, None)
    };
    
    // Wrap in wgpu texture
    Ok(device.create_texture_from_hal::<A>(hal_texture, desc))
}
```

### Platform-Specific Handles

```rust
pub enum NativeTextureHandle {
    D3D11(winapi::um::d3d11::ID3D11Texture2D),
    D3D12(winapi::um::d3d12::ID3D12Resource),
    Vulkan(ash::vk::Image),
    Metal(*mut metal::MTLTexture),
    DmaBuf(std::os::unix::io::RawFd),
}
```

---

## Testing Strategy

### Unit Tests

```rust
#[cfg(test)]
mod tests {
    #[test]
    fn test_backend_selection() {
        // Test backend selection logic
    }
    
    #[test]
    fn test_capability_detection() {
        // Test capability querying
    }
    
    #[cfg(target_os = "windows")]
    #[test]
    fn test_media_foundation_init() {
        // Test MF initialization
    }
}
```

### Integration Tests

```rust
#[test]
fn test_decode_h264_frame() {
    let device = create_test_wgpu_device();
    let decoder = VideoDecoderBuilder::new()
        .with_wgpu_device(&device)
        .with_codec(Codec::H264)
        .build()
        .unwrap();
    
    let test_frame = load_test_frame("test.h264");
    let decoded = decoder.decode_frame(&test_frame).unwrap();
    
    assert_eq!(decoded.texture().width(), 1920);
    assert_eq!(decoded.texture().height(), 1080);
}
```

### Platform-Specific CI

```yaml
# .github/workflows/ci.yml
name: CI

on: [push, pull_request]

jobs:
  test-windows:
    runs-on: windows-latest
    steps:
      - uses: actions/checkout@v2
      - run: cargo test --features media-foundation
  
  test-linux:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v2
      - run: cargo test --features vaapi
  
  test-macos:
    runs-on: macos-latest
    steps:
      - uses: actions/checkout@v2
      - run: cargo test --features videotoolbox
```

---

## Performance Optimization

### Key Metrics

- **Decode latency**: Time from input to texture ready
- **Throughput**: Frames per second
- **Memory usage**: Texture pool size
- **CPU usage**: Should be minimal for hardware decode

### Optimization Techniques

1. **Zero-copy paths**: Avoid CPU↔GPU transfers
2. **Texture pooling**: Reuse decode surfaces
3. **Async operations**: Overlap decode with rendering
4. **Batch processing**: Queue multiple frames
5. **Format matching**: Avoid unnecessary conversions

---

## Future Enhancements

- **Multi-threading**: Parallel decoding for multiple streams
- **Adaptive quality**: Dynamic resolution/bitrate
- **Hardware encoding**: Mirror architecture for encode
- **Post-processing**: Deinterlacing, scaling, filtering
- **Unified memory**: Explore unified memory architectures