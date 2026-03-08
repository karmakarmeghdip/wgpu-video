#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VideoCodec {
    H264,
    H265,
    Vp8,
    Vp9,
    Av1,
}

impl std::fmt::Display for VideoCodec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::H264 => write!(f, "H.264"),
            Self::H265 => write!(f, "H.265"),
            Self::Vp8 => write!(f, "VP8"),
            Self::Vp9 => write!(f, "VP9"),
            Self::Av1 => write!(f, "AV1"),
        }
    }
}

#[derive(Clone, Debug)]
pub struct H264TrackConfig {
    pub track_id: u32,
    pub width: u16,
    pub height: u16,
    pub timescale: u32,
    pub sequence_parameter_sets: Vec<Vec<u8>>,
    pub picture_parameter_sets: Vec<Vec<u8>>,
    pub nal_length_size: usize,
}

#[derive(Clone, Debug)]
pub struct H265TrackConfig {
    pub track_id: u32,
    pub width: u16,
    pub height: u16,
    pub timescale: u32,
    pub video_parameter_sets: Vec<Vec<u8>>,
    pub sequence_parameter_sets: Vec<Vec<u8>>,
    pub picture_parameter_sets: Vec<Vec<u8>>,
    pub nal_length_size: usize,
}

#[derive(Clone, Debug)]
pub struct VideoTrackConfig {
    pub track_id: u32,
    pub codec: VideoCodec,
    pub width: u32,
    pub height: u32,
    pub timescale: u32,
    pub h264: Option<H264TrackConfig>,
    pub h265: Option<H265TrackConfig>,
}

#[derive(Clone, Debug)]
pub struct VideoSample {
    pub bytes: Vec<u8>,
    pub start_time: u64,
    pub duration: u64,
    pub rendering_offset: i64,
    pub is_sync: bool,
}

#[derive(Clone, Debug)]
pub(super) struct VideoSampleMetadata {
    pub(super) start_time: u64,
    pub(super) duration: u64,
    pub(super) rendering_offset: i64,
    pub(super) is_sync: bool,
}

pub(crate) fn sample_presentation_timestamp(sample: &VideoSample) -> u64 {
    sample
        .start_time
        .saturating_add_signed(sample.rendering_offset)
}

pub(crate) fn sample_presentation_end_timestamp(sample: &VideoSample) -> u64 {
    sample_presentation_timestamp(sample).saturating_add(sample.duration)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presentation_timestamp_applies_rendering_offset() {
        let sample = VideoSample {
            bytes: Vec::new(),
            start_time: 100,
            duration: 40,
            rendering_offset: -10,
            is_sync: true,
        };

        assert_eq!(sample_presentation_timestamp(&sample), 90);
        assert_eq!(sample_presentation_end_timestamp(&sample), 130);
    }

    #[test]
    fn presentation_timestamp_saturates_negative_offset() {
        let sample = VideoSample {
            bytes: Vec::new(),
            start_time: 5,
            duration: 10,
            rendering_offset: -20,
            is_sync: false,
        };

        assert_eq!(sample_presentation_timestamp(&sample), 0);
        assert_eq!(sample_presentation_end_timestamp(&sample), 10);
    }
}
