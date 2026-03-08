mod decode;
mod renderer;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, TryRecvError};

use crate::backend::libva_wgpu::VaapiVulkanFrameImporter;

use super::{
    BackendKind, PlaybackDiagnostics, PlayerBackend, PlayerConfig, PlayerError, TickResult,
    VideoSource,
};
use decode::{spawn_decode_thread, DecodedFramePacket, PlaybackTiming};
use renderer::{OutputFrame, VideoRenderer};

pub(crate) struct LibvaPlayer {
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
    source: PathBuf,
    config: PlayerConfig,
    importer: VaapiVulkanFrameImporter,
    renderer: VideoRenderer,
    frame_rx: Receiver<DecodedFramePacket>,
    next_packet: Option<DecodedFramePacket>,
    output: Option<OutputFrame>,
    playback_timing: PlaybackTiming,
    paused_position: Duration,
    started_at: Option<Instant>,
    waiting_for_preroll: bool,
    diagnostics: PlaybackDiagnostics,
    playing: bool,
    receiver_closed: bool,
    ended: bool,
}

const PREROLL_FRAMES: usize = 3;
const PREROLL_POLL_INTERVAL: Duration = Duration::from_millis(2);

impl LibvaPlayer {
    pub(crate) fn new(
        device: Arc<wgpu::Device>,
        queue: Arc<wgpu::Queue>,
        source: VideoSource,
        config: PlayerConfig,
    ) -> Result<Self, PlayerError> {
        let source_path = match source {
            VideoSource::Path(path) => path,
        };

        if !VaapiVulkanFrameImporter::is_supported(&device) {
            return Err(PlayerError::WgpuInteropError(
                "libva playback requires a Vulkan-backed wgpu device".to_string(),
            ));
        }

        let importer = VaapiVulkanFrameImporter::new(device.clone(), queue.clone())
            .map_err(|err| PlayerError::WgpuInteropError(format!("{err:#}")))?;
        let renderer = VideoRenderer::new(&device, config.target_format);
        let (frame_rx, playback_timing) =
            spawn_decode_thread(source_path.clone(), config.decode_queue_size.max(1))?;

        Ok(Self {
            device,
            queue,
            source: source_path,
            config,
            importer,
            renderer,
            frame_rx,
            next_packet: None,
            output: None,
            playback_timing,
            paused_position: Duration::ZERO,
            started_at: None,
            waiting_for_preroll: false,
            diagnostics: PlaybackDiagnostics::default(),
            playing: false,
            receiver_closed: false,
            ended: false,
        }
        .with_autoplay())
    }

    fn with_autoplay(mut self) -> Self {
        self.playing = self.config.autoplay;
        self.waiting_for_preroll = self.playing;
        self
    }

    fn restart(&mut self) -> Result<(), PlayerError> {
        let (frame_rx, playback_timing) =
            spawn_decode_thread(self.source.clone(), self.config.decode_queue_size.max(1))?;
        self.frame_rx = frame_rx;
        self.playback_timing = playback_timing;
        self.next_packet = None;
        self.paused_position = Duration::ZERO;
        self.started_at = None;
        self.waiting_for_preroll = self.playing;
        self.receiver_closed = false;
        self.ended = false;
        Ok(())
    }

    fn playback_position(&self) -> Duration {
        if self.playing {
            self.started_at
                .map(|started_at| {
                    self.paused_position
                        .saturating_add(Instant::now().saturating_duration_since(started_at))
                })
                .unwrap_or(self.paused_position)
                .min(self.playback_timing.expected_duration)
        } else {
            self.paused_position
                .min(self.playback_timing.expected_duration)
        }
    }

    fn sync_playback_position(&mut self) -> Duration {
        let position = self.playback_position();
        if self.playing {
            self.paused_position = position;
            self.started_at = Some(Instant::now());
        }
        position
    }

    fn fill_next_packet(&mut self) {
        if self.next_packet.is_some() || self.receiver_closed {
            return;
        }

        match self.frame_rx.try_recv() {
            Ok(packet) => self.next_packet = Some(packet),
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => self.receiver_closed = true,
        }
    }

    fn buffered_frame_count(&mut self) -> usize {
        self.fill_next_packet();
        let count = usize::from(self.next_packet.is_some()) + self.frame_rx.len();
        self.diagnostics.buffered_frames = count;
        count
    }

    fn maybe_finish_preroll(&mut self) {
        if !self.waiting_for_preroll || !self.playing {
            return;
        }

        let enough_frames = self.buffered_frame_count() >= PREROLL_FRAMES;
        let no_more_frames = self.receiver_closed;

        if enough_frames || no_more_frames {
            self.waiting_for_preroll = false;
            self.started_at = Some(Instant::now());
        }
    }

