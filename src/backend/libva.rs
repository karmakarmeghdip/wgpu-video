use std::cell::RefCell;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{anyhow, bail, Context};
use codecs::backend::vaapi::decoder::VaapiBackend as CodecVaapiBackend;
use codecs::backend::vaapi::decoder::VaapiDecodedHandle;
use codecs::decoder::stateless::h264::H264;
use codecs::decoder::stateless::{DecodeError, StatelessDecoder, StatelessVideoDecoder};
use codecs::decoder::{DecodedHandle, DecoderEvent, StreamInfo};
use codecs::video_frame::frame_pool::{FramePool, PooledVideoFrame};
use codecs::video_frame::VideoFrame;
use codecs::{BlockingMode, Fourcc, Resolution};
use libva::{Display, Surface, UsageHint};

use crate::demuxer::{sample_presentation_timestamp, Demuxer, H264TrackConfig};

const ANNEX_B_START_CODE: [u8; 4] = [0, 0, 0, 1];
const DEFAULT_RENDER_NODE: &str = "/dev/dri/renderD128";
static LOGGED_RGBA_EXPORT: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone)]
pub struct ExportedDmabufObject {
    pub fd: i32,
    pub size: u32,
    pub drm_format_modifier: u64,
}

#[derive(Debug, Clone)]
pub struct ExportedDmabufLayer {
    pub drm_format: u32,
    pub num_planes: u32,
    pub object_index: [u8; 4],
    pub offset: [u32; 4],
    pub pitch: [u32; 4],
}

#[derive(Debug, Clone)]
pub struct ExportedDmabufFrame {
    pub timestamp: u64,
    pub coded_resolution: (u32, u32),
    pub display_resolution: (u32, u32),
    pub drm_fourcc: u32,
    pub width: u32,
    pub height: u32,
    pub plane_pitches: Vec<usize>,
    pub plane_sizes: Vec<usize>,
    pub y_plane_preview: Vec<u8>,
    pub objects: Vec<ExportedDmabufObject>,
    pub layers: Vec<ExportedDmabufLayer>,
}

#[derive(Debug, Clone)]
pub struct DecodeReport {
    pub track_id: u32,
    pub timescale: u32,
    pub packets_decoded: usize,
    pub frames_decoded: usize,
    pub exported_frames: Vec<ExportedDmabufFrame>,
}

#[derive(Debug, Clone, Copy)]
pub struct PrimeFrameMetadata {
    pub timestamp: u64,
    pub coded_resolution: (u32, u32),
    pub display_resolution: (u32, u32),
}

pub struct PrimeDmabufFrame {
    pub metadata: PrimeFrameMetadata,
    pub descriptor: libva::DrmPrimeSurfaceDescriptor,
}

#[derive(Debug, Clone)]
pub struct CpuNv12Frame {
    pub metadata: PrimeFrameMetadata,
    pub width: u32,
    pub height: u32,
    pub y_stride: u32,
    pub uv_stride: u32,
    pub y_plane: Vec<u8>,
    pub uv_plane: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct CpuRgbaFrame {
    pub metadata: PrimeFrameMetadata,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub format: CpuRgbaFormat,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CpuRgbaFormat {
    Rgba,
    Bgra,
}

type DecoderFrame = PooledVideoFrame<NativeVaFrame>;
type H264Decoder = StatelessDecoder<H264, CodecVaapiBackend<DecoderFrame>>;

#[derive(Debug)]
struct NativeVaFrame {
    coded_resolution: Resolution,
    display_resolution: Resolution,
}

impl VideoFrame for NativeVaFrame {
    #[cfg(target_os = "linux")]
    type MemDescriptor = ();
    #[cfg(target_os = "linux")]
    type NativeHandle = Surface<()>;

    fn fourcc(&self) -> Fourcc {
        Fourcc::from(b"NV12")
    }

    fn resolution(&self) -> Resolution {
        self.display_resolution
    }

    fn get_plane_size(&self) -> Vec<usize> {
        let coded_width = self.coded_resolution.width as usize;
        let coded_height = self.coded_resolution.height as usize;
        vec![coded_width * coded_height, coded_width * coded_height / 2]
    }

    fn get_plane_pitch(&self) -> Vec<usize> {
        let coded_width = self.coded_resolution.width as usize;
        vec![coded_width, coded_width]
    }

    fn map<'a>(&'a self) -> Result<Box<dyn codecs::video_frame::ReadMapping<'a> + 'a>, String> {
        Err("CPU mapping is not supported for native VA decoder frames".to_string())
    }

    fn map_mut<'a>(
        &'a mut self,
    ) -> Result<Box<dyn codecs::video_frame::WriteMapping<'a> + 'a>, String> {
        Err("CPU mapping is not supported for native VA decoder frames".to_string())
    }

