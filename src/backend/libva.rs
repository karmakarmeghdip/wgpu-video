use std::path::PathBuf;

use codecs::backend::vaapi::decoder::VaapiBackend as CodecVaapiBackend;
use codecs::{
    BlockingMode, Fourcc,
    decoder::stateless::{StatelessDecoder, h264::H264},
    video_frame::{
        frame_pool::FramePool,
        gbm_video_frame::{GbmDevice, GbmUsage},
        generic_dma_video_frame::GenericDmaVideoFrame,
    },
};
use libva::Display;

pub struct VaapiBackend {
    decoder: StatelessDecoder<H264, CodecVaapiBackend<GenericDmaVideoFrame>>,
    framepool: FramePool<GenericDmaVideoFrame>,
}

impl VaapiBackend {
    pub fn new() -> Option<Self> {
        let display = Display::open()?;
        let decoder =
            StatelessDecoder::<H264, _>::new_vaapi(display, BlockingMode::Blocking).ok()?;
        let gbm_device = GbmDevice::open(PathBuf::from("/dev/dri/renderD128")).ok()?;
        let framepool = FramePool::new(move |stream_info| {
            gbm_device
                .clone()
                .new_frame(
                    Fourcc::from(stream_info.format),
                    stream_info.display_resolution,
                    stream_info.coded_resolution,
                    GbmUsage::Decode,
                )
                .expect("Failed to allocate GBM frame")
                .to_generic_dma_video_frame()
                .expect("Failed to export to DMA")
        });
        Some(Self { decoder, framepool })
    }
}
