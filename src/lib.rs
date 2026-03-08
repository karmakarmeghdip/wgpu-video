mod backend;
pub mod demuxer;
mod player;

#[cfg(target_os = "linux")]
pub use backend::libva::{
    CpuNv12Frame, CpuRgbaFormat, CpuRgbaFrame, DecodeReport, ExportedDmabufFrame,
    ExportedDmabufLayer, ExportedDmabufObject, PrimeDmabufFrame, PrimeFrameMetadata, VaapiBackend,
};
#[cfg(target_os = "linux")]
pub use backend::libva_wgpu::{ImportedPlaneFrame, VaapiVulkanFrameImporter};
pub use player::{
    BackendKind, PlaybackDiagnostics, PlayerConfig, PlayerError, TickResult, VideoPlayer,
    VideoSource,
};
