use anyhow::{anyhow, bail, Context};

use super::{H264TrackConfig, H265TrackConfig, VideoCodec};

pub(super) fn codec_from_mp4_fourcc(fourcc: mp4::FourCC) -> anyhow::Result<VideoCodec> {
    match fourcc {
        value if value == mp4::FourCC::from(*b"avc1") || value == mp4::FourCC::from(*b"avc3") => {
            Ok(VideoCodec::H264)
        }
        value if value == mp4::FourCC::from(*b"hev1") || value == mp4::FourCC::from(*b"hvc1") => {
            Ok(VideoCodec::H265)
        }
        value if value == mp4::FourCC::from(*b"vp08") => Ok(VideoCodec::Vp8),
        value if value == mp4::FourCC::from(*b"vp09") => Ok(VideoCodec::Vp9),
        value if value == mp4::FourCC::from(*b"av01") => Ok(VideoCodec::Av1),
        _ => bail!("Unsupported codec {}", fourcc),
    }
}

#[cfg(feature = "libavformat")]
pub(super) fn codec_from_ffmpeg_id(codec_id: ffmpeg_the_third::codec::Id) -> Option<VideoCodec> {
    match codec_id {
        ffmpeg_the_third::codec::Id::H264 => Some(VideoCodec::H264),
        ffmpeg_the_third::codec::Id::HEVC | ffmpeg_the_third::codec::Id::H265 => {
            Some(VideoCodec::H265)
        }
        ffmpeg_the_third::codec::Id::VP8 => Some(VideoCodec::Vp8),
        ffmpeg_the_third::codec::Id::VP9 => Some(VideoCodec::Vp9),
        ffmpeg_the_third::codec::Id::AV1 => Some(VideoCodec::Av1),
        _ => None,
    }
}

pub(super) fn codec_from_matroska_id(codec_id: &str) -> Option<VideoCodec> {
    match codec_id {
        "V_MPEG4/ISO/AVC" => Some(VideoCodec::H264),
        "V_MPEGH/ISO/HEVC" => Some(VideoCodec::H265),
        "V_VP8" => Some(VideoCodec::Vp8),
        "V_VP9" => Some(VideoCodec::Vp9),
        "V_AV1" => Some(VideoCodec::Av1),
        _ => None,
    }
}

