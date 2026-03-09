use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::OnceLock,
};

use anyhow::{anyhow, bail, Context};
use ffmpeg_the_third as ffmpeg;

use super::codec_config::{
    codec_from_ffmpeg_id, parse_matroska_h264_codec_private, parse_matroska_h265_codec_private,
};
use super::types::sample_presentation_timestamp;
use super::{VideoCodec, VideoSample, VideoTrackConfig};

pub(super) struct FfmpegDemuxer {
    path: PathBuf,
    container_name: String,
    container_description: String,
    tracks: HashMap<u32, VideoTrackConfig>,
    samples: HashMap<u32, Vec<VideoSample>>,
}

impl FfmpegDemuxer {
    pub(super) fn open(file_path: &Path) -> anyhow::Result<Self> {
        ensure_ffmpeg_initialized()?;

        let mut input = ffmpeg::format::input(file_path).with_context(|| {
            format!(
                "Failed to open container {} with FFmpeg",
                file_path.display()
            )
        })?;

        let container_name = input.format().name().to_owned();
        let container_description = input.format().description().to_owned();
        let mut tracks = HashMap::new();
        let mut time_bases = HashMap::new();

        for stream in input.streams() {
            let parameters = stream.parameters();
            if parameters.medium() != ffmpeg::media::Type::Video {
                continue;
            }

            let Some(codec) = codec_from_ffmpeg_id(parameters.id()) else {
                continue;
            };

            let track_id = u32::try_from(stream.index())
                .with_context(|| format!("Unsupported FFmpeg stream index {}", stream.index()))?;
            let width = parameters.width();
            let height = parameters.height();
            let timescale = 1_000_000_000;
            let extradata = extradata_from_parameters(&parameters);

            let h264 = match codec {
                VideoCodec::H264 => Some(
                    parse_matroska_h264_codec_private(
                        track_id, width, height, timescale, extradata,
                    )
                    .with_context(|| {
                        format!("Failed to parse H.264 codec configuration for stream {track_id}")
                    })?,
                ),
                _ => None,
            };
            let h265 = match codec {
                VideoCodec::H265 => Some(
                    parse_matroska_h265_codec_private(
                        track_id, width, height, timescale, extradata,
                    )
                    .with_context(|| {
                        format!("Failed to parse H.265 codec configuration for stream {track_id}")
                    })?,
                ),
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
            time_bases.insert(track_id, stream.time_base());
        }

        if tracks.is_empty() {
            bail!("No supported video tracks found in FFmpeg demuxer")
        }

        let mut samples: HashMap<u32, Vec<VideoSample>> = HashMap::new();
        for packet in input.packets() {
            let (stream, packet) = packet.context("Failed to read packet from FFmpeg demuxer")?;
            let track_id = match u32::try_from(stream.index()) {
                Ok(track_id) if tracks.contains_key(&track_id) => track_id,
                _ => continue,
            };
            let Some(bytes) = packet.data() else {
                continue;
            };

            let time_base = *time_bases
                .get(&track_id)
                .ok_or_else(|| anyhow!("Missing FFmpeg time base for stream {track_id}"))?;
            let (start_time, duration, rendering_offset) = packet_timing(&packet, time_base);

            samples.entry(track_id).or_default().push(VideoSample {
                bytes: bytes.to_vec(),
                start_time,
                duration,
                rendering_offset,
                is_sync: packet.is_key(),
            });
        }

        for track_samples in samples.values_mut() {
            backfill_sample_durations(track_samples);
        }

        Ok(Self {
            path: file_path.to_path_buf(),
            container_name,
            container_description,
            tracks,
            samples,
        })
    }

    pub(super) fn track_ids(&self) -> Vec<u32> {
        let mut track_ids = self.tracks.keys().copied().collect::<Vec<_>>();
        track_ids.sort_unstable();
        track_ids
    }

    pub(super) fn track_config(&self, track_id: u32) -> anyhow::Result<VideoTrackConfig> {
        self.tracks
            .get(&track_id)
            .cloned()
            .ok_or_else(|| anyhow!("Invalid track id {track_id}"))
    }

    pub(super) fn sample_count(&self, track_id: u32) -> anyhow::Result<u32> {
        let count = self.samples.get(&track_id).map(Vec::len).unwrap_or(0);
        u32::try_from(count).context("Track sample count exceeds u32")
    }

    pub(super) fn read_sample(
        &mut self,
        track_id: u32,
        sample_id: u32,
    ) -> anyhow::Result<Option<VideoSample>> {
        let index = sample_id.saturating_sub(1) as usize;
        Ok(self
            .samples
            .get(&track_id)
            .and_then(|track_samples| track_samples.get(index))
            .cloned())
    }

    pub(super) fn print_debug_info(&mut self) -> anyhow::Result<()> {
        println!(
            "container: FFmpeg {} ({})",
            self.container_name,
            self.path.display()
        );
        if !self.container_description.is_empty() {
            println!("description: {}", self.container_description);
        }
        for track_id in self.track_ids() {
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

fn ensure_ffmpeg_initialized() -> anyhow::Result<()> {
    static INIT: OnceLock<Result<(), String>> = OnceLock::new();

    INIT.get_or_init(|| ffmpeg::init().map_err(|err| err.to_string()))
        .as_ref()
        .map_err(|err| anyhow!("Failed to initialize FFmpeg: {err}"))?;
    Ok(())
}

fn extradata_from_parameters<'a>(
    parameters: &'a ffmpeg::codec::parameters::ParametersRef<'a>,
) -> Option<&'a [u8]> {
    unsafe {
        let ptr = parameters.as_ptr();
        let extradata = (*ptr).extradata;
        let extradata_size = (*ptr).extradata_size;
        if extradata.is_null() || extradata_size <= 0 {
            None
        } else {
            Some(std::slice::from_raw_parts(
                extradata.cast::<u8>(),
                extradata_size as usize,
            ))
        }
    }
}

fn packet_timing(packet: &ffmpeg::Packet, time_base: ffmpeg::Rational) -> (u64, u64, i64) {
    let start_units = packet.dts().or_else(|| packet.pts()).unwrap_or(0);
    let presentation_units = packet.pts().or_else(|| packet.dts()).unwrap_or(start_units);
    let start_time = scale_timestamp_to_ns(start_units, time_base);
    let presentation_time = scale_timestamp_to_ns(presentation_units, time_base);
    let duration = scale_duration_to_ns(packet.duration(), time_base);
    let rendering_offset =
        saturating_i128_to_i64(i128::from(presentation_time) - i128::from(start_time));

    (start_time, duration, rendering_offset)
}

fn scale_timestamp_to_ns(value: i64, time_base: ffmpeg::Rational) -> u64 {
    if value <= 0 {
        return 0;
    }

    let numerator = i128::from(time_base.numerator());
    let denominator = i128::from(time_base.denominator());
    if numerator <= 0 || denominator <= 0 {
        return 0;
    }

    let scaled = i128::from(value)
        .saturating_mul(numerator)
        .saturating_mul(1_000_000_000i128)
        / denominator;
    scaled.clamp(0, i128::from(u64::MAX)) as u64
}

fn scale_duration_to_ns(value: i64, time_base: ffmpeg::Rational) -> u64 {
    if value <= 0 {
        return 0;
    }

    scale_timestamp_to_ns(value, time_base)
}

fn saturating_i128_to_i64(value: i128) -> i64 {
    value.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64
}

fn backfill_sample_durations(samples: &mut [VideoSample]) {
    for index in 0..samples.len() {
        if samples[index].duration != 0 {
            continue;
        }
        let next_start = samples.get(index + 1).map(sample_presentation_timestamp);
        samples[index].duration = next_start
            .map(|next| next.saturating_sub(sample_presentation_timestamp(&samples[index])))
            .unwrap_or_default();
    }
}