    fn to_native_handle(&self, display: &Rc<Display>) -> Result<Self::NativeHandle, String> {
        let mut surfaces = display
            .create_surfaces(
                libva::VA_RT_FORMAT_YUV420,
                Some(libva::VA_FOURCC_NV12),
                self.coded_resolution.width,
                self.coded_resolution.height,
                Some(UsageHint::USAGE_HINT_DECODER | UsageHint::USAGE_HINT_EXPORT),
                vec![()],
            )
            .map_err(|err| format!("Failed to create native VA surface: {err}"))?;
        surfaces
            .pop()
            .ok_or_else(|| "VA surface allocation returned no surfaces".to_string())
    }
}

pub struct VaapiBackend {
    display: Rc<Display>,
    decoder: H264Decoder,
    framepool: FramePool<NativeVaFrame>,
    parameter_sets_sent: bool,
}

impl VaapiBackend {
    pub fn new() -> anyhow::Result<Self> {
        Self::new_with_render_node(Path::new(DEFAULT_RENDER_NODE))
    }

    pub fn new_with_render_node(render_node: &Path) -> anyhow::Result<Self> {
        let display = Display::open_drm_display(render_node)
            .with_context(|| format!("Failed to open VA display on {}", render_node.display()))?;
        let decoder =
            StatelessDecoder::<H264, _>::new_vaapi(display.clone(), BlockingMode::Blocking)
                .map_err(|err| anyhow!("Failed to create VA-API H.264 decoder: {err:?}"))?;
        let framepool = FramePool::new(move |stream_info: &StreamInfo| NativeVaFrame {
            coded_resolution: stream_info.coded_resolution,
            display_resolution: stream_info.display_resolution,
        });

        Ok(Self {
            display,
            decoder,
            framepool,
            parameter_sets_sent: false,
        })
    }

    pub fn decode_h264_mp4_track(
        &mut self,
        demuxer: &mut Demuxer,
        track_id: u32,
        export_frame_limit: usize,
    ) -> anyhow::Result<DecodeReport> {
        let config = demuxer.get_h264_track_config(track_id)?;
        let sample_count = demuxer.sample_count(track_id)?;
        let mut report = DecodeReport {
            track_id,
            timescale: config.timescale,
            packets_decoded: 0,
            frames_decoded: 0,
            exported_frames: Vec::with_capacity(export_frame_limit.min(16)),
        };

        for sample_id in 1..=sample_count {
            let Some(sample) = demuxer.read_sample(track_id, sample_id)? else {
                continue;
            };
            let annex_b = sample_to_annex_b(
                &config,
                &sample.bytes,
                sample.is_sync,
                &mut self.parameter_sets_sent,
            )
            .with_context(|| format!("Failed to convert sample {sample_id} to Annex-B"))?;
            self.decode_annex_b_packet(
                sample.start_time,
                &annex_b,
                export_frame_limit,
                &mut report,
            )
            .with_context(|| format!("Failed to decode sample {sample_id}"))?;
            report.packets_decoded += 1;
        }

        self.decoder
            .flush()
            .map_err(|err| anyhow!("Failed to flush decoder: {err:?}"))?;
        self.handle_decoder_events(export_frame_limit, &mut report)?;

        if report.frames_decoded == 0 {
            bail!("Decoder finished without producing any frame")
        }

        Ok(report)
    }

