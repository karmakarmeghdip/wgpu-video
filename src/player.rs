use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[cfg(target_os = "linux")]
mod linux;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    Auto,
    #[cfg(target_os = "linux")]
    Libva,
    #[cfg(target_os = "windows")]
    Wmf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VideoSource {
    Path(PathBuf),
}

impl VideoSource {
    pub fn path(&self) -> &Path {
        match self {
            Self::Path(path) => path.as_path(),
        }
    }
}

impl From<PathBuf> for VideoSource {
    fn from(value: PathBuf) -> Self {
        Self::Path(value)
    }
}

impl From<&Path> for VideoSource {
    fn from(value: &Path) -> Self {
        Self::Path(value.to_path_buf())
    }
}

impl From<&str> for VideoSource {
    fn from(value: &str) -> Self {
        Self::Path(PathBuf::from(value))
    }
}

impl From<String> for VideoSource {
    fn from(value: String) -> Self {
        Self::Path(PathBuf::from(value))
    }
}

#[derive(Debug, Clone)]
pub struct PlayerConfig {
    pub target_format: wgpu::TextureFormat,
    pub autoplay: bool,
    pub loop_playback: bool,
    pub backend: BackendKind,
    pub decode_queue_size: usize,
}

impl Default for PlayerConfig {
    fn default() -> Self {
        Self {
            target_format: wgpu::TextureFormat::Bgra8Unorm,
            autoplay: true,
            loop_playback: false,
            backend: BackendKind::Auto,
            decode_queue_size: 8,
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct TickResult {
    pub presented_frame: bool,
    pub reached_end: bool,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct PlaybackDiagnostics {
    pub presented_frames: u64,
    pub dropped_frames: u64,
    pub late_frames: u64,
    pub buffered_frames: usize,
    pub waiting_for_preroll: bool,
    pub last_frame_lateness: Duration,
    pub max_frame_lateness: Duration,
}

#[derive(Debug)]
pub enum PlayerError {
    IoError(std::io::Error),
    DemuxError(String),
    DecoderError(String),
    WgpuInteropError(String),
    Unsupported(String),
}

impl std::fmt::Display for PlayerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::IoError(err) => write!(f, "I/O error: {err}"),
            Self::DemuxError(err) => write!(f, "Demux error: {err}"),
            Self::DecoderError(err) => write!(f, "Decoder error: {err}"),
            Self::WgpuInteropError(err) => write!(f, "wgpu interop error: {err}"),
            Self::Unsupported(err) => write!(f, "Unsupported operation: {err}"),
        }
    }
}

impl std::error::Error for PlayerError {}

impl From<std::io::Error> for PlayerError {
    fn from(value: std::io::Error) -> Self {
        Self::IoError(value)
    }
}

pub(crate) trait PlayerBackend: Send + Sync {
    fn poll(&mut self) -> Result<TickResult, PlayerError>;
    fn next_frame_deadline(&self) -> Option<Instant>;
    fn diagnostics(&self) -> PlaybackDiagnostics;
    fn texture_view(&self) -> Option<&wgpu::TextureView>;
    fn dimensions(&self) -> (u32, u32);
    fn play(&mut self) -> Result<(), PlayerError>;
    fn pause(&mut self);
    fn is_playing(&self) -> bool;
    fn duration(&self) -> Duration;
    fn position(&self) -> Duration;
    fn seek(&mut self, target: Duration) -> Result<(), PlayerError>;
    fn backend_kind(&self) -> BackendKind;
}

pub struct VideoPlayer {
    backend: Box<dyn PlayerBackend>,
}

impl VideoPlayer {
    pub fn new(
        device: Arc<wgpu::Device>,
        queue: Arc<wgpu::Queue>,
        source: impl Into<VideoSource>,
        config: PlayerConfig,
    ) -> Result<Self, PlayerError> {
        let source = source.into();
        let backend: Box<dyn PlayerBackend> = match config.backend {
            #[cfg(target_os = "linux")]
            BackendKind::Auto | BackendKind::Libva => {
                Box::new(linux::LibvaPlayer::new(device, queue, source, config)?)
            }
            #[cfg(target_os = "windows")]
            BackendKind::Auto | BackendKind::Wmf => {
                return Err(PlayerError::Unsupported(
                    "Windows playback backend has not been implemented yet".to_string(),
                ))
            }
            #[allow(unreachable_patterns)]
            _ => {
                return Err(PlayerError::Unsupported(
                    "No playback backend is available for this platform".to_string(),
                ))
            }
        };

        Ok(Self { backend })
    }

    pub fn open_path(
        device: Arc<wgpu::Device>,
        queue: Arc<wgpu::Queue>,
        path: impl Into<PathBuf>,
        config: PlayerConfig,
    ) -> Result<Self, PlayerError> {
        Self::new(device, queue, VideoSource::Path(path.into()), config)
    }

    pub fn poll(&mut self) -> Result<TickResult, PlayerError> {
        self.backend.poll()
    }

    pub fn next_frame_deadline(&self) -> Option<Instant> {
        self.backend.next_frame_deadline()
    }

    pub fn diagnostics(&self) -> PlaybackDiagnostics {
        self.backend.diagnostics()
    }

    pub fn texture_view(&self) -> Option<&wgpu::TextureView> {
        self.backend.texture_view()
    }

    pub fn dimensions(&self) -> (u32, u32) {
        self.backend.dimensions()
    }

    pub fn backend_kind(&self) -> BackendKind {
        self.backend.backend_kind()
    }

    pub fn play(&mut self) -> Result<(), PlayerError> {
        self.backend.play()
    }

    pub fn pause(&mut self) {
        self.backend.pause()
    }

    pub fn is_playing(&self) -> bool {
        self.backend.is_playing()
    }

    pub fn duration(&self) -> Duration {
        self.backend.duration()
    }

    pub fn position(&self) -> Duration {
        self.backend.position()
    }

    pub fn seek(&mut self, target: Duration) -> Result<(), PlayerError> {
        self.backend.seek(target)
    }
}
