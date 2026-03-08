use super::annexb::{hevc_sample_to_annex_b, sample_to_annex_b};
use super::*;
use crate::demuxer::VideoSample;

type PrimeFrameCallback<'a> = dyn FnMut(PrimeDmabufFrame) -> anyhow::Result<()> + 'a;
type CpuNv12FrameCallback<'a> = dyn FnMut(CpuNv12Frame) -> anyhow::Result<()> + 'a;
type CpuRgbaFrameCallback<'a> = dyn FnMut(CpuRgbaFrame) -> anyhow::Result<()> + 'a;

enum PacketHandler<'a> {
    ExportSummary { export_frame_limit: usize },
    PrimeFrames(&'a mut PrimeFrameCallback<'a>),
    CpuNv12Frames(&'a mut CpuNv12FrameCallback<'a>),
    CpuRgbaFrames(&'a mut CpuRgbaFrameCallback<'a>),
}

impl VaapiBackend {
    pub fn decode_h264_mp4_track(
        &mut self,
        demuxer: &mut Demuxer,
        track_id: u32,
        export_frame_limit: usize,
    ) -> anyhow::Result<DecodeReport> {
        self.ensure_decoder(VideoCodec::H264)?;
        let config = demuxer.get_h264_track_config(track_id)?;
        let report = self.decode_track_samples(
            demuxer,
            track_id,
            config.timescale,
            PacketHandler::ExportSummary { export_frame_limit },
            |backend, sample| {
                sample_to_annex_b(
                    &config,
                    &sample.bytes,
                    sample.is_sync,
                    &mut backend.parameter_sets_sent,
                )
            },
            |sample| sample.start_time,
        )?;

        self.finish_decode_report(report)
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
        self.ensure_decoder(VideoCodec::H264)?;
        let config = demuxer.get_h264_track_config(track_id)?;
        let report = self.decode_track_samples(
            demuxer,
            track_id,
            config.timescale,
            PacketHandler::PrimeFrames(&mut on_frame),
            |backend, sample| {
                sample_to_annex_b(
                    &config,
                    &sample.bytes,
                    sample.is_sync,
                    &mut backend.parameter_sets_sent,
                )
            },
            sample_presentation_timestamp,
        )?;

        self.finish_decode_report(report)
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
        self.ensure_decoder(VideoCodec::H264)?;
        let config = demuxer.get_h264_track_config(track_id)?;
        let report = self.decode_track_samples(
            demuxer,
            track_id,
            config.timescale,
            PacketHandler::CpuNv12Frames(&mut on_frame),
            |backend, sample| {
                sample_to_annex_b(
                    &config,
                    &sample.bytes,
                    sample.is_sync,
                    &mut backend.parameter_sets_sent,
                )
            },
            |sample| sample.start_time,
        )?;

        self.finish_decode_report(report)
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
        self.ensure_decoder(VideoCodec::H264)?;
        let config = demuxer.get_h264_track_config(track_id)?;
        let report = self.decode_track_samples(
            demuxer,
            track_id,
            config.timescale,
            PacketHandler::CpuRgbaFrames(&mut on_frame),
            |backend, sample| {
                sample_to_annex_b(
                    &config,
                    &sample.bytes,
                    sample.is_sync,
                    &mut backend.parameter_sets_sent,
                )
            },
            |sample| sample.start_time,
        )?;

        self.finish_decode_report(report)
    }

    pub fn decode_video_track_with_prime_frames<F>(
        &mut self,
        demuxer: &mut Demuxer,
        track_id: u32,
        mut on_frame: F,
    ) -> anyhow::Result<DecodeReport>
    where
        F: FnMut(PrimeDmabufFrame) -> anyhow::Result<()>,
    {
        let track = demuxer.get_track_config(track_id)?;
        self.ensure_decoder(track.codec)?;

        let report = match track.codec {
            VideoCodec::H264 => self.decode_track_samples(
                demuxer,
                track.track_id,
                track.timescale,
                PacketHandler::PrimeFrames(&mut on_frame),
                |backend, sample| {
                    let config = track.h264.as_ref().ok_or_else(|| {
                        anyhow!("Track {} is missing H.264 configuration", track.track_id)
                    })?;
                    sample_to_annex_b(
                        config,
                        &sample.bytes,
                        sample.is_sync,
                        &mut backend.parameter_sets_sent,
                    )
                },
                sample_presentation_timestamp,
            )?,
            VideoCodec::H265 => self.decode_track_samples(
                demuxer,
                track.track_id,
                track.timescale,
                PacketHandler::PrimeFrames(&mut on_frame),
                |backend, sample| {
                    let config = track.h265.as_ref().ok_or_else(|| {
                        anyhow!("Track {} is missing H.265 configuration", track.track_id)
                    })?;
                    hevc_sample_to_annex_b(
                        config,
                        &sample.bytes,
                        sample.is_sync,
                        &mut backend.parameter_sets_sent,
                    )
                },
                sample_presentation_timestamp,
            )?,
            VideoCodec::Vp8 | VideoCodec::Vp9 | VideoCodec::Av1 => self.decode_track_samples(
                demuxer,
                track.track_id,
                track.timescale,
                PacketHandler::PrimeFrames(&mut on_frame),
                |_, sample| Ok(sample.bytes.clone()),
                sample_presentation_timestamp,
            )?,
        };

        self.finish_decode_report(report)
    }