    pub fn decode_h264_mp4_track_with_prime_frames<F>(
        &mut self,
        demuxer: &mut Demuxer,
        track_id: u32,
        mut on_frame: F,
    ) -> anyhow::Result<DecodeReport>
    where
        F: FnMut(PrimeDmabufFrame) -> anyhow::Result<()>,
    {
        let config = demuxer.get_h264_track_config(track_id)?;
        let sample_count = demuxer.sample_count(track_id)?;
        let mut report = DecodeReport {
            track_id,
            timescale: config.timescale,
            packets_decoded: 0,
            frames_decoded: 0,
            exported_frames: Vec::new(),
        };

        for sample_id in 1..=sample_count {
            let Some(sample) = demuxer.read_sample(track_id, sample_id)? else {
                continue;
            };
            let annex_b = sample_to_annex_b(
                &config,
                &sample.bytes,
                sample.is_sync,
                &mut self.parameter_sets_sent,
            )
            .with_context(|| format!("Failed to convert sample {sample_id} to Annex-B"))?;
            self.decode_annex_b_packet_with_callback(
                sample_presentation_timestamp(&sample),
                &annex_b,
                &mut report,
                &mut on_frame,
            )
            .with_context(|| format!("Failed to decode sample {sample_id}"))?;
            report.packets_decoded += 1;
        }

        self.decoder
            .flush()
            .map_err(|err| anyhow!("Failed to flush decoder: {err:?}"))?;
        self.handle_decoder_events_with_callback(&mut report, &mut on_frame)?;

        if report.frames_decoded == 0 {
            bail!("Decoder finished without producing any frame")
        }

        Ok(report)
    }

    pub fn decode_h264_mp4_track_with_cpu_frames<F>(
        &mut self,
        demuxer: &mut Demuxer,
        track_id: u32,
        mut on_frame: F,
    ) -> anyhow::Result<DecodeReport>
    where
        F: FnMut(CpuNv12Frame) -> anyhow::Result<()>,
    {
        let config = demuxer.get_h264_track_config(track_id)?;
        let sample_count = demuxer.sample_count(track_id)?;
        let mut report = DecodeReport {
            track_id,
            timescale: config.timescale,
            packets_decoded: 0,
            frames_decoded: 0,
            exported_frames: Vec::new(),
        };

        for sample_id in 1..=sample_count {
            let Some(sample) = demuxer.read_sample(track_id, sample_id)? else {
                continue;
            };
            let annex_b = sample_to_annex_b(
                &config,
                &sample.bytes,
                sample.is_sync,
                &mut self.parameter_sets_sent,
            )
            .with_context(|| format!("Failed to convert sample {sample_id} to Annex-B"))?;
            self.decode_annex_b_packet_with_cpu_callback(
                sample.start_time,
                &annex_b,
                &mut report,
                &mut on_frame,
            )
            .with_context(|| format!("Failed to decode sample {sample_id}"))?;
            report.packets_decoded += 1;
        }

        self.decoder
            .flush()
            .map_err(|err| anyhow!("Failed to flush decoder: {err:?}"))?;
        self.handle_decoder_events_with_cpu_callback(&mut report, &mut on_frame)?;

        if report.frames_decoded == 0 {
            bail!("Decoder finished without producing any frame")
        }

        Ok(report)
    }

    pub fn decode_h264_mp4_track_with_rgba_frames<F>(
        &mut self,
        demuxer: &mut Demuxer,
        track_id: u32,
        mut on_frame: F,
    ) -> anyhow::Result<DecodeReport>
    where
        F: FnMut(CpuRgbaFrame) -> anyhow::Result<()>,
    {
        let config = demuxer.get_h264_track_config(track_id)?;
        let sample_count = demuxer.sample_count(track_id)?;
        let mut report = DecodeReport {
            track_id,
            timescale: config.timescale,
            packets_decoded: 0,
            frames_decoded: 0,
            exported_frames: Vec::new(),
        };

        for sample_id in 1..=sample_count {
            let Some(sample) = demuxer.read_sample(track_id, sample_id)? else {
                continue;
            };
            let annex_b = sample_to_annex_b(
                &config,
                &sample.bytes,
                sample.is_sync,
                &mut self.parameter_sets_sent,
            )
            .with_context(|| format!("Failed to convert sample {sample_id} to Annex-B"))?;
            self.decode_annex_b_packet_with_rgba_callback(
                sample.start_time,
                &annex_b,
                &mut report,
                &mut on_frame,
            )
            .with_context(|| format!("Failed to decode sample {sample_id}"))?;
            report.packets_decoded += 1;
        }

        self.decoder
            .flush()
            .map_err(|err| anyhow!("Failed to flush decoder: {err:?}"))?;
        self.handle_decoder_events_with_rgba_callback(&mut report, &mut on_frame)?;

        if report.frames_decoded == 0 {
            bail!("Decoder finished without producing any frame")
        }

        Ok(report)
    }

