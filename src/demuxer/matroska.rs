use std::{
    collections::HashMap,
    fs::File,
    io::BufReader,
    path::{Path, PathBuf},
};

use anyhow::{Context, anyhow, bail};
use matroska_demuxer::{Frame as MatroskaFrame, MatroskaFile, TrackType};

use super::codec_config::{
    codec_from_matroska_id, parse_matroska_h264_codec_private, parse_matroska_h265_codec_private,
};
use super::types::VideoSampleMetadata;
use super::{VideoCodec, VideoSample, VideoTrackConfig};

pub(super) struct MatroskaDemuxer {
    path: PathBuf,
    tracks: HashMap<u32, VideoTrackConfig>,
    samples: HashMap<u32, Vec<VideoSampleMetadata>>,
    reader: MatroskaFile<BufReader<File>>,
    next_sample_indices: HashMap<u32, u32>,
    timestamp_scale_ns: u64,
    duration_ns: Option<u64>,
}

impl MatroskaDemuxer {
    pub(super) fn open(file_path: &Path) -> anyhow::Result<Self> {
        let mut mkv = open_matroska_reader(file_path)?;

        let info = mkv.info();
        let timestamp_scale_ns = info.timestamp_scale().get();
        let duration_ns = info.duration().map(|duration| {
            let duration = duration.max(0.0) * timestamp_scale_ns as f64;
            duration.min(u64::MAX as f64) as u64
        });
        let timescale = 1_000_000_000;

        let mut tracks = HashMap::new();
        for track in mkv.tracks() {
            if track.track_type() != TrackType::Video {
                continue;
            }

            let Some(codec) = codec_from_matroska_id(track.codec_id()) else {
                continue;
            };
            let Some(video) = track.video() else {
                continue;
            };

            let track_id = u32::try_from(track.track_number().get()).with_context(|| {
                format!("Unsupported Matroska track id {}", track.track_number())
            })?;
            let width = u32::try_from(video.pixel_width().get())
                .with_context(|| format!("Unsupported width for track {track_id}"))?;
            let height = u32::try_from(video.pixel_height().get())
                .with_context(|| format!("Unsupported height for track {track_id}"))?;
            let h264 = match codec {
                VideoCodec::H264 => Some(parse_matroska_h264_codec_private(
                    track_id,
                    width,
                    height,
                    timescale,
                    track.codec_private(),
                )?),
                _ => None,
            };
            let h265 = match codec {
                VideoCodec::H265 => Some(parse_matroska_h265_codec_private(
                    track_id,
                    width,
                    height,
                    timescale,
                    track.codec_private(),
                )?),
                _ => None,
            };

            tracks.insert(
                track_id,
                VideoTrackConfig {
                    track_id,
                    codec,
                    width,
                    height,
                    timescale,
                    h264,
                    h265,
                },
            );
        }

        if tracks.is_empty() {
            bail!("No supported video tracks found in Matroska container")
        }

        let mut samples: HashMap<u32, Vec<VideoSampleMetadata>> = HashMap::new();
        let mut frame = MatroskaFrame::default();
        while mkv.next_frame(&mut frame)? {
            let Ok(track_id) = u32::try_from(frame.track) else {
                continue;
            };
            let Some(track) = tracks.get(&track_id) else {
                continue;
            };

            let start_time = scale_matroska_timestamp(frame.timestamp, timestamp_scale_ns);
            let duration = frame
                .duration
                .map(|value| scale_matroska_timestamp(value, timestamp_scale_ns))
                .unwrap_or_default();

            samples
                .entry(track_id)
                .or_default()
                .push(VideoSampleMetadata {
                    start_time,
                    duration,
                    rendering_offset: 0,
                    is_sync: match track.codec {
                        VideoCodec::H264 | VideoCodec::H265 => frame.is_keyframe.unwrap_or(false),
                        _ => frame.is_keyframe.unwrap_or(true),
                    },
                });
        }

        if duration_ns.is_none() {
            for track_samples in samples.values_mut() {
                backfill_sample_durations(track_samples);
            }
        }

        let reader = open_matroska_reader(file_path)?;

        Ok(Self {
            path: file_path.to_path_buf(),
            tracks,
            samples,
            reader,
            next_sample_indices: HashMap::new(),
            timestamp_scale_ns,
            duration_ns,
        })
    }

