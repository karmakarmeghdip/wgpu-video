# libva + wgpu zero-copy guide (Linux)

This document is a practical implementation guide for building a `VideoPlayer` like your `src/lib.rs` API, using:

- `cros-libva` for hardware decode
- DMA-BUF export from VA surfaces
- Vulkan + `wgpu` interop for rendering in an existing UI `wgpu::Device` (egui/iced)

The goal is to help you implement it yourself with the right function-level references and architecture.

---

## 1) What “zero-copy” means here

For this guide, “zero-copy” means:

1. Decode on GPU via VA-API (`libva`) into VA surfaces.
2. Export those surfaces as DMA-BUF FDs.
3. Import DMA-BUF into Vulkan image(s) that your existing `wgpu` device can sample.
4. Convert NV12/P010 -> RGBA in shader and render.

No CPU pixel roundtrip (`vaDeriveImage` / readback / upload) in the steady-state render path.

---

## 2) Prerequisites and constraints

- Linux only.
- `wgpu` backend must be Vulkan for this path.
- VA display GPU and Vulkan adapter must be the same physical GPU (or import may fail / stall / copy).
- Drivers must support required extensions and modifiers.

System packages typically needed:

- `libva-dev`, `libva-utils`
- Vendor VA driver (`intel-media-driver` or Mesa VA stack)
- Vulkan loader + vendor Vulkan driver

Quick sanity checks:

- `vainfo` works and shows your decode profile as `VAEntrypointVLD`.
- A Vulkan app on the same machine uses the same GPU (vendor + PCI ID).

---

## 3) API shape mapping to your `VideoPlayer`

Your existing API:

- `new(device, queue, source, config)`
- `tick(delta)`
- `texture_view()`
- `dimensions()`
- playback controls

Recommended internal split:

- `Demuxer` (container to Annex-B NAL units / access units)
- `LibvaDecoder` (VA config/context/surfaces + decode submission)
- `DmabufImporter` (VA export -> Vulkan image -> wgpu texture)
- `ColorConverter` (NV12/P010 to target RGBA texture/view)

`VideoPlayer::tick()` calls:

1. Read compressed data from source
2. Feed decoder
3. Wait/sync decoded surface
4. Export DMA-BUF
5. Import/update GPU texture
6. Run color conversion pass
7. Publish current `TextureView`

---

## 4) libva decode pipeline (with `cros-libva` functions)

### 4.1 Open display

Use:

- `Display::open()` (first working DRM render node)
- or `Display::open_drm_display("/dev/dri/renderD128")` for explicit control

Then query capabilities:

- `display.query_config_profiles()`
- `display.query_config_entrypoints(profile)`
- `display.get_config_attributes(profile, entrypoint, attrs)`

### 4.2 Create decode config and context

Typical decode values:

- entrypoint: `VAEntrypoint::VAEntrypointVLD`
- profile: codec-specific (H264/H265/etc)
- RT format: usually `VA_RT_FORMAT_YUV420`

Use:

- `display.create_config(attrs, profile, entrypoint)`
- `display.create_surfaces(rt_format, va_fourcc, width, height, usage_hint, descriptors)`
  - For decoder-allocated surfaces: descriptor `vec![(); N]`
  - Use `Some(UsageHint::USAGE_HINT_DECODER | UsageHint::USAGE_HINT_EXPORT)`
- `display.create_context(&config, coded_width, coded_height, Some(&surfaces), progressive)`

### 4.3 Submit decode work per frame/access-unit

In `cros-libva`, decode submission follows `Picture` state transitions:

- `Picture::new(timestamp, context, surface)`
- add codec buffers: `picture.add_buffer(context.create_buffer(BufferType::... )?)`
- `picture.begin()?`
- `picture.render()?`
- `picture.end()?`
- `picture.sync()`

Important:

- You provide codec-specific parameter/slice buffers (`buffer/*` modules).
- Keep reference surfaces and DPB handling in your decoder state for inter-frames.

### 4.4 Reclaim / reuse surfaces

After sync and export, recycle surfaces via your own pool strategy.
`Picture::take_surface()` can help when surface ownership is wrapped.

---

## 5) Export decoded VA surface as DMA-BUF

From a decoded `Surface`:

- `surface.sync()?` (or ensure sync already happened)
- `let prime = surface.export_prime()?`

`prime` (`DrmPrimeSurfaceDescriptor`) contains:

- `fourcc`, `width`, `height`
- `objects[]`:
  - `fd` (`OwnedFd`)
  - `size`
  - `drm_format_modifier`
- `layers[]`:
  - `drm_format`
  - `num_planes`
  - `object_index[]`
  - `offset[]`
  - `pitch[]`

This is the exact metadata you need for Vulkan image import.

---

## 6) Import DMA-BUF into Vulkan (core steps)

This is the critical part.

### 6.1 Required Vulkan capabilities

At minimum, check availability for:

- `VK_EXT_external_memory_dma_buf`
- `VK_EXT_image_drm_format_modifier`
- `VK_KHR_external_memory_fd`
- plus normal external-memory/image features from your driver

### 6.2 Create Vulkan image compatible with exported planes/modifier

When descriptor provides a DRM modifier:

- Build `VkImageCreateInfo` with:
  - external handle type = `DMA_BUF_EXT`
  - format matching exported surface (often multi-planar YUV)
  - tiling + modifier struct (`VkImageDrmFormatModifierExplicitCreateInfoEXT`)
  - explicit plane layout from `offset[]` and `pitch[]`

For multi-planar disjoint formats, include disjoint image flags and bind per plane if required by your format path.

### 6.3 Import FD as external memory

Per imported object FD:

- `vkAllocateMemory` chained with `VkImportMemoryFdInfoKHR`
- choose compatible memory type
- `vkBindImageMemory` (or `vkBindImageMemory2` for disjoint/plane cases)

Ownership notes:

- `vkAllocateMemory` import usually consumes FD ownership semantics per Vulkan spec path.
- Keep lifetime model explicit in your importer cache.

### 6.4 Synchronization and queue ownership

Before sampling in your graphics queue:

- Ensure producer (VA) finished: `vaSyncSurface` already done.
- Perform required Vulkan image layout transition and queue-family ownership transfer if needed
  (external queue family -> graphics queue family).

If this step is wrong, you get flicker, stale frames, or validation errors.

---

## 7) Wrap imported Vulkan image as `wgpu::Texture`

There are two practical routes.

### Route A (recommended for control): `wgpu-hal` interop

Using unsafe HAL interop (native, Vulkan backend):

1. Access HAL device from your existing `wgpu::Device`:
   - `unsafe { device.as_hal::<wgpu::hal::api::Vulkan>() }`
2. Build HAL/Vulkan texture wrapper from raw `VkImage`:
   - `wgpu_hal::vulkan::Device::texture_from_raw(...)`
   - memory mode should reflect external memory ownership (`TextureMemory::External`)
3. Promote HAL texture to `wgpu::Texture`:
   - `unsafe { device.create_texture_from_hal::<wgpu::hal::api::Vulkan>(...) }`
4. Create plane views (`TextureAspect::Plane0` / `Plane1`) and sample in shader.

Notes:

- This path is advanced and unsafe.
- Gate this code behind a backend module and isolate unsafe invariants.

### Route B: import directly in your own Vulkan renderer path

If you already own Vulkan rendering path outside `wgpu`, you can sample imported images directly in Vulkan.
For your `wgpu`-centric API, Route A usually matches better.

---

## 8) NV12/P010 sampling and conversion

Once texture is visible in `wgpu`:

- Keep texture format multi-planar (`NV12`/`P010` equivalent path).
- Create plane views:
  - Y plane as `R8Unorm` (or `R16Unorm` for P010)
  - UV plane as `Rg8Unorm` (or `Rg16Unorm` for P010)
- Shader converts YUV -> linear RGB -> target format.

This is the same pattern already used in your example shader logic.

---

## 9) Suggested `tick()` flow (pseudo-code)

```rust
fn tick(&mut self, dt: Duration) -> Result<(), PlayerError> {
    if !self.playing { return Ok(()); }

    let packet_or_au = self.demux.next()?;
    let decoded_surface = self.decoder.decode_one(packet_or_au)?; // Picture begin/render/end/sync

    let prime = decoded_surface.export_prime()?;                  // DMA-BUF descriptors

    let nv12_texture = self.importer.import_or_reuse(&prime)?;    // Vulkan ext memory + wgpu-hal wrap
    let rgba_view = self.converter.convert_to_rgba(&nv12_texture, self.config.target_format)?;

    self.current_view = Some(rgba_view);
    self.position += dt;
    Ok(())
}
```

---

## 10) Caching strategy (important)

Do not import fresh every frame blindly. Cache by stable surface identity (or FD/modifier tuple) and recycle.

Recommended cache keys:

- surface slot index (if stable pool)
- OR tuple of `(fd inode-ish identity if available, modifier, width, height, drm_format)`

Cache contents:

- Vulkan image handle
- imported memory handles
- wrapped `wgpu::Texture`
- plane `TextureView`s

---

## 11) Error handling checklist

Convert these to your `PlayerError` variants:

- VA init/config/context errors -> `DecoderError`
- decode submission/sync failure -> `DecoderError`
- `export_prime` failure -> `WgpuInteropError`
- Vulkan import/memory bind/layout transition failure -> `WgpuInteropError`
- shader conversion failures -> `WgpuInteropError`
- demux/read failures -> `IoError` / `DemuxError`

---

## 12) Minimal bring-up plan

1. Implement decode-only (no rendering): decode + `export_prime` log descriptors.
2. Validate one imported frame path into Vulkan image.
3. Wrap to `wgpu::Texture` and create plane views.
4. Add YUV->RGB shader and render quad.
5. Add cache + steady playback.
6. Add seek/loop/clock control.

---

## 13) Practical pitfalls

- Mismatched GPU between VA and Vulkan adapter.
- Ignoring DRM modifiers / plane layouts.
- Missing external-memory synchronization.
- Wrong multi-planar format/aspect handling in `wgpu` views.
- Re-importing every frame causing stalls/leaks.

---

## 14) Quick reference: `cros-libva` calls you will use

- Display:
  - `Display::open` / `Display::open_drm_display`
  - `query_config_profiles`, `query_config_entrypoints`, `get_config_attributes`
  - `create_config`, `create_surfaces`, `create_context`
- Decode submission:
  - `Context::create_buffer`
  - `Picture::new`, `add_buffer`, `begin`, `render`, `end`, `sync`
- Surface:
  - `Surface::sync`
  - `Surface::export_prime`

---

## 15) Scope note

`cros-libva` gives you VA-API wrappers and codec buffer types, but it is not a full media framework.
You still own:

- demuxing/container parsing
- codec bitstream parsing/state (especially H264/HEVC references)
- scheduling/frame timing/reorder/seek logic

If you want to keep implementation manageable, start with Annex-B H264 elementary stream first, then add container + richer codecs.

---

## 16) Where to integrate in your crate

Suggested modules:

- `src/backend/libva/mod.rs`
- `src/backend/libva/decoder.rs`
- `src/backend/libva/dmabuf_export.rs`
- `src/backend/libva/vulkan_import.rs`
- `src/backend/libva/color_convert.rs`

Keep `src/lib.rs` API surface unchanged and route internals through this backend.