    fn decode_annex_b_packet(
        &mut self,
        timestamp: u64,
        packet: &[u8],
        export_frame_limit: usize,
        report: &mut DecodeReport,
    ) -> anyhow::Result<()> {
        let mut remaining = packet;

        while !remaining.is_empty() {
            let decode_result = {
                let decoder = &mut self.decoder;
                let framepool = &mut self.framepool;
                let mut alloc_cb = || framepool.alloc();
                decoder.decode(timestamp, remaining, &mut alloc_cb)
            };

            match decode_result {
                Ok(consumed) => {
                    if consumed == 0 {
                        bail!("Decoder consumed 0 bytes from a non-empty packet")
                    }
                    remaining = &remaining[consumed..];
                }
                Err(DecodeError::CheckEvents) => {
                    self.handle_decoder_events(export_frame_limit, report)?;
                }
                Err(DecodeError::NotEnoughOutputBuffers(_)) => {
                    let drained = self.handle_decoder_events(export_frame_limit, report)?;
                    if drained == 0 {
                        bail!("Decoder ran out of output buffers and no frame became available")
                    }
                }
                Err(err) => return Err(anyhow!("Decoder error: {err:?}")),
            }
        }

        self.handle_decoder_events(export_frame_limit, report)?;
        Ok(())
    }

    fn decode_annex_b_packet_with_callback<F>(
        &mut self,
        timestamp: u64,
        packet: &[u8],
        report: &mut DecodeReport,
        on_frame: &mut F,
    ) -> anyhow::Result<()>
    where
        F: FnMut(PrimeDmabufFrame) -> anyhow::Result<()>,
    {
        let mut remaining = packet;

        while !remaining.is_empty() {
            let decode_result = {
                let decoder = &mut self.decoder;
                let framepool = &mut self.framepool;
                let mut alloc_cb = || framepool.alloc();
                decoder.decode(timestamp, remaining, &mut alloc_cb)
            };

            match decode_result {
                Ok(consumed) => {
                    if consumed == 0 {
                        bail!("Decoder consumed 0 bytes from a non-empty packet")
                    }
                    remaining = &remaining[consumed..];
                }
                Err(DecodeError::CheckEvents) => {
                    self.handle_decoder_events_with_callback(report, on_frame)?;
                }
                Err(DecodeError::NotEnoughOutputBuffers(_)) => {
                    let drained = self.handle_decoder_events_with_callback(report, on_frame)?;
                    if drained == 0 {
                        bail!("Decoder ran out of output buffers and no frame became available")
                    }
                }
                Err(err) => return Err(anyhow!("Decoder error: {err:?}")),
            }
        }

        self.handle_decoder_events_with_callback(report, on_frame)?;
        Ok(())
    }

    fn decode_annex_b_packet_with_cpu_callback<F>(
        &mut self,
        timestamp: u64,
        packet: &[u8],
        report: &mut DecodeReport,
        on_frame: &mut F,
    ) -> anyhow::Result<()>
    where
        F: FnMut(CpuNv12Frame) -> anyhow::Result<()>,
    {
        let mut remaining = packet;

        while !remaining.is_empty() {
            let decode_result = {
                let decoder = &mut self.decoder;
                let framepool = &mut self.framepool;
                let mut alloc_cb = || framepool.alloc();
                decoder.decode(timestamp, remaining, &mut alloc_cb)
            };

            match decode_result {
                Ok(consumed) => {
                    if consumed == 0 {
                        bail!("Decoder consumed 0 bytes from a non-empty packet")
                    }
                    remaining = &remaining[consumed..];
                }
                Err(DecodeError::CheckEvents) => {
                    self.handle_decoder_events_with_cpu_callback(report, on_frame)?;
                }
                Err(DecodeError::NotEnoughOutputBuffers(_)) => {
                    let drained = self.handle_decoder_events_with_cpu_callback(report, on_frame)?;
                    if drained == 0 {
                        bail!("Decoder ran out of output buffers and no frame became available")
                    }
                }
                Err(err) => return Err(anyhow!("Decoder error: {err:?}")),
            }
        }

        self.handle_decoder_events_with_cpu_callback(report, on_frame)?;
        Ok(())
    }

