use std::{
    fs::File,
    io::{BufReader, Read, Seek},
    path::{Path, PathBuf},
};

use anyhow::{anyhow, bail, Context};

use super::codec_config::{codec_from_mp4_fourcc, parse_hevc_decoder_configuration_record};
use super::{H264TrackConfig, H265TrackConfig, VideoCodec, VideoSample, VideoTrackConfig};

pub(super) struct Mp4Demuxer {
    path: PathBuf,
    video: mp4::Mp4Reader<BufReader<File>>,
}

impl Mp4Demuxer {
    pub(super) fn open(file_path: &Path) -> anyhow::Result<Self> {
        let file = File::open(file_path)?;
        let size = file.metadata()?.len();
        let reader = BufReader::new(file);
        let video = mp4::Mp4Reader::read_header(reader, size)?;

        Ok(Self {
            path: file_path.to_path_buf(),
            video,
        })
    }

    pub(super) fn track_ids(&self) -> Vec<u32> {
        self.video.tracks().keys().copied().collect()
    }

    pub(super) fn track_config(&mut self, track_id: u32) -> anyhow::Result<VideoTrackConfig> {
        let track = self
            .video
            .tracks()
            .get(&track_id)
            .ok_or_else(|| anyhow!("Invalid track id {track_id}"))?;
        let h265 = parse_mp4_hevc_track_config(
            &self.path,
            track_id,
            u32::from(track.width()),
            u32::from(track.height()),
            track.timescale(),
        )?;
        let codec = match track.box_type() {
            Ok(fourcc) => codec_from_mp4_fourcc(fourcc)?,
            Err(_) if h265.is_some() => VideoCodec::H265,
            Err(err) => {
                return Err(anyhow!(
                    "Failed to get codec string for track {track_id}: {err}"
                ));
            }
        };

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
            h265: match codec {
                VideoCodec::H265 => Some(h265.ok_or_else(|| {
                    anyhow!("Missing H.265 decoder configuration for track {track_id}")
                })?),
                _ => None,
            },
        })
    }

    pub(super) fn sample_count(&mut self, track_id: u32) -> anyhow::Result<u32> {
        Ok(self.video.sample_count(track_id)?)
    }

    pub(super) fn read_sample(
        &mut self,
        track_id: u32,
        sample_id: u32,
    ) -> anyhow::Result<Option<VideoSample>> {
        Ok(self
            .video
            .read_sample(track_id, sample_id)?
            .map(|sample| VideoSample {
                bytes: sample.bytes.to_vec(),
                start_time: sample.start_time,
                duration: u64::from(sample.duration),
                rendering_offset: i64::from(sample.rendering_offset),
                is_sync: sample.is_sync,
            }))
    }

    pub(super) fn print_debug_info(&mut self) -> anyhow::Result<()> {
        println!("major brand: {}", self.video.ftyp.major_brand);
        println!("timescale: {}", self.video.moov.mvhd.timescale);
        println!("size: {}", self.video.size());
        println!("duration: {:?}", self.video.duration());
        for track in self.video.tracks().values() {
            println!(
                "track: #{}({}) {} : {}",
                track.track_id(),
                track.language(),
                track.track_type()?,
                track.box_type()?,
            );
        }
        Ok(())
    }
}

fn parse_mp4_hevc_track_config(
    file_path: &Path,
    target_track_id: u32,
    width: u32,
    height: u32,
    timescale: u32,
) -> anyhow::Result<Option<H265TrackConfig>> {
    let file = File::open(file_path)
        .with_context(|| format!("Failed to open MP4 container {}", file_path.display()))?;
    let size = file.metadata()?.len();
    let mut reader = BufReader::new(file);

    while reader.stream_position()? < size {
        let header = mp4::BoxHeader::read(&mut reader)?;
        let end = mp4::box_start(&mut reader)? + header.size;
        match header.name {
            mp4::BoxType::MoovBox => {
                let config = parse_mp4_moov_for_hevc_track(
                    &mut reader,
                    end,
                    target_track_id,
                    width,
                    height,
                    timescale,
                )?;
                mp4::skip_bytes_to(&mut reader, end)?;
                return Ok(config);
            }
            _ => mp4::skip_bytes_to(&mut reader, end)?,
        }
    }

    Ok(None)
}

