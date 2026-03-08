mod codec_config;
mod matroska;
mod mp4;
mod types;

use std::path::Path;

use anyhow::{anyhow, bail};

use matroska::MatroskaDemuxer;
use mp4::Mp4Demuxer;

pub use types::{H264TrackConfig, H265TrackConfig, VideoCodec, VideoSample, VideoTrackConfig};
pub(crate) use types::{sample_presentation_end_timestamp, sample_presentation_timestamp};

enum DemuxerInner {
    Mp4(Mp4Demuxer),
    Matroska(MatroskaDemuxer),
}

pub struct Demuxer {
    inner: DemuxerInner,
}

impl Demuxer {
    pub fn new(file_path: &Path) -> anyhow::Result<Self> {
        let extension = file_path
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.to_ascii_lowercase());

        match extension.as_deref() {
            Some("mkv") | Some("webm") => Self::open_matroska(file_path)
                .or_else(|matroska_err| Self::open_mp4(file_path).map_err(|_| matroska_err)),
            _ => Self::open_mp4(file_path)
                .or_else(|mp4_err| Self::open_matroska(file_path).map_err(|_| mp4_err)),
        }
    }

    fn open_mp4(file_path: &Path) -> anyhow::Result<Self> {
        Ok(Self {
            inner: DemuxerInner::Mp4(Mp4Demuxer::open(file_path)?),
        })
    }

    fn open_matroska(file_path: &Path) -> anyhow::Result<Self> {
        Ok(Self {
            inner: DemuxerInner::Matroska(MatroskaDemuxer::open(file_path)?),
        })
    }

    pub fn get_tracks(&mut self) -> Vec<u32> {
        match &mut self.inner {
            DemuxerInner::Mp4(demuxer) => demuxer.track_ids(),
            DemuxerInner::Matroska(demuxer) => demuxer.track_ids(),
        }
    }

    pub fn get_track_info(&mut self, track_id: u32) -> anyhow::Result<VideoCodec> {
        Ok(self.get_track_config(track_id)?.codec)
    }

    pub fn get_track_config(&mut self, track_id: u32) -> anyhow::Result<VideoTrackConfig> {
        match &mut self.inner {
            DemuxerInner::Mp4(demuxer) => demuxer.track_config(track_id),
            DemuxerInner::Matroska(demuxer) => demuxer.track_config(track_id),
        }
    }

    pub fn find_video_track(&mut self) -> anyhow::Result<u32> {
        let preferred = [
            VideoCodec::H264,
            VideoCodec::H265,
            VideoCodec::Vp9,
            VideoCodec::Vp8,
            VideoCodec::Av1,
        ];
        let tracks = self.get_tracks();

        for codec in preferred {
            for track_id in &tracks {
                if let Ok(track_codec) = self.get_track_info(*track_id) {
                    if track_codec == codec {
                        return Ok(*track_id);
                    }
                }
            }
        }

        bail!("No supported video track found in file")
    }

    pub fn find_h264_track(&mut self) -> anyhow::Result<u32> {
        self.get_tracks()
            .into_iter()
            .find(|track_id| matches!(self.get_track_info(*track_id), Ok(VideoCodec::H264)))
            .ok_or_else(|| anyhow!("No H.264 track found in file"))
    }

    pub fn get_h264_track_config(&mut self, track_id: u32) -> anyhow::Result<H264TrackConfig> {
        self.get_track_config(track_id)?
            .h264
            .ok_or_else(|| anyhow!("Track {track_id} is not an H.264 track"))
    }

    pub fn sample_count(&mut self, track_id: u32) -> anyhow::Result<u32> {
        match &mut self.inner {
            DemuxerInner::Mp4(demuxer) => demuxer.sample_count(track_id),
            DemuxerInner::Matroska(demuxer) => demuxer.sample_count(track_id),
        }
    }

    pub fn read_sample(
        &mut self,
        track_id: u32,
        sample_id: u32,
    ) -> anyhow::Result<Option<VideoSample>> {
        match &mut self.inner {
            DemuxerInner::Mp4(demuxer) => demuxer.read_sample(track_id, sample_id),
            DemuxerInner::Matroska(demuxer) => demuxer.read_sample(track_id, sample_id),
        }
    }

    pub fn parse_track_packets<F>(&mut self, track_id: u32, mut cb: F) -> anyhow::Result<()>
    where
        F: FnMut(&VideoSample),
    {
        let sample_count = self.sample_count(track_id)?;
        for sample_id in 1..=sample_count {
            if let Some(sample) = self.read_sample(track_id, sample_id)? {
                cb(&sample);
            }
        }
        Ok(())
    }

    pub fn print_debug_info(&mut self) -> anyhow::Result<()> {
        match &mut self.inner {
            DemuxerInner::Mp4(demuxer) => demuxer.print_debug_info(),
            DemuxerInner::Matroska(demuxer) => demuxer.print_debug_info(),
        }
    }
}