    fn decode_annex_b_packet_with_rgba_callback<F>(
        &mut self,
        timestamp: u64,
        packet: &[u8],
        report: &mut DecodeReport,
        on_frame: &mut F,
    ) -> anyhow::Result<()>
    where
        F: FnMut(CpuRgbaFrame) -> anyhow::Result<()>,
    {
        let mut remaining = packet;

        while !remaining.is_empty() {
            let decode_result = {
                let decoder = &mut self.decoder;
                let framepool = &mut self.framepool;
                let mut alloc_cb = || framepool.alloc();
                decoder.decode(timestamp, remaining, &mut alloc_cb)
            };

            match decode_result {
                Ok(consumed) => {
                    if consumed == 0 {
                        bail!("Decoder consumed 0 bytes from a non-empty packet")
                    }
                    remaining = &remaining[consumed..];
                }
                Err(DecodeError::CheckEvents) => {
                    self.handle_decoder_events_with_rgba_callback(report, on_frame)?;
                }
                Err(DecodeError::NotEnoughOutputBuffers(_)) => {
                    let drained =
                        self.handle_decoder_events_with_rgba_callback(report, on_frame)?;
                    if drained == 0 {
                        bail!("Decoder ran out of output buffers and no frame became available")
                    }
                }
                Err(err) => return Err(anyhow!("Decoder error: {err:?}")),
            }
        }

        self.handle_decoder_events_with_rgba_callback(report, on_frame)?;
        Ok(())
    }

    fn handle_decoder_events(
        &mut self,
        export_frame_limit: usize,
        report: &mut DecodeReport,
    ) -> anyhow::Result<usize> {
        let mut handled = 0;
        while let Some(event) = self.decoder.next_event() {
            match event {
                DecoderEvent::FormatChanged => {
                    let stream_info = self.decoder.stream_info().cloned().ok_or(anyhow!(
                        "Decoder reported a format change without stream info"
                    ))?;
                    self.framepool.resize(&stream_info);
                }
                DecoderEvent::FrameReady(handle) => {
                    handle
                        .sync()
                        .context("Failed to synchronize decoded frame")?;
                    report.frames_decoded += 1;
                    if report.exported_frames.len() < export_frame_limit {
                        let summary = self.export_frame(&handle)?;
                        report.exported_frames.push(summary);
                    }
                    handled += 1;
                }
            }
        }
        Ok(handled)
    }

    fn handle_decoder_events_with_callback<F>(
        &mut self,
        report: &mut DecodeReport,
        on_frame: &mut F,
    ) -> anyhow::Result<usize>
    where
        F: FnMut(PrimeDmabufFrame) -> anyhow::Result<()>,
    {
        let mut handled = 0;
        while let Some(event) = self.decoder.next_event() {
            match event {
                DecoderEvent::FormatChanged => {
                    let stream_info = self.decoder.stream_info().cloned().ok_or(anyhow!(
                        "Decoder reported a format change without stream info"
                    ))?;
                    self.framepool.resize(&stream_info);
                }
                DecoderEvent::FrameReady(handle) => {
                    handle
                        .sync()
                        .context("Failed to synchronize decoded frame")?;
                    report.frames_decoded += 1;
                    let prime_frame = self.export_prime_frame(&handle)?;
                    on_frame(prime_frame)?;
                    handled += 1;
                }
            }
        }
        Ok(handled)
    }

    fn handle_decoder_events_with_cpu_callback<F>(
        &mut self,
        report: &mut DecodeReport,
        on_frame: &mut F,
    ) -> anyhow::Result<usize>
    where
        F: FnMut(CpuNv12Frame) -> anyhow::Result<()>,
    {
        let mut handled = 0;
        while let Some(event) = self.decoder.next_event() {
            match event {
                DecoderEvent::FormatChanged => {
                    let stream_info = self.decoder.stream_info().cloned().ok_or(anyhow!(
                        "Decoder reported a format change without stream info"
                    ))?;
                    self.framepool.resize(&stream_info);
                }
                DecoderEvent::FrameReady(handle) => {
                    handle
                        .sync()
                        .context("Failed to synchronize decoded frame")?;
                    report.frames_decoded += 1;
                    let cpu_frame = self.export_cpu_frame(&handle)?;
                    on_frame(cpu_frame)?;
                    handled += 1;
                }
            }
        }
        Ok(handled)
    }

