struct Decoder<'a> {
    device: &'a wgpu::Device,
    window: &'a winit::window::Window,
}

impl<'a> Decoder<'a> {
    pub fn new(device: &'a wgpu::Device, window: &'a winit::window::Window) -> Self {
        Self { device, window }
    }

    pub fn decode(
        &self,
        mut encoded_video_reader: impl std::io::Read,
        send: std::sync::mpsc::Sender<wgpu::Texture>,
    ) {
        let instance = vk_video::VulkanInstance::new().unwrap();
        let surface = instance
            .wgpu_instance()
            .create_surface(self.window)
            .unwrap();
        let adapter = instance.create_adapter(Some(&surface)).unwrap();
        let device = adapter
            .create_device(
                wgpu::Features::empty(),
                wgpu::ExperimentalFeatures::disabled(),
                wgpu::Limits::default(),
            )
            .unwrap();
        let mut decoder = device
            .create_wgpu_textures_decoder(vk_video::parameters::DecoderParameters::default())
            .unwrap();

        let mut buffer = vec![0; 4096];

        while let Ok(n) = encoded_video_reader.read(&mut buffer) {
            if n == 0 {
                return;
            }

            let decoded_frames = decoder
                .decode(vk_video::EncodedInputChunk {
                    data: &buffer[..n],
                    pts: None,
                })
                .unwrap();

            for frame in decoded_frames {
                send.send(frame.data).unwrap();
            }
        }
    }
}
