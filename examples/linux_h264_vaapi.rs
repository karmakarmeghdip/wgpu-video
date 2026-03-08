#[cfg(target_os = "linux")]
fn main() -> anyhow::Result<()> {
    use std::env;
    use std::path::Path;

    use wgpu_video::VaapiBackend;
    use wgpu_video::demuxer::{Demuxer, VideoCodec};

    let input = env::args()
        .nth(1)
        .unwrap_or_else(|| "examples/asset/test.mp4".to_owned());

    let mut demuxer = Demuxer::new(Path::new(&input))?;
    let track_id = demuxer.find_video_track()?;
    let mut backend = VaapiBackend::new()?;
    let report = match demuxer.get_track_info(track_id)? {
        VideoCodec::H264 => backend.decode_h264_mp4_track(&mut demuxer, track_id, 8)?,
        VideoCodec::H265 | VideoCodec::Vp8 | VideoCodec::Vp9 | VideoCodec::Av1 => {
            backend.decode_video_track_with_prime_frames(&mut demuxer, track_id, |_frame| Ok(()))?
        }
    };

    println!(
        "decoded {} frames from track {} (timescale={})",
        report.frames_decoded, report.track_id, report.timescale
    );

    for (index, frame) in report.exported_frames.iter().enumerate() {
        println!(
            "frame #{index}: pts={} coded={}x{} display={}x{} drm_fourcc=0x{:08x} objects={} layers={} y_preview={:?}",
            frame.timestamp,
            frame.coded_resolution.0,
            frame.coded_resolution.1,
            frame.display_resolution.0,
            frame.display_resolution.1,
            frame.drm_fourcc,
            frame.objects.len(),
            frame.layers.len(),
            frame.y_plane_preview,
        );

        for (object_index, object) in frame.objects.iter().enumerate() {
            println!(
                "  object #{object_index}: fd={} size={} modifier=0x{:x}",
                object.fd, object.size, object.drm_format_modifier
            );
        }

        for (layer_index, layer) in frame.layers.iter().enumerate() {
            println!(
                "  layer #{layer_index}: drm_format=0x{:08x} planes={} object_index={:?} offset={:?} pitch={:?}",
                layer.drm_format, layer.num_planes, layer.object_index, layer.offset, layer.pitch,
            );
        }
    }

    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("This example is only available on Linux.");
}