    fn decode_track_samples<'a, P, T>(
        &mut self,
        demuxer: &mut Demuxer,
        track_id: u32,
        timescale: u32,
        mut packet_handler: PacketHandler<'a>,
        mut packet_transform: P,
        timestamp_for_sample: T,
    ) -> anyhow::Result<DecodeReport>
    where
        P: FnMut(&mut Self, &VideoSample) -> anyhow::Result<Vec<u8>>,
        T: Fn(&VideoSample) -> u64,
    {
        let sample_count = demuxer.sample_count(track_id)?;
        let mut report = Self::new_decode_report(track_id, timescale, &packet_handler);

        for sample_id in 1..=sample_count {
            let Some(sample) = demuxer.read_sample(track_id, sample_id)? else {
                continue;
            };
            let packet = packet_transform(self, &sample)
                .with_context(|| format!("Failed to convert sample {sample_id} to packet"))?;
            self.decode_packet(
                timestamp_for_sample(&sample),
                &packet,
                &mut report,
                &mut packet_handler,
            )
            .with_context(|| format!("Failed to decode sample {sample_id}"))?;
            report.packets_decoded += 1;
        }

        self.flush_decoder(&mut report, &mut packet_handler)?;
        Ok(report)
    }

    fn new_decode_report(
        track_id: u32,
        timescale: u32,
        packet_handler: &PacketHandler<'_>,
    ) -> DecodeReport {
        let exported_frames = match packet_handler {
            PacketHandler::ExportSummary { export_frame_limit } => {
                Vec::with_capacity((*export_frame_limit).min(16))
            }
            _ => Vec::new(),
        };

        DecodeReport {
            track_id,
            timescale,
            packets_decoded: 0,
            frames_decoded: 0,
            exported_frames,
        }
    }

    fn finish_decode_report(&mut self, report: DecodeReport) -> anyhow::Result<DecodeReport> {
        if report.frames_decoded == 0 {
            bail!("Decoder finished without producing any frame")
        }

        Ok(report)
    }

    fn flush_decoder(
        &mut self,
        report: &mut DecodeReport,
        packet_handler: &mut PacketHandler<'_>,
    ) -> anyhow::Result<()> {
        self.decoder
            .flush()
            .map_err(|err| anyhow!("Failed to flush decoder: {err:?}"))?;
        self.handle_packet_events(report, packet_handler)?;
        Ok(())
    }

    fn decode_packet(
        &mut self,
        timestamp: u64,
        packet: &[u8],
        report: &mut DecodeReport,
        packet_handler: &mut PacketHandler<'_>,
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
                    self.handle_packet_events(report, packet_handler)?;
                }
                Err(DecodeError::NotEnoughOutputBuffers(_)) => {
                    let drained = self.handle_packet_events(report, packet_handler)?;
                    if drained == 0 {
                        bail!("Decoder ran out of output buffers and no frame became available")
                    }
                }
                Err(err) => return Err(anyhow!("Decoder error: {err:?}")),
            }
        }

        self.handle_packet_events(report, packet_handler)?;
        Ok(())
    }

    fn handle_packet_events(
        &mut self,
        report: &mut DecodeReport,
        packet_handler: &mut PacketHandler<'_>,
    ) -> anyhow::Result<usize> {
        match packet_handler {
            PacketHandler::ExportSummary { export_frame_limit } => {
                self.handle_decoder_events(*export_frame_limit, report)
            }
            PacketHandler::PrimeFrames(on_frame) => {
                self.handle_decoder_events_with_callback(report, *on_frame)
            }
            PacketHandler::CpuNv12Frames(on_frame) => {
                self.handle_decoder_events_with_cpu_callback(report, *on_frame)
            }
            PacketHandler::CpuRgbaFrames(on_frame) => {
                self.handle_decoder_events_with_rgba_callback(report, *on_frame)
            }
        }
    }
}
