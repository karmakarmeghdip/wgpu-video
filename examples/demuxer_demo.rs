use wgpu_video::demuxer::Demuxer;
fn main() {
    let mut demuxer = Demuxer::new(std::path::Path::new("./examples/asset/test.mp4")).unwrap();
    demuxer.print_debug_info().unwrap();
}