    fn handle_decoder_events_with_rgba_callback<F>(
        &mut self,
        report: &mut DecodeReport,
        on_frame: &mut F,
    ) -> anyhow::Result<usize>
    where
        F: FnMut(CpuRgbaFrame) -> anyhow::Result<()>,
    {
        let mut handled = 0;
        while let Some(event) = self.decoder.next_event() {
            match event {
                DecoderEvent::FormatChanged => {
                    let stream_info = self.decoder.stream_info().cloned().ok_or(anyhow!(
                        "Decoder reported a format change without stream info"
                    ))?;
                    self.framepool.resize(&stream_info);
                }
                DecoderEvent::FrameReady(handle) => {
                    handle
                        .sync()
                        .context("Failed to synchronize decoded frame")?;
                    report.frames_decoded += 1;
                    let rgba_frame = self.export_rgba_frame(&handle)?;
                    on_frame(rgba_frame)?;
                    handled += 1;
                }
            }
        }
        Ok(handled)
    }

    fn export_frame<H>(&self, handle: &H) -> anyhow::Result<ExportedDmabufFrame>
    where
        H: DecodedHandle<Frame = DecoderFrame>,
    {
        let frame = handle.video_frame();
        let plane_pitches = frame.get_plane_pitch();
        let plane_sizes = frame.get_plane_size();
        let y_plane_preview = frame
            .map()
            .ok()
            .and_then(|mapping| {
                mapping
                    .get()
                    .first()
                    .map(|plane| plane.iter().copied().take(16).collect())
            })
            .unwrap_or_default();
        let va_surface = frame
            .to_native_handle(&self.display)
            .map_err(|err| anyhow!(err))?;
        let descriptor = va_surface
            .export_prime()
            .context("Failed to export VA surface as PRIME")?;

        Ok(ExportedDmabufFrame {
            timestamp: handle.timestamp(),
            coded_resolution: (
                handle.coded_resolution().width,
                handle.coded_resolution().height,
            ),
            display_resolution: (
                handle.display_resolution().width,
                handle.display_resolution().height,
            ),
            drm_fourcc: descriptor.fourcc,
            width: descriptor.width,
            height: descriptor.height,
            plane_pitches,
            plane_sizes,
            y_plane_preview,
            objects: descriptor
                .objects
                .iter()
                .map(|object| ExportedDmabufObject {
                    fd: object.fd.as_raw_fd(),
                    size: object.size,
                    drm_format_modifier: object.drm_format_modifier,
                })
                .collect(),
            layers: descriptor
                .layers
                .iter()
                .map(|layer| ExportedDmabufLayer {
                    drm_format: layer.drm_format,
                    num_planes: layer.num_planes,
                    object_index: layer.object_index,
                    offset: layer.offset,
                    pitch: layer.pitch,
                })
                .collect(),
        })
    }

    fn export_prime_frame(
        &self,
        handle: &Rc<RefCell<VaapiDecodedHandle<DecoderFrame>>>,
    ) -> anyhow::Result<PrimeDmabufFrame> {
        let handle_ref = handle.borrow();
        let va_surface = handle_ref.surface();
        let descriptor = va_surface
            .export_prime()
            .context("Failed to export VA surface as PRIME")?;

        Ok(PrimeDmabufFrame {
            metadata: PrimeFrameMetadata {
                timestamp: handle.timestamp(),
                coded_resolution: (
                    handle.coded_resolution().width,
                    handle.coded_resolution().height,
                ),
                display_resolution: (
                    handle.display_resolution().width,
                    handle.display_resolution().height,
                ),
            },
            descriptor,
        })
    }