    pub(super) fn track_ids(&self) -> Vec<u32> {
        self.tracks.keys().copied().collect()
    }

    pub(super) fn track_config(&self, track_id: u32) -> anyhow::Result<VideoTrackConfig> {
        self.tracks
            .get(&track_id)
            .cloned()
            .ok_or_else(|| anyhow!("Invalid track id {track_id}"))
    }

    pub(super) fn sample_count(&self, track_id: u32) -> anyhow::Result<u32> {
        let count = self
            .samples
            .get(&track_id)
            .map(|track_samples| track_samples.len())
            .unwrap_or(0);
        u32::try_from(count).context("Track sample count exceeds u32")
    }

    pub(super) fn read_sample(
        &mut self,
        track_id: u32,
        sample_id: u32,
    ) -> anyhow::Result<Option<VideoSample>> {
        let index = sample_id.saturating_sub(1) as usize;
        let Some(metadata) = self
            .samples
            .get(&track_id)
            .and_then(|track_samples| track_samples.get(index))
            .cloned()
        else {
            return Ok(None);
        };

        let next_expected = self
            .next_sample_indices
            .get(&track_id)
            .copied()
            .unwrap_or(1);
        if sample_id < next_expected {
            self.reader = open_matroska_reader(&self.path)?;
            self.next_sample_indices.clear();
        }

        let mut frame = MatroskaFrame::default();
        loop {
            if !self.reader.next_frame(&mut frame)? {
                return Ok(None);
            }

            let Ok(frame_track_id) = u32::try_from(frame.track) else {
                continue;
            };
            let current_sample_id = self.next_sample_indices.entry(frame_track_id).or_insert(1);
            let matched = frame_track_id == track_id && *current_sample_id == sample_id;
            *current_sample_id = current_sample_id.saturating_add(1);

            if matched {
                return Ok(Some(VideoSample {
                    bytes: frame.data.clone(),
                    start_time: metadata.start_time,
                    duration: metadata.duration,
                    rendering_offset: metadata.rendering_offset,
                    is_sync: metadata.is_sync,
                }));
            }
        }
    }

    pub(super) fn print_debug_info(&mut self) -> anyhow::Result<()> {
        println!("container: Matroska ({})", self.path.display());
        println!("timestamp scale (ns): {}", self.timestamp_scale_ns);
        println!("duration (ns): {:?}", self.duration_ns);
        for track_id in self.tracks.keys().copied().collect::<Vec<_>>() {
            let track = self
                .tracks
                .get(&track_id)
                .ok_or_else(|| anyhow!("Invalid track id {track_id}"))?;
            let sample_count = self.samples.get(&track_id).map(Vec::len).unwrap_or(0);
            println!(
                "track: #{} {} {}x{} samples:{}",
                track.track_id, track.codec, track.width, track.height, sample_count,
            );
        }
        Ok(())
    }
}

fn scale_matroska_timestamp(value: u64, timestamp_scale_ns: u64) -> u64 {
    (u128::from(value).saturating_mul(u128::from(timestamp_scale_ns))).min(u128::from(u64::MAX))
        as u64
}

fn open_matroska_reader(file_path: &Path) -> anyhow::Result<MatroskaFile<BufReader<File>>> {
    let file = File::open(file_path)?;
    let reader = BufReader::new(file);
    MatroskaFile::open(reader)
        .with_context(|| format!("Failed to open Matroska container {}", file_path.display()))
}

fn backfill_sample_durations(samples: &mut [VideoSampleMetadata]) {
    for index in 0..samples.len() {
        if samples[index].duration != 0 {
            continue;
        }
        let next_start = samples.get(index + 1).map(|sample| sample.start_time);
        samples[index].duration = next_start
            .map(|next| next.saturating_sub(samples[index].start_time))
            .unwrap_or_default();
    }
}
