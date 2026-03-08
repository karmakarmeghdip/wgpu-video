use std::{
    collections::HashMap,
    fs::File,
    io::BufReader,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, bail, Context};
use matroska_demuxer::{Frame as MatroskaFrame, MatroskaFile, TrackType};

pub(crate) fn sample_presentation_timestamp(sample: &VideoSample) -> u64 {
    sample
        .start_time
        .saturating_add_signed(sample.rendering_offset)
}

pub(crate) fn sample_presentation_end_timestamp(sample: &VideoSample) -> u64 {
    sample_presentation_timestamp(sample).saturating_add(sample.duration)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VideoCodec {
    H264,
    Vp8,
    Vp9,
    Av1,
}

impl std::fmt::Display for VideoCodec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::H264 => write!(f, "H.264"),
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
pub struct VideoTrackConfig {
    pub track_id: u32,
    pub codec: VideoCodec,
    pub width: u32,
    pub height: u32,
    pub timescale: u32,
    pub h264: Option<H264TrackConfig>,
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
struct VideoSampleMetadata {
    start_time: u64,
    duration: u64,
    rendering_offset: i64,
    is_sync: bool,
}

enum DemuxerInner {
    Mp4 {
        video: mp4::Mp4Reader<BufReader<File>>,
    },
    Matroska {
        path: PathBuf,
        tracks: HashMap<u32, VideoTrackConfig>,
        samples: HashMap<u32, Vec<VideoSampleMetadata>>,
        reader: MatroskaFile<BufReader<File>>,
        next_sample_indices: HashMap<u32, u32>,
        timestamp_scale_ns: u64,
        duration_ns: Option<u64>,
    },
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
        let file = File::open(file_path)?;
        let size = file.metadata()?.len();
        let reader = BufReader::new(file);
        let mp4 = mp4::Mp4Reader::read_header(reader, size)?;
        Ok(Self {
            inner: DemuxerInner::Mp4 { video: mp4 },
        })
    }

    fn open_matroska(file_path: &Path) -> anyhow::Result<Self> {
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

            tracks.insert(
                track_id,
                VideoTrackConfig {
                    track_id,
                    codec,
                    width,
                    height,
                    timescale,
                    h264,
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
                        VideoCodec::H264 => frame.is_keyframe.unwrap_or(false),
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
            inner: DemuxerInner::Matroska {
                path: file_path.to_path_buf(),
                tracks,
                samples,
                reader,
                next_sample_indices: HashMap::new(),
                timestamp_scale_ns,
                duration_ns,
            },
        })
    }

    pub fn get_tracks(&mut self) -> Vec<u32> {
        match &mut self.inner {
            DemuxerInner::Mp4 { video } => video.tracks().keys().copied().collect(),
            DemuxerInner::Matroska { tracks, .. } => tracks.keys().copied().collect(),
        }
    }

    pub fn get_track_info(&mut self, track_id: u32) -> anyhow::Result<VideoCodec> {
        Ok(self.get_track_config(track_id)?.codec)
    }

    pub fn get_track_config(&mut self, track_id: u32) -> anyhow::Result<VideoTrackConfig> {
        match &mut self.inner {
            DemuxerInner::Mp4 { video } => {
                let track = video
                    .tracks()
                    .get(&track_id)
                    .ok_or_else(|| anyhow!("Invalid track id {track_id}"))?;
                let codec = codec_from_mp4_fourcc(
                    track
                        .box_type()
                        .map_err(|_| anyhow!("Failed to get codec string"))?,
                )?;
                Ok(VideoTrackConfig {
                    track_id,
                    codec,
                    width: u32::from(track.width()),
                    height: u32::from(track.height()),
                    timescale: track.timescale(),
                    h264: match codec {
                        VideoCodec::H264 => Some(H264TrackConfig {
                            track_id,
                            width: track.width(),
                            height: track.height(),
                            timescale: track.timescale(),
                            sequence_parameter_sets: vec![track
                                .sequence_parameter_set()
                                .map_err(|_| anyhow!("Missing H.264 SPS for track {track_id}"))?
                                .to_vec()],
                            picture_parameter_sets: vec![track
                                .picture_parameter_set()
                                .map_err(|_| anyhow!("Missing H.264 PPS for track {track_id}"))?
                                .to_vec()],
                            nal_length_size: 4,
                        }),
                        _ => None,
                    },
                })
            }
            DemuxerInner::Matroska { tracks, .. } => tracks
                .get(&track_id)
                .cloned()
                .ok_or_else(|| anyhow!("Invalid track id {track_id}")),
        }
    }

    pub fn find_video_track(&mut self) -> anyhow::Result<u32> {
        let preferred = [
            VideoCodec::H264,
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
            DemuxerInner::Mp4 { video } => Ok(video.sample_count(track_id)?),
            DemuxerInner::Matroska { samples, .. } => {
                let count = samples
                    .get(&track_id)
                    .map(|track_samples| track_samples.len())
                    .unwrap_or(0);
                u32::try_from(count).context("Track sample count exceeds u32")
            }
        }
    }

    pub fn read_sample(
        &mut self,
        track_id: u32,
        sample_id: u32,
    ) -> anyhow::Result<Option<VideoSample>> {
        match &mut self.inner {
            DemuxerInner::Mp4 { video } => {
                Ok(video
                    .read_sample(track_id, sample_id)?
                    .map(|sample| VideoSample {
                        bytes: sample.bytes.to_vec(),
                        start_time: sample.start_time,
                        duration: u64::from(sample.duration),
                        rendering_offset: i64::from(sample.rendering_offset),
                        is_sync: sample.is_sync,
                    }))
            }
            DemuxerInner::Matroska {
                path,
                samples,
                reader,
                next_sample_indices,
                ..
            } => {
                let index = sample_id.saturating_sub(1) as usize;
                let Some(metadata) = samples
                    .get(&track_id)
                    .and_then(|track_samples| track_samples.get(index))
                    .cloned()
                else {
                    return Ok(None);
                };

                let next_expected = next_sample_indices.get(&track_id).copied().unwrap_or(1);
                if sample_id < next_expected {
                    *reader = open_matroska_reader(path)?;
                    next_sample_indices.clear();
                }

                let mut frame = MatroskaFrame::default();
                loop {
                    if !reader.next_frame(&mut frame)? {
                        return Ok(None);
                    }

                    let Ok(frame_track_id) = u32::try_from(frame.track) else {
                        continue;
                    };
                    let current_sample_id = next_sample_indices.entry(frame_track_id).or_insert(1);
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
            DemuxerInner::Mp4 { video } => {
                println!("major brand: {}", video.ftyp.major_brand);
                println!("timescale: {}", video.moov.mvhd.timescale);
                println!("size: {}", video.size());
                println!("duration: {:?}", video.duration());
                for track in video.tracks().values() {
                    println!(
                        "track: #{}({}) {} : {}",
                        track.track_id(),
                        track.language(),
                        track.track_type()?,
                        track.box_type()?,
                    );
                }
            }
            DemuxerInner::Matroska {
                path,
                tracks,
                samples,
                reader: _,
                next_sample_indices: _,
                timestamp_scale_ns,
                duration_ns,
            } => {
                println!("container: Matroska ({})", path.display());
                println!("timestamp scale (ns): {}", timestamp_scale_ns);
                println!("duration (ns): {:?}", duration_ns);
                for track_id in tracks.keys().copied().collect::<Vec<_>>() {
                    let track = tracks
                        .get(&track_id)
                        .ok_or_else(|| anyhow!("Invalid track id {track_id}"))?;
                    let sample_count = samples.get(&track_id).map(Vec::len).unwrap_or(0);
                    println!(
                        "track: #{} {} {}x{} samples:{}",
                        track.track_id, track.codec, track.width, track.height, sample_count,
                    );
                }
            }
        }
        Ok(())
    }
}

fn codec_from_mp4_fourcc(fourcc: mp4::FourCC) -> anyhow::Result<VideoCodec> {
    match fourcc {
        value if value == mp4::FourCC::from(*b"avc1") || value == mp4::FourCC::from(*b"avc3") => {
            Ok(VideoCodec::H264)
        }
        value if value == mp4::FourCC::from(*b"vp08") => Ok(VideoCodec::Vp8),
        value if value == mp4::FourCC::from(*b"vp09") => Ok(VideoCodec::Vp9),
        value if value == mp4::FourCC::from(*b"av01") => Ok(VideoCodec::Av1),
        _ => bail!("Unsupported codec {}", fourcc),
    }
}

fn codec_from_matroska_id(codec_id: &str) -> Option<VideoCodec> {
    match codec_id {
        "V_MPEG4/ISO/AVC" => Some(VideoCodec::H264),
        "V_VP8" => Some(VideoCodec::Vp8),
        "V_VP9" => Some(VideoCodec::Vp9),
        "V_AV1" => Some(VideoCodec::Av1),
        _ => None,
    }
}

fn parse_matroska_h264_codec_private(
    track_id: u32,
    width: u32,
    height: u32,
    timescale: u32,
    codec_private: Option<&[u8]>,
) -> anyhow::Result<H264TrackConfig> {
    let codec_private = codec_private
        .ok_or_else(|| anyhow!("Missing H.264 codec private data for track {track_id}"))?;
    if codec_private.len() < 7 {
        bail!("Invalid H.264 codec private data for track {track_id}")
    }
    if codec_private[0] != 1 {
        bail!("Unsupported H.264 codec private version for track {track_id}")
    }

    let nal_length_size = usize::from((codec_private[4] & 0b11) + 1);
    let mut cursor = 5usize;

    let sps_count = usize::from(codec_private[cursor] & 0b1_1111);
    cursor += 1;

    let mut sequence_parameter_sets = Vec::with_capacity(sps_count);
    for _ in 0..sps_count {
        let sps = read_length_prefixed_blob(codec_private, &mut cursor)
            .with_context(|| format!("Invalid SPS in track {track_id}"))?;
        sequence_parameter_sets.push(sps.to_vec());
    }

    if cursor >= codec_private.len() {
        bail!("Missing PPS count in H.264 codec private data for track {track_id}")
    }
    let pps_count = usize::from(codec_private[cursor]);
    cursor += 1;

    let mut picture_parameter_sets = Vec::with_capacity(pps_count);
    for _ in 0..pps_count {
        let pps = read_length_prefixed_blob(codec_private, &mut cursor)
            .with_context(|| format!("Invalid PPS in track {track_id}"))?;
        picture_parameter_sets.push(pps.to_vec());
    }

    if sequence_parameter_sets.is_empty() || picture_parameter_sets.is_empty() {
        bail!("Incomplete H.264 codec private data for track {track_id}")
    }

    Ok(H264TrackConfig {
        track_id,
        width: width.min(u32::from(u16::MAX)) as u16,
        height: height.min(u32::from(u16::MAX)) as u16,
        timescale,
        sequence_parameter_sets,
        picture_parameter_sets,
        nal_length_size,
    })
}

fn read_length_prefixed_blob<'a>(data: &'a [u8], cursor: &mut usize) -> anyhow::Result<&'a [u8]> {
    if *cursor + 2 > data.len() {
        bail!("Unexpected end of codec private data")
    }
    let len = u16::from_be_bytes([data[*cursor], data[*cursor + 1]]) as usize;
    *cursor += 2;
    if *cursor + len > data.len() {
        bail!("Codec private length exceeds buffer")
    }
    let slice = &data[*cursor..*cursor + len];
    *cursor += len;
    Ok(slice)
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