    fn present_due_frames(&mut self) -> Result<bool, PlayerError> {
        let mut latest_due = None;
        let mut due_count = 0u64;
        let lead = self
            .playback_timing
            .frame_interval
            .checked_div(2)
            .unwrap_or(Duration::ZERO);
        let deadline = self.playback_position().saturating_add(lead);

        loop {
            self.fill_next_packet();
            let Some(packet) = self.next_packet.as_ref() else {
                break;
            };
            if packet.presentation_time > deadline {
                break;
            }
            due_count = due_count.saturating_add(1);
            latest_due = self.next_packet.take();
        }

        let Some(packet) = latest_due else {
            return Ok(false);
        };

        let playback_position = self.playback_position();
        let lateness = playback_position.saturating_sub(packet.presentation_time);
        self.diagnostics.last_frame_lateness = lateness;
        self.diagnostics.max_frame_lateness = self.diagnostics.max_frame_lateness.max(lateness);
        if lateness > self.playback_timing.frame_interval {
            self.diagnostics.late_frames = self.diagnostics.late_frames.saturating_add(1);
        }
        if due_count > 1 {
            self.diagnostics.dropped_frames = self
                .diagnostics
                .dropped_frames
                .saturating_add(due_count.saturating_sub(1));
        }
        self.diagnostics.presented_frames = self.diagnostics.presented_frames.saturating_add(1);

        let imported = self
            .importer
            .import_prime_frame(packet.frame)
            .map_err(|err| PlayerError::WgpuInteropError(format!("{err:#}")))?;
        self.renderer
            .render_frame(&self.device, &self.queue, &mut self.output, &imported);
        Ok(true)
    }
}

impl PlayerBackend for LibvaPlayer {
    fn poll(&mut self) -> Result<TickResult, PlayerError> {
        let mut result = TickResult::default();

        if self.playing {
            self.maybe_finish_preroll();
        }
        self.diagnostics.waiting_for_preroll = self.waiting_for_preroll;
        self.diagnostics.buffered_frames =
            usize::from(self.next_packet.is_some()) + self.frame_rx.len();

        if self.playing && !self.ended && !self.waiting_for_preroll {
            self.sync_playback_position();
            result.presented_frame = self.present_due_frames()?;
        } else {
            self.fill_next_packet();
        }

        if !self.ended
            && self.receiver_closed
            && self.next_packet.is_none()
            && self.playback_position() >= self.playback_timing.expected_duration
        {
            if self.config.loop_playback {
                self.restart()?;
                self.playing = self.config.autoplay;
            } else {
                self.paused_position = self.playback_timing.expected_duration;
                self.started_at = None;
                self.playing = false;
                self.ended = true;
                result.reached_end = true;
            }
        }

        Ok(result)
    }

    fn next_frame_deadline(&self) -> Option<Instant> {
        if !self.playing || self.ended {
            return None;
        }

        if self.waiting_for_preroll {
            return Some(Instant::now() + PREROLL_POLL_INTERVAL);
        }

        let Some(packet) = self.next_packet.as_ref() else {
            return Some(Instant::now() + Duration::from_millis(5));
        };
        let started_at = self.started_at?;
        let target_offset = packet.presentation_time;
        if target_offset <= self.paused_position {
            return Some(Instant::now());
        }

        Some(started_at + target_offset.saturating_sub(self.paused_position))
    }

    fn diagnostics(&self) -> PlaybackDiagnostics {
        let mut diagnostics = self.diagnostics;
        diagnostics.waiting_for_preroll = self.waiting_for_preroll;
        diagnostics.buffered_frames = usize::from(self.next_packet.is_some()) + self.frame_rx.len();
        diagnostics
    }

    fn texture_view(&self) -> Option<&wgpu::TextureView> {
        self.output.as_ref().map(|frame| &frame.view)
    }

    fn dimensions(&self) -> (u32, u32) {
        self.output
            .as_ref()
            .map(|frame| (frame.width, frame.height))
            .unwrap_or((0, 0))
    }

    fn play(&mut self) -> Result<(), PlayerError> {
        if self.ended {
            self.restart()?;
        }
        if !self.playing {
            self.started_at = None;
        }
        self.playing = true;
        self.waiting_for_preroll = true;
        Ok(())
    }

    fn pause(&mut self) {
        self.paused_position = self.playback_position();
        self.started_at = None;
        self.waiting_for_preroll = false;
        self.playing = false;
    }

    fn is_playing(&self) -> bool {
        self.playing
    }

    fn duration(&self) -> Duration {
        self.playback_timing.expected_duration
    }

    fn position(&self) -> Duration {
        self.playback_position()
    }

    fn seek(&mut self, _target: Duration) -> Result<(), PlayerError> {
        Err(PlayerError::Unsupported(
            "Seeking is not implemented for the libva backend yet".to_string(),
        ))
    }

    fn backend_kind(&self) -> BackendKind {
        BackendKind::Libva
    }
}
