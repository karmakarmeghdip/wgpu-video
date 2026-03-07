use std::{fs::File, io::BufReader};

use anyhow::anyhow;

pub struct Demuxer {
    video: mp4::Mp4Reader<BufReader<File>>,
}

impl Demuxer {
    pub fn new(file_path: &std::path::Path) -> anyhow::Result<Self> {
        let file = File::open(file_path)?;
        let size = file.metadata()?.len();
        let reader = BufReader::new(file);
        let mp4 = mp4::Mp4Reader::read_header(reader, size)?;
        Ok(Self { video: mp4 })
    }

    pub fn get_tracks(&mut self) -> Vec<u32> {
        self.video.tracks().keys().copied().collect()
    }

    pub fn get_track_info(&mut self, track_id: u32) -> anyhow::Result<mp4::FourCC> {
        let track = self
            .video
            .tracks()
            .get(&track_id)
            .ok_or(anyhow::anyhow!("Invalid track id"))?;
        track
            .box_type()
            .map_err(|_| anyhow!("Failed to get codec string"))
    }

    pub fn get_track(&mut self, track_id: u32) -> anyhow::Result<&mp4::Mp4Track> {
        self.video
            .tracks()
            .get(&track_id)
            .ok_or(anyhow::anyhow!("Invalid track id"))
    }

    pub fn parse_track_packets<F>(&mut self, track_id: u32, cb: F) -> anyhow::Result<()>
    where
        F: Fn(&mp4::Mp4Sample) -> (),
    {
        let sample_count = self.video.sample_count(track_id)?;
        for i in 0..sample_count {
            let id = i + 1;
            let sample = self.video.read_sample(track_id, id)?;
            if let Some(ref samp) = sample {
                cb(samp);
            }
        }
        Ok(())
    }

    pub fn print_debug_info(&mut self) -> anyhow::Result<()> {
        // Print boxes.
        println!("major brand: {}", self.video.ftyp.major_brand);
        println!("timescale: {}", self.video.moov.mvhd.timescale);

        // Use available methods.
        println!("size: {}", self.video.size());

        let mut compatible_brands = String::new();
        for brand in self.video.compatible_brands().iter() {
            compatible_brands.push_str(&brand.to_string());
            compatible_brands.push_str(",");
        }
        println!("compatible brands: {}", compatible_brands);
        println!("duration: {:?}", self.video.duration());

        // Track info.
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
