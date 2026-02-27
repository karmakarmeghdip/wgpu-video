# In-depth libva Rust API guide (`cros-libva`) for this project

This guide is a deeper companion to `docs/libva-zero-copy-guide.md` and focuses on **how to use the Rust API precisely**, including:

- capability probing and what to do with returned `Vec`s,
- decode context/surface setup,
- the per-frame decode state machine,
- exporting decoded frames over time (`DMA-BUF`),
- practical pool/reuse patterns and failure handling.

Scope: Linux + VA-API decode path using the vendored `third_party/cros-libva` crate in this repository.

---

## 1) Mental model: key `cros-libva` objects and ownership

Think of the API as a strict graph:

1. `Display` (device + driver session)
2. `Config` (codec profile + entrypoint + attributes)
3. `Surface` pool (decode render targets)
4. `Context` (decode session bound to config + optional render targets)
5. `Picture` (one decode submission; typestate enforces `begin -> render -> end -> sync`)

Important ownership/lifetime notes:

- `Display` is `Rc`-owned and must outlive `Config`, `Context`, and `Surface`s.
- `Surface`s are values you usually keep in a pool (`VecDeque`/`Vec`).
- `Picture::new(..., surface)` *takes* a surface owner; reclaim via `take_surface()` after sync.
- `surface.export_prime()` transfers FD ownership into `OwnedFd` inside `DrmPrimeSurfaceDescriptor`.

---

## 2) Initialization in detail

## 2.1 Open display and confirm driver

```rust
use libva::Display;

let display = Display::open().ok_or("No VA-capable DRM render node found")?;
let vendor = display.query_vendor_string().unwrap_or_else(|_| "<unknown>".to_string());
```

`Display::open()` tries `/dev/dri/renderD128..` until one initializes. If you need deterministic adapter matching with Vulkan, prefer `Display::open_drm_display("/dev/dri/renderD128")`.

## 2.2 Interpret `query_config_profiles()` correctly

`display.query_config_profiles()?` returns `Vec<VAProfile::Type>`.

This is a **set of supported codec profiles**, not a single choice. You should:

1. Map your stream codec + profile to one or more candidate VA profiles in priority order.
2. Pick the first candidate present in the returned vector.
3. Fail with a clear error if none are present.

Example strategy:

```rust
use libva::bindings::VAProfile;

fn choose_h264_profile(supported: &[VAProfile::Type]) -> Option<VAProfile::Type> {
    let preferred = [
        VAProfile::VAProfileH264High,
        VAProfile::VAProfileH264Main,
        VAProfile::VAProfileH264ConstrainedBaseline,
        VAProfile::VAProfileH264Baseline,
    ];
    preferred.into_iter().find(|p| supported.contains(p))
}
```

Do not use `profiles[0]` in production logic.

## 2.3 Interpret `query_config_entrypoints(profile)` correctly

`display.query_config_entrypoints(profile)?` returns `Vec<VAEntrypoint::Type>` for that profile.

For decode, require `VAEntrypoint::VAEntrypointVLD`:

```rust
use libva::bindings::VAEntrypoint;

let entrypoints = display.query_config_entrypoints(profile)?;
if !entrypoints.contains(&VAEntrypoint::VAEntrypointVLD) {
    return Err("Profile exists but decode entrypoint VLD is missing".into());
}
let entrypoint = VAEntrypoint::VAEntrypointVLD;
```

## 2.4 Query config attributes (`get_config_attributes`) and parse values

This API writes values into the mutable slice you provide. You must initialize `type_` fields first.

Typical decode attribute request:

```rust
use libva::bindings::{self, VAConfigAttrib, VAConfigAttribType};

let mut attrs = vec![
    VAConfigAttrib { type_: VAConfigAttribType::VAConfigAttribRTFormat, value: 0 },
];

display.get_config_attributes(profile, entrypoint, &mut attrs)?;
```

How to interpret:

- `value == VA_ATTRIB_NOT_SUPPORTED`: attribute unsupported for this profile/entrypoint.
- Otherwise, `value` is usually a bitmask.

