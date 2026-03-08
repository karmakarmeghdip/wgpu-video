use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use anyhow::Context;
use crossbeam_channel::{bounded, Receiver, Sender};

use crate::demuxer::{sample_presentation_end_timestamp, sample_presentation_timestamp, Demuxer};
use crate::{PrimeDmabufFrame, VaapiBackend};

use super::super::PlayerError;

pub(super) struct DecodedFramePacket {
    pub frame: PrimeDmabufFrame,
    pub presentation_time: Duration,
}

#[derive(Clone, Copy)]
pub(super) struct PlaybackTiming {
    pub first_timestamp: u64,
    pub frame_interval: Duration,
    pub expected_duration: Duration,
    pub timescale: u32,
}

pub(super) fn spawn_decode_thread(
    source: PathBuf,
    queue_size: usize,
) -> Result<(Receiver<DecodedFramePacket>, PlaybackTiming), PlayerError> {
    let mut demuxer =
        Demuxer::new(&source).map_err(|err| PlayerError::DemuxError(format!("{err:#}")))?;
    let track_id = demuxer
        .find_h264_track()
        .map_err(|err| PlayerError::DemuxError(format!("{err:#}")))?;
    let playback_timing = analyze_track_timing(&mut demuxer, track_id)?;

    let (tx, rx) = bounded(queue_size.max(1));
    thread::spawn(move || {
        if let Err(err) = decode_all_frames(source, tx, playback_timing) {
            eprintln!("decoder thread failed: {err:#}");
        }
    });

    Ok((rx, playback_timing))
}

fn decode_all_frames(
    source: PathBuf,
    tx: Sender<DecodedFramePacket>,
    playback_timing: PlaybackTiming,
) -> anyhow::Result<()> {
    let mut demuxer = Demuxer::new(&source)?;
    let track_id = demuxer.find_h264_track()?;
    let mut backend = VaapiBackend::new()?;
    backend.decode_h264_mp4_track_with_prime_frames(&mut demuxer, track_id, |frame| {
        let presentation_time = timestamp_delta_to_duration(
            frame
                .metadata
                .timestamp
                .saturating_sub(playback_timing.first_timestamp),
            playback_timing.timescale,
        );
        tx.send(DecodedFramePacket {
            frame,
            presentation_time,
        })
        .context("frame queue receiver dropped")?;
        Ok(())
    })?;
    Ok(())
}

fn analyze_track_timing(
    demuxer: &mut Demuxer,
    track_id: u32,
) -> Result<PlaybackTiming, PlayerError> {
    let timescale = demuxer
        .get_h264_track_config(track_id)
        .map_err(|err| PlayerError::DemuxError(format!("{err:#}")))?
        .timescale
        .max(1);
    let sample_count = demuxer
        .sample_count(track_id)
        .map_err(|err| PlayerError::DemuxError(format!("{err:#}")))?;
    let mut previous_start_time = None;
    let mut first_start_time = None;
    let mut last_presentation_end = None;
    let mut deltas = Vec::new();

    for sample_id in 1..=sample_count {
        let sample = demuxer
            .read_sample(track_id, sample_id)
            .map_err(|err| PlayerError::DemuxError(format!("{err:#}")))?;
        let Some(sample) = sample else {
            continue;
        };
        let presentation_time = sample_presentation_timestamp(&sample);
        first_start_time.get_or_insert(presentation_time);
        last_presentation_end = Some(sample_presentation_end_timestamp(&sample));
        if let Some(previous) = previous_start_time.replace(presentation_time) {
            let delta = presentation_time.saturating_sub(previous);
            if delta != 0 {
                deltas.push(delta);
            }
        }
    }

    let first_timestamp = first_start_time.unwrap_or(0);
    let nominal_delta = if deltas.is_empty() {
        1
    } else {
        deltas.sort_unstable();
        deltas[deltas.len() / 2]
    };
    let frame_interval = timestamp_delta_to_duration(nominal_delta, timescale);
    let expected_duration = match last_presentation_end {
        Some(last_end) => {
            timestamp_delta_to_duration(last_end.saturating_sub(first_timestamp), timescale)
        }
        None => frame_interval,
    };

    Ok(PlaybackTiming {
        first_timestamp,
        frame_interval,
        expected_duration,
        timescale,
    })
}

fn timestamp_delta_to_duration(delta: u64, timescale: u32) -> Duration {
    if delta == 0 || timescale == 0 {
        return Duration::ZERO;
    }

    let nanos = (delta as u128)
        .saturating_mul(1_000_000_000u128)
        .checked_div(timescale as u128)
        .unwrap_or(0)
        .min(u64::MAX as u128) as u64;
    Duration::from_nanos(nanos)
}
