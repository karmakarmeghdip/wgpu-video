use std::cell::RefCell;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, anyhow, bail};
use codecs::backend::vaapi::decoder::VaapiBackend as CodecVaapiBackend;
use codecs::backend::vaapi::decoder::VaapiDecodedHandle;
use codecs::decoder::stateless::av1::Av1;
use codecs::decoder::stateless::h264::H264;
use codecs::decoder::stateless::h265::H265;
use codecs::decoder::stateless::vp8::Vp8;
use codecs::decoder::stateless::vp9::Vp9;
use codecs::decoder::stateless::{DecodeError, StatelessDecoder, StatelessVideoDecoder};
use codecs::decoder::{DecodedHandle, DecoderEvent, StreamInfo};
use codecs::video_frame::VideoFrame;
use codecs::video_frame::frame_pool::{FramePool, PooledVideoFrame};
use codecs::{BlockingMode, Fourcc, Resolution};
use libva::{Display, Surface, UsageHint};

use crate::demuxer::{
    Demuxer, H264TrackConfig, H265TrackConfig, VideoCodec, sample_presentation_timestamp,
};

mod annexb;
mod decode;
mod events;
mod export;

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
type DecoderHandle = Rc<RefCell<VaapiDecodedHandle<DecoderFrame>>>;
type H264Decoder = StatelessDecoder<H264, CodecVaapiBackend<DecoderFrame>>;
type H265Decoder = StatelessDecoder<H265, CodecVaapiBackend<DecoderFrame>>;
type Vp8Decoder = StatelessDecoder<Vp8, CodecVaapiBackend<DecoderFrame>>;
type Vp9Decoder = StatelessDecoder<Vp9, CodecVaapiBackend<DecoderFrame>>;
type Av1Decoder = StatelessDecoder<Av1, CodecVaapiBackend<DecoderFrame>>;

enum ActiveDecoder {
    H264(H264Decoder),
    H265(H265Decoder),
    Vp8(Vp8Decoder),
    Vp9(Vp9Decoder),
    Av1(Av1Decoder),
}

impl ActiveDecoder {
    fn new(codec: VideoCodec, display: Rc<Display>) -> anyhow::Result<Self> {
        match codec {
            VideoCodec::H264 => {
                StatelessDecoder::<H264, _>::new_vaapi(display, BlockingMode::Blocking)
                    .map(Self::H264)
                    .map_err(|err| anyhow!("Failed to create VA-API H.264 decoder: {err:?}"))
            }
            VideoCodec::H265 => {
                StatelessDecoder::<H265, _>::new_vaapi(display, BlockingMode::Blocking)
                    .map(Self::H265)
                    .map_err(|err| anyhow!("Failed to create VA-API H.265 decoder: {err:?}"))
            }
            VideoCodec::Vp8 => {
                StatelessDecoder::<Vp8, _>::new_vaapi(display, BlockingMode::Blocking)
                    .map(Self::Vp8)
                    .map_err(|err| anyhow!("Failed to create VA-API VP8 decoder: {err:?}"))
            }
            VideoCodec::Vp9 => {
                StatelessDecoder::<Vp9, _>::new_vaapi(display, BlockingMode::Blocking)
                    .map(Self::Vp9)
                    .map_err(|err| anyhow!("Failed to create VA-API VP9 decoder: {err:?}"))
            }
            VideoCodec::Av1 => {
                StatelessDecoder::<Av1, _>::new_vaapi(display, BlockingMode::Blocking)
                    .map(Self::Av1)
                    .map_err(|err| anyhow!("Failed to create VA-API AV1 decoder: {err:?}"))
            }
        }
    }

    fn decode(
        &mut self,
        timestamp: u64,
        bitstream: &[u8],
        alloc_cb: &mut dyn FnMut() -> Option<DecoderFrame>,
    ) -> Result<usize, DecodeError> {
        match self {
            Self::H264(decoder) => decoder.decode(timestamp, bitstream, alloc_cb),
            Self::H265(decoder) => decoder.decode(timestamp, bitstream, alloc_cb),
            Self::Vp8(decoder) => decoder.decode(timestamp, bitstream, alloc_cb),
            Self::Vp9(decoder) => decoder.decode(timestamp, bitstream, alloc_cb),
            Self::Av1(decoder) => decoder.decode(timestamp, bitstream, alloc_cb),
        }
    }

    fn flush(&mut self) -> Result<(), DecodeError> {
        match self {
            Self::H264(decoder) => decoder.flush(),
            Self::H265(decoder) => decoder.flush(),
            Self::Vp8(decoder) => decoder.flush(),
            Self::Vp9(decoder) => decoder.flush(),
            Self::Av1(decoder) => decoder.flush(),
        }
    }

    fn next_event(&mut self) -> Option<DecoderEvent<DecoderHandle>> {
        match self {
            Self::H264(decoder) => decoder.next_event(),
            Self::H265(decoder) => decoder.next_event(),
            Self::Vp8(decoder) => decoder.next_event(),
            Self::Vp9(decoder) => decoder.next_event(),
            Self::Av1(decoder) => decoder.next_event(),
        }
    }

    fn stream_info(&self) -> Option<&StreamInfo> {
        match self {
            Self::H264(decoder) => decoder.stream_info(),
            Self::H265(decoder) => decoder.stream_info(),
            Self::Vp8(decoder) => decoder.stream_info(),
            Self::Vp9(decoder) => decoder.stream_info(),
            Self::Av1(decoder) => decoder.stream_info(),
        }
    }
}

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
    decoder: ActiveDecoder,
    framepool: FramePool<NativeVaFrame>,
    active_codec: VideoCodec,
    parameter_sets_sent: bool,
}

impl VaapiBackend {
    pub fn new() -> anyhow::Result<Self> {
        Self::new_with_render_node(Path::new(DEFAULT_RENDER_NODE))
    }

    pub fn new_with_render_node(render_node: &Path) -> anyhow::Result<Self> {
        let display = Display::open_drm_display(render_node)
            .with_context(|| format!("Failed to open VA display on {}", render_node.display()))?;
        let decoder = ActiveDecoder::new(VideoCodec::H264, display.clone())?;
        let framepool = FramePool::new(move |stream_info: &StreamInfo| NativeVaFrame {
            coded_resolution: stream_info.coded_resolution,
            display_resolution: stream_info.display_resolution,
        });

        Ok(Self {
            display,
            decoder,
            framepool,
            active_codec: VideoCodec::H264,
            parameter_sets_sent: false,
        })
    }

    fn ensure_decoder(&mut self, codec: VideoCodec) -> anyhow::Result<()> {
        if self.active_codec == codec {
            return Ok(());
        }

        self.decoder = ActiveDecoder::new(codec, self.display.clone())?;
        self.active_codec = codec;
        self.parameter_sets_sent = false;
        Ok(())
    }
}