    fn export_cpu_frame(
        &self,
        handle: &Rc<RefCell<VaapiDecodedHandle<DecoderFrame>>>,
    ) -> anyhow::Result<CpuNv12Frame> {
        fn copy_nv12_image(
            image: &libva::Image<'_>,
            metadata: PrimeFrameMetadata,
        ) -> anyhow::Result<CpuNv12Frame> {
            let va_image = *image.image();
            let data = image.as_ref();
            let width = metadata.display_resolution.0 as usize;
            let height = metadata.display_resolution.1 as usize;
            let y_stride = va_image.pitches[0] as usize;
            let uv_stride = va_image.pitches[1] as usize;
            let y_offset = va_image.offsets[0] as usize;
            let uv_offset = va_image.offsets[1] as usize;

            let mut y_plane = Vec::with_capacity(width * height);
            let mut uv_plane = Vec::with_capacity(width * (height / 2));

            for row in 0..height {
                let start = y_offset + row * y_stride;
                let end = start + width;
                y_plane.extend_from_slice(&data[start..end]);
            }

            for row in 0..(height / 2) {
                let start = uv_offset + row * uv_stride;
                let end = start + width;
                uv_plane.extend_from_slice(&data[start..end]);
            }

            Ok(CpuNv12Frame {
                metadata,
                width: metadata.display_resolution.0,
                height: metadata.display_resolution.1,
                y_stride: metadata.display_resolution.0,
                uv_stride: metadata.display_resolution.0,
                y_plane,
                uv_plane,
            })
        }

        let handle_ref = handle.borrow();
        let va_surface = handle_ref.surface();
        let display_resolution = (
            handle.display_resolution().width,
            handle.display_resolution().height,
        );
        let metadata = PrimeFrameMetadata {
            timestamp: handle.timestamp(),
            coded_resolution: (
                handle.coded_resolution().width,
                handle.coded_resolution().height,
            ),
            display_resolution,
        };

        if let Ok(image) = libva::Image::derive_from(&va_surface, display_resolution) {
            if image.image().format.fourcc == libva::VA_FOURCC_NV12 {
                return copy_nv12_image(&image, metadata);
            }
        }

        let image_format = self
            .display
            .query_image_formats()
            .context("Failed to query VA image formats")?
            .into_iter()
            .find(|format| format.fourcc == libva::VA_FOURCC_NV12)
            .ok_or_else(|| anyhow!("VA driver does not expose an NV12 VAImage format"))?;
        let image = libva::Image::create_from(
            &va_surface,
            image_format,
            metadata.coded_resolution,
            metadata.display_resolution,
        )
        .context("Failed to create a readable NV12 VAImage from the decoded surface")?;

        copy_nv12_image(&image, metadata)
    }

    fn export_rgba_frame(
        &mut self,
        handle: &Rc<RefCell<VaapiDecodedHandle<DecoderFrame>>>,
    ) -> anyhow::Result<CpuRgbaFrame> {
        let handle_ref = handle.borrow();
        let va_surface = handle_ref.surface();
        let metadata = PrimeFrameMetadata {
            timestamp: handle.timestamp(),
            coded_resolution: (
                handle.coded_resolution().width,
                handle.coded_resolution().height,
            ),
            display_resolution: (
                handle.display_resolution().width,
                handle.display_resolution().height,
            ),
        };

        let (image_format, format) = self
            .display
            .query_image_formats()
            .context("Failed to query VA image formats")?
            .into_iter()
            .find_map(|format| {
                if format.bits_per_pixel != 32 {
                    return None;
                }
                if format.red_mask == 0x00ff0000
                    && format.green_mask == 0x0000ff00
                    && format.blue_mask == 0x000000ff
                    && format.alpha_mask == 0xff000000
                {
                    Some((format, CpuRgbaFormat::Bgra))
                } else if format.red_mask == 0x000000ff
                    && format.green_mask == 0x0000ff00
                    && format.blue_mask == 0x00ff0000
                    && format.alpha_mask == 0xff000000
                {
                    Some((format, CpuRgbaFormat::Rgba))
                } else {
                    None
                }
            })
            .ok_or_else(|| {
                anyhow!("VA driver does not expose a supported 32-bit RGBA/BGRA image format")
            })?;

        let rgba = {
            let image = libva::Image::create_from(
                &va_surface,
                image_format,
                metadata.coded_resolution,
                metadata.display_resolution,
            )
            .context("Failed to create a readable RGBA VAImage from the decoded surface")?;

            copy_packed_rgba_image(&image, metadata.display_resolution)?
        };

        if !LOGGED_RGBA_EXPORT.swap(true, Ordering::Relaxed) {
            eprintln!(
                "rgba export direct: fourcc=0x{fourcc:08x} bpp={} depth={} byte_order={} masks=(r=0x{r:08x}, g=0x{g:08x}, b=0x{b:08x}, a=0x{a:08x}) first_pixel={:?}",
                image_format.bits_per_pixel,
                image_format.depth,
                image_format.byte_order,
                &rgba.get(0..4).unwrap_or(&[]),
                fourcc = image_format.fourcc,
                r = image_format.red_mask,
                g = image_format.green_mask,
                b = image_format.blue_mask,
                a = image_format.alpha_mask,
            );

            let dump_path = std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join("target/first_frame_rgba.ppm");
            if let Err(err) = dump_rgba_frame_ppm(
                &dump_path,
                metadata.display_resolution.0 as usize,
                metadata.display_resolution.1 as usize,
                format,
                &rgba,
            ) {
                eprintln!(
                    "failed to dump first RGBA frame to {}: {err:#}",
                    dump_path.display()
                );
            } else {
                eprintln!("wrote first RGBA frame dump to {}", dump_path.display());
            }
        }

        Ok(CpuRgbaFrame {
            metadata,
            width: metadata.display_resolution.0,
            height: metadata.display_resolution.1,
            stride: metadata.display_resolution.0 * 4,
            format,
            data: rgba,
        })
    }
}