fn parse_mp4_moov_for_hevc_track<R: Read + Seek>(
    reader: &mut R,
    end: u64,
    target_track_id: u32,
    width: u32,
    height: u32,
    timescale: u32,
) -> anyhow::Result<Option<H265TrackConfig>> {
    while reader.stream_position()? < end {
        let header = mp4::BoxHeader::read(reader)?;
        let box_end = mp4::box_start(reader)? + header.size;
        match header.name {
            mp4::BoxType::TrakBox => {
                if let Some(config) = parse_mp4_trak_for_hevc_track(
                    reader,
                    box_end,
                    target_track_id,
                    width,
                    height,
                    timescale,
                )? {
                    mp4::skip_bytes_to(reader, box_end)?;
                    return Ok(Some(config));
                }
            }
            _ => {}
        }
        mp4::skip_bytes_to(reader, box_end)?;
    }

    Ok(None)
}

fn parse_mp4_trak_for_hevc_track<R: Read + Seek>(
    reader: &mut R,
    end: u64,
    target_track_id: u32,
    width: u32,
    height: u32,
    timescale: u32,
) -> anyhow::Result<Option<H265TrackConfig>> {
    let mut track_id = None;
    let mut hvcc = None;

    while reader.stream_position()? < end {
        let header = mp4::BoxHeader::read(reader)?;
        let box_end = mp4::box_start(reader)? + header.size;
        match header.name {
            mp4::BoxType::TkhdBox => {
                track_id = Some(parse_mp4_tkhd_track_id(reader, box_end)?);
            }
            mp4::BoxType::MdiaBox => {
                hvcc = parse_mp4_mdia_for_hevc_track(reader, box_end)?;
            }
            _ => {}
        }
        mp4::skip_bytes_to(reader, box_end)?;
    }

    if track_id == Some(target_track_id) {
        return match hvcc {
            Some(hvcc) => parse_hevc_decoder_configuration_record(
                target_track_id,
                width,
                height,
                timescale,
                &hvcc,
            )
            .map(Some),
            None => Ok(None),
        };
    }

    Ok(None)
}

fn parse_mp4_mdia_for_hevc_track<R: Read + Seek>(
    reader: &mut R,
    end: u64,
) -> anyhow::Result<Option<Vec<u8>>> {
    while reader.stream_position()? < end {
        let header = mp4::BoxHeader::read(reader)?;
        let box_end = mp4::box_start(reader)? + header.size;
        match header.name {
            mp4::BoxType::MinfBox => {
                let hvcc = parse_mp4_minf_for_hevc_track(reader, box_end)?;
                mp4::skip_bytes_to(reader, box_end)?;
                return Ok(hvcc);
            }
            _ => mp4::skip_bytes_to(reader, box_end)?,
        }
    }

    Ok(None)
}

fn parse_mp4_minf_for_hevc_track<R: Read + Seek>(
    reader: &mut R,
    end: u64,
) -> anyhow::Result<Option<Vec<u8>>> {
    while reader.stream_position()? < end {
        let header = mp4::BoxHeader::read(reader)?;
        let box_end = mp4::box_start(reader)? + header.size;
        match header.name {
            mp4::BoxType::StblBox => {
                let hvcc = parse_mp4_stbl_for_hevc_track(reader, box_end)?;
                mp4::skip_bytes_to(reader, box_end)?;
                return Ok(hvcc);
            }
            _ => mp4::skip_bytes_to(reader, box_end)?,
        }
    }

    Ok(None)
}