For `VAConfigAttribRTFormat`, check your requested format bit:

```rust
let rt_attr = attrs[0].value;
if rt_attr == bindings::VA_ATTRIB_NOT_SUPPORTED || (rt_attr & bindings::VA_RT_FORMAT_YUV420) == 0 {
    return Err("YUV420 render target format not supported".into());
}
```

## 2.5 Create `Config`

```rust
let config = display.create_config(attrs, profile, entrypoint)?;
```

The `Config` carries your decode contract. It must outlive the context using it.

## 2.6 Create decode surface pool (size + usage hints)

```rust
use libva::{UsageHint, bindings};

let width = coded_width;
let height = coded_height;
let pool_size = 12; // tune based on codec reorder depth + pipeline latency

let descriptors = vec![(); pool_size];
let surfaces = display.create_surfaces(
    bindings::VA_RT_FORMAT_YUV420,
    None, // optional explicit fourcc
    width,
    height,
    Some(UsageHint::USAGE_HINT_DECODER | UsageHint::USAGE_HINT_EXPORT),
    descriptors,
)?;
```

Guidance for pool size:

- H264 baseline streams: often 8–12 is safe.
- HEVC/long-GOP/reorder-heavy streams: increase (e.g. 16+).
- Too small pool causes stalls waiting for reusable surfaces.

## 2.7 Create decode context

```rust
let context = display.create_context(
    &config,
    coded_width,
    coded_height,
    Some(&surfaces),
    true, // progressive only
)?;
```

Use coded dimensions from bitstream headers (not display/crop dimensions).

---

## 3) Decode submission lifecycle over time

`cros-libva::Picture` uses typestate to enforce legal call order.

Per decoded access unit/frame:

1. Acquire a free `Surface` from your pool.
2. Build codec buffers (`BufferType::*`) for that frame.
3. `Picture::new(ts, context.clone(), surface)`
4. `add_buffer(...)` for each buffer.
5. `begin()? -> render()? -> end()? -> sync()?`
6. Reclaim surface (or hold for export/cache) with `take_surface()`.

Skeleton:

```rust
use libva::{BufferType, Picture};

let mut picture = Picture::new(frame_pts, context.clone(), surface);
for b in frame_buffers {
    picture.add_buffer(context.create_buffer(b)?);
}

let picture = picture.begin()?;
let picture = picture.render()?;
let picture = picture.end()?;
let picture = picture.sync().map_err(|(e, _unfinished)| e)?;

let surface = picture.take_surface().map_err(|_| "surface still aliased")?;
```

`sync()` blocks until decode completion for that surface. If you pipeline async work, use your own scheduling around surface readiness.

---

## 4) What buffers you actually submit (decode)

You provide codec-specific VA parameter buffers each frame/access unit.

For H264 decode this usually means:

- `BufferType::PictureParameter(PictureParameter::H264(...))`
- `BufferType::IQMatrix(IQMatrix::H264(...))` when needed
- `BufferType::SliceParameter(SliceParameter::H264(vec![...]))`
- `BufferType::SliceData(Vec<u8>)`

`cros-libva` gives the buffer wrappers and constructors, but you still own parser/state logic:

- SPS/PPS parsing and active parameter set tracking,
- DPB/reference list management,
- POC/reorder semantics,
- mapping NAL payload + offsets into slice parameter fields.

Rule of thumb: your parser emits a fully-populated “decode job” struct; libva submission just translates that struct into `BufferType`s.

---

## 5) Surface states and reuse policy

A practical pool model:

- **free**: available for new decode submit,
- **in_flight_decode**: submitted, waiting for `sync`,
- **ready_for_export**: decoded and can be exported/imported,
- **held_for_reference**: cannot recycle yet (codec DPB),
- **displayed_or_cached**: currently bound in renderer cache.

Do not immediately recycle surfaces that are still referenced by codec state or renderer cache.

---

## 6) Export decoded surface to DMA-BUF

After successful decode sync:

```rust
surface.sync()?; // safe even if already synced through Picture::sync
let prime = surface.export_prime()?;
```

`prime` contains:

- `fourcc`, `width`, `height`
- `objects: Vec<DrmPrimeSurfaceDescriptorObject>`
  - each object has `fd`, `size`, `drm_format_modifier`
- `layers: Vec<DrmPrimeSurfaceDescriptorLayer>`
  - each layer has `drm_format`, `num_planes`, `object_index`, `offset`, `pitch`

How to use this metadata:

1. For each plane, find backing object using `object_index[plane]`.
2. Use `offset[plane]` + `pitch[plane]` to define Vk plane layout.
3. Preserve modifier (`drm_format_modifier`) when creating/importing Vk image.

If you ignore modifier/plane layout, import may succeed but sampling is often corrupted.

---

## 7) Timing loop: “receive compressed data and return frames over time”

A robust player loop usually has three queues:

1. **compressed queue**: demux/parser output (access units)
2. **decode queue**: jobs waiting on free surfaces/context submission
3. **ready queue**: decoded/exported frames with PTS for render clock

Pseudo-flow per tick:

```text
tick(dt):
  - pull compressed AU(s) while input budget allows
  - if free surface exists: submit next AU to libva
  - move completed decode to ready queue (sync + export_prime)
  - pick frame for current media clock (drop/hold based on PTS)
  - import/schedule render
```

Design implications:

- Decode is not exactly “one input -> one output immediately”; B-frames reorder output.
- During startup/seek, you may queue several AUs before first presentable frame.
- End-of-stream needs explicit drain logic in parser + decoder state.

---

## 8) Error handling and recovery strategy

Map errors by stage so behavior is deterministic:

- capability/init (`open`, profile/entrypoint/attrs): fatal for this backend
- per-frame buffer/submission error: drop frame, maybe request keyframe/seek
- `sync` timeout/failure: reset context/pool if driver appears wedged
- `export_prime` failure: keep decode alive, fail interop path, optionally fallback

Useful APIs:

- `surface.query_status()` to inspect pending/completed state
- `surface.query_error()` for macroblock decode errors (`VADecode*`)

---

## 9) Minimal but correct decoder skeleton for your backend

```rust
struct LibvaDecoder {
    display: std::rc::Rc<libva::Display>,
    config: libva::Config,
    context: std::rc::Rc<libva::Context>,
    free_surfaces: std::collections::VecDeque<libva::Surface<()>>,
    held_surfaces: Vec<libva::Surface<()>>, // references/cache-owned
}
```

Initialization should:

1. open display,
2. choose profile from supported vector by codec-specific priority,
3. ensure `VAEntrypointVLD` is present,
4. query attrs and validate RT format,
5. create config/context/surface pool,
6. pre-warm queues.

Per frame should:

1. pop free surface,
2. build `BufferType`s from parsed AU,
3. submit through `Picture` typestate chain,
4. export PRIME,
5. hand off descriptor + surface identity to importer/cache,
6. recycle only when no longer referenced.

---

## 10) Practical checklist for your current repo

- Replace the `profiles[0]` placeholder in `src/backend/libva/decoder.rs` with deterministic profile selection.
- Validate `VAEntrypointVLD` explicitly before config creation.
- Validate `VAConfigAttribRTFormat` mask before creating surfaces.
- Add a real surface pool and reclaim path (`Picture::take_surface`).
- Add explicit per-frame job structs (parsed AU -> libva buffers -> decoded surface).
- Add telemetry logs: chosen profile, entrypoint, RT format mask, pool size, export descriptor summary.

---

## 11) Reference locations in vendored crate

- `third_party/cros-libva/lib/src/display.rs`
  - profile/entrypoint/config/surface/context creation functions
- `third_party/cros-libva/lib/src/picture.rs`
  - typestate decode order and `take_surface`
- `third_party/cros-libva/lib/src/surface.rs`
  - `sync`, `query_status`, `query_error`, `export_prime`
- `third_party/cros-libva/lib/src/buffer.rs`
  - `BufferType` enum and buffer wrappers
- `third_party/cros-libva/lib/src/lib.rs` tests
  - end-to-end decode submission example (`libva_utils_mpeg2vldemo`)