pub(super) fn parse_matroska_h264_codec_private(
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

pub(super) fn parse_matroska_h265_codec_private(
    track_id: u32,
    width: u32,
    height: u32,
    timescale: u32,
    codec_private: Option<&[u8]>,
) -> anyhow::Result<H265TrackConfig> {
    let codec_private = codec_private
        .ok_or_else(|| anyhow!("Missing H.265 codec private data for track {track_id}"))?;
    parse_hevc_decoder_configuration_record(track_id, width, height, timescale, codec_private)
}

pub(super) fn parse_hevc_decoder_configuration_record(
    track_id: u32,
    width: u32,
    height: u32,
    timescale: u32,
    config_record: &[u8],
) -> anyhow::Result<H265TrackConfig> {
    if config_record.len() < 23 {
        bail!("Invalid H.265 decoder configuration record for track {track_id}")
    }
    if config_record[0] != 1 {
        bail!("Unsupported H.265 decoder configuration version for track {track_id}")
    }

    let nal_length_size = usize::from((config_record[21] & 0b11) + 1);
    let num_arrays = usize::from(config_record[22]);
    let mut cursor = 23usize;
    let mut video_parameter_sets = Vec::new();
    let mut sequence_parameter_sets = Vec::new();
    let mut picture_parameter_sets = Vec::new();

    for _ in 0..num_arrays {
        if cursor + 3 > config_record.len() {
            bail!("Invalid H.265 parameter arrays for track {track_id}")
        }

        let nal_unit_type = config_record[cursor] & 0b0011_1111;
        cursor += 1;
        let num_nalus = read_be_u16(config_record, &mut cursor)? as usize;

        for _ in 0..num_nalus {
            let nal = read_length_prefixed_blob(config_record, &mut cursor)
                .with_context(|| format!("Invalid H.265 parameter set in track {track_id}"))?;
            match nal_unit_type {
                32 => video_parameter_sets.push(nal.to_vec()),
                33 => sequence_parameter_sets.push(nal.to_vec()),
                34 => picture_parameter_sets.push(nal.to_vec()),
                _ => {}
            }
        }
    }

    if video_parameter_sets.is_empty()
        || sequence_parameter_sets.is_empty()
        || picture_parameter_sets.is_empty()
    {
        bail!("Incomplete H.265 decoder configuration record for track {track_id}")
    }

    Ok(H265TrackConfig {
        track_id,
        width: width.min(u32::from(u16::MAX)) as u16,
        height: height.min(u32::from(u16::MAX)) as u16,
        timescale,
        video_parameter_sets,
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

fn read_be_u16(data: &[u8], cursor: &mut usize) -> anyhow::Result<u16> {
    if *cursor + 2 > data.len() {
        bail!("Unexpected end of codec private data")
    }
    let value = u16::from_be_bytes([data[*cursor], data[*cursor + 1]]);
    *cursor += 2;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_known_mp4_fourcc_values() {
        assert_eq!(
            codec_from_mp4_fourcc(mp4::FourCC::from(*b"avc1")).unwrap(),
            VideoCodec::H264
        );
        assert_eq!(
            codec_from_mp4_fourcc(mp4::FourCC::from(*b"hvc1")).unwrap(),
            VideoCodec::H265
        );
        assert_eq!(
            codec_from_mp4_fourcc(mp4::FourCC::from(*b"vp09")).unwrap(),
            VideoCodec::Vp9
        );
        assert_eq!(
            codec_from_mp4_fourcc(mp4::FourCC::from(*b"av01")).unwrap(),
            VideoCodec::Av1
        );
    }

    #[test]
    fn maps_known_matroska_codec_ids() {
        assert_eq!(
            codec_from_matroska_id("V_MPEG4/ISO/AVC"),
            Some(VideoCodec::H264)
        );
        assert_eq!(
            codec_from_matroska_id("V_MPEGH/ISO/HEVC"),
            Some(VideoCodec::H265)
        );
        assert_eq!(codec_from_matroska_id("V_VP8"), Some(VideoCodec::Vp8));
        assert_eq!(codec_from_matroska_id("unknown"), None);
    }

    #[test]
    fn parses_h264_codec_private() {
        let config = parse_matroska_h264_codec_private(
            7,
            1920,
            1080,
            90_000,
            Some(&[
                1, 100, 0, 31, 0xff, 0xe1, 0x00, 0x02, 0x67, 0x64, 0x01, 0x00, 0x01, 0x68,
            ]),
        )
        .unwrap();

        assert_eq!(config.track_id, 7);
        assert_eq!(config.width, 1920);
        assert_eq!(config.height, 1080);
        assert_eq!(config.timescale, 90_000);
        assert_eq!(config.nal_length_size, 4);
        assert_eq!(config.sequence_parameter_sets, vec![vec![0x67, 0x64]]);
        assert_eq!(config.picture_parameter_sets, vec![vec![0x68]]);
    }

    #[test]
    fn parses_h265_decoder_configuration_record() {
        let config = parse_hevc_decoder_configuration_record(
            9,
            3840,
            2160,
            1_000,
            &[
                1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xff, 3, 32, 0, 1,
                0, 2, 0x40, 0x01, 33, 0, 1, 0, 2, 0x42, 0x01, 34, 0, 1, 0, 2, 0x44, 0x01,
            ],
        )
        .unwrap();

        assert_eq!(config.track_id, 9);
        assert_eq!(config.width, 3840);
        assert_eq!(config.height, 2160);
        assert_eq!(config.timescale, 1_000);
        assert_eq!(config.nal_length_size, 4);
        assert_eq!(config.video_parameter_sets, vec![vec![0x40, 0x01]]);
        assert_eq!(config.sequence_parameter_sets, vec![vec![0x42, 0x01]]);
        assert_eq!(config.picture_parameter_sets, vec![vec![0x44, 0x01]]);
    }

    #[test]
    fn rejects_incomplete_h265_decoder_configuration_record() {
        let mut config = [0u8; 23];
        config[0] = 1;
        config[21] = 0xff;
        let err = parse_hevc_decoder_configuration_record(1, 640, 480, 1_000, &config).unwrap_err();
        assert!(err.to_string().contains("Incomplete H.265"));
    }
}