fn parse_mp4_stbl_for_hevc_track<R: Read + Seek>(
    reader: &mut R,
    end: u64,
) -> anyhow::Result<Option<Vec<u8>>> {
    while reader.stream_position()? < end {
        let header = mp4::BoxHeader::read(reader)?;
        let box_end = mp4::box_start(reader)? + header.size;
        match header.name {
            mp4::BoxType::StsdBox => {
                let hvcc = parse_mp4_stsd_for_hevc_track(reader, box_end)?;
                mp4::skip_bytes_to(reader, box_end)?;
                return Ok(hvcc);
            }
            _ => mp4::skip_bytes_to(reader, box_end)?,
        }
    }

    Ok(None)
}

fn parse_mp4_stsd_for_hevc_track<R: Read + Seek>(
    reader: &mut R,
    end: u64,
) -> anyhow::Result<Option<Vec<u8>>> {
    let _version_and_flags = read_be_u32_from_reader(reader)?;
    let entry_count = read_be_u32_from_reader(reader)?;

    for _ in 0..entry_count {
        let header = mp4::BoxHeader::read(reader)?;
        let box_end = mp4::box_start(reader)? + header.size;
        let is_hevc_entry = matches!(header.name, mp4::BoxType::Hev1Box)
            || matches!(header.name, mp4::BoxType::UnknownBox(value) if value == u32::from_be_bytes(*b"hvc1"));
        if is_hevc_entry {
            let hvcc = parse_mp4_hevc_sample_entry(reader, box_end)?;
            mp4::skip_bytes_to(reader, box_end)?;
            if hvcc.is_some() {
                mp4::skip_bytes_to(reader, end)?;
                return Ok(hvcc);
            }
        } else {
            mp4::skip_bytes_to(reader, box_end)?;
        }
    }

    mp4::skip_bytes_to(reader, end)?;
    Ok(None)
}

fn parse_mp4_hevc_sample_entry<R: Read + Seek>(
    reader: &mut R,
    end: u64,
) -> anyhow::Result<Option<Vec<u8>>> {
    const VISUAL_SAMPLE_ENTRY_HEADER_SIZE: u64 = 78;

    if end.saturating_sub(reader.stream_position()?) < VISUAL_SAMPLE_ENTRY_HEADER_SIZE {
        bail!("Invalid HEVC sample entry")
    }
    mp4::skip_bytes(reader, VISUAL_SAMPLE_ENTRY_HEADER_SIZE)?;

    while reader.stream_position()? < end {
        let header = mp4::BoxHeader::read(reader)?;
        let box_end = mp4::box_start(reader)? + header.size;
        if matches!(header.name, mp4::BoxType::HvcCBox) {
            let payload_size = usize::try_from(header.size.saturating_sub(8))
                .context("HEVC configuration box is too large")?;
            let mut payload = vec![0; payload_size];
            reader.read_exact(&mut payload)?;
            mp4::skip_bytes_to(reader, box_end)?;
            return Ok(Some(payload));
        }
        mp4::skip_bytes_to(reader, box_end)?;
    }

    Ok(None)
}

fn parse_mp4_tkhd_track_id<R: Read + Seek>(reader: &mut R, end: u64) -> anyhow::Result<u32> {
    let version = {
        let mut buf = [0u8; 4];
        reader.read_exact(&mut buf)?;
        buf[0]
    };

    let track_id = if version == 1 {
        mp4::skip_bytes(reader, 16)?;
        let track_id = read_be_u32_from_reader(reader)?;
        mp4::skip_bytes(reader, 4)?;
        track_id
    } else if version == 0 {
        mp4::skip_bytes(reader, 8)?;
        let track_id = read_be_u32_from_reader(reader)?;
        mp4::skip_bytes(reader, 4)?;
        track_id
    } else {
        bail!("Unsupported tkhd version {version}")
    };

    mp4::skip_bytes_to(reader, end)?;
    Ok(track_id)
}

fn read_be_u32_from_reader<R: Read>(reader: &mut R) -> anyhow::Result<u32> {
    let mut buf = [0u8; 4];
    reader.read_exact(&mut buf)?;
    Ok(u32::from_be_bytes(buf))
}
