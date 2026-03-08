use wgpu_video::demuxer::Demuxer;

fn main() {
    let input = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "./examples/asset/test.mp4".to_owned());
    let mut demuxer = Demuxer::new(std::path::Path::new(&input)).unwrap();
    demuxer.print_debug_info().unwrap();
}