fn copy_packed_rgba_image(
    image: &libva::Image<'_>,
    display_resolution: (u32, u32),
) -> anyhow::Result<Vec<u8>> {
    let va_image = *image.image();
    let width = display_resolution.0 as usize;
    let height = display_resolution.1 as usize;
    let stride = va_image.pitches[0] as usize;
    let offset = va_image.offsets[0] as usize;
    let data = image.as_ref();
    let mut rgba = Vec::with_capacity(width * height * 4);

    for row in 0..height {
        let start = offset + row * stride;
        let end = start + width * 4;
        rgba.extend_from_slice(&data[start..end]);
    }

    Ok(rgba)
}

fn dump_rgba_frame_ppm(
    path: &Path,
    width: usize,
    height: usize,
    format: CpuRgbaFormat,
    data: &[u8],
) -> anyhow::Result<()> {
    use std::fs::File;
    use std::io::Write;

    let mut file = File::create(path)
        .with_context(|| format!("Failed to create frame dump {}", path.display()))?;
    write!(file, "P6\n{} {}\n255\n", width, height)?;

    for pixel in data.chunks_exact(4) {
        let (r, g, b) = match format {
            CpuRgbaFormat::Rgba => (pixel[0], pixel[1], pixel[2]),
            CpuRgbaFormat::Bgra => (pixel[2], pixel[1], pixel[0]),
        };
        file.write_all(&[r, g, b])?;
    }

    Ok(())
}

fn sample_to_annex_b(
    config: &H264TrackConfig,
    packet: &[u8],
    is_sync: bool,
    parameter_sets_sent: &mut bool,
) -> anyhow::Result<Vec<u8>> {
    let mut annex_b = convert_avcc_packet(packet)?;

    if !*parameter_sets_sent || is_sync {
        let mut with_headers = Vec::with_capacity(
            annex_b.len()
                + config.sequence_parameter_set.len()
                + config.picture_parameter_set.len()
                + 8,
        );
        push_annex_b_nal(&mut with_headers, &config.sequence_parameter_set);
        push_annex_b_nal(&mut with_headers, &config.picture_parameter_set);
        with_headers.append(&mut annex_b);
        annex_b = with_headers;
        *parameter_sets_sent = true;
    }

    Ok(annex_b)
}

fn convert_avcc_packet(packet: &[u8]) -> anyhow::Result<Vec<u8>> {
    for length_size in [4usize, 2, 1] {
        if let Some(annex_b) = try_convert_avcc_packet(packet, length_size) {
            return Ok(annex_b);
        }
    }

    Err(anyhow!(
        "Unsupported AVC sample layout; failed to infer NAL length prefix size"
    ))
}

fn try_convert_avcc_packet(packet: &[u8], length_size: usize) -> Option<Vec<u8>> {
    let mut cursor = 0usize;
    let mut output = Vec::with_capacity(packet.len() + 64);

    while cursor < packet.len() {
        if cursor + length_size > packet.len() {
            return None;
        }

        let nal_len = match length_size {
            1 => packet[cursor] as usize,
            2 => u16::from_be_bytes([packet[cursor], packet[cursor + 1]]) as usize,
            4 => u32::from_be_bytes([
                packet[cursor],
                packet[cursor + 1],
                packet[cursor + 2],
                packet[cursor + 3],
            ]) as usize,
            _ => return None,
        };
        cursor += length_size;

        if nal_len == 0 || cursor + nal_len > packet.len() {
            return None;
        }

        push_annex_b_nal(&mut output, &packet[cursor..cursor + nal_len]);
        cursor += nal_len;
    }

    if output.is_empty() {
        None
    } else {
        Some(output)
    }
}

fn push_annex_b_nal(output: &mut Vec<u8>, nal: &[u8]) {
    output.extend_from_slice(&ANNEX_B_START_CODE);
    output.extend_from_slice(nal);
}
