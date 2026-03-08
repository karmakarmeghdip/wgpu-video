#[cfg(target_os = "linux")]
mod app {
    use std::fs::File;
    use std::io::Write;
    use std::path::Path;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread;
    use std::time::{Duration, Instant};

    use crossbeam_channel::{Receiver, Sender, TryRecvError, TrySendError, bounded};
    use wgpu::{
        BindGroup, BindGroupLayout, Color, ColorTargetState, CommandEncoderDescriptor,
        CompositeAlphaMode, Device, DeviceDescriptor, Features, FilterMode, FragmentState,
        FrontFace, Instance, InstanceDescriptor, Limits, LoadOp, MultisampleState, Operations,
        PipelineCompilationOptions, PipelineLayoutDescriptor, PolygonMode, PowerPreference,
        PresentMode, PrimitiveState, PrimitiveTopology, Queue, RenderPassColorAttachment,
        RenderPassDescriptor, RenderPipeline, RenderPipelineDescriptor, RequestAdapterOptions,
        Sampler, SamplerBindingType, SamplerDescriptor, ShaderModuleDescriptor, ShaderSource,
        ShaderStages, StoreOp, Surface, SurfaceConfiguration, SurfaceError, Texture, TextureAspect,
        TextureFormat, TextureSampleType, TextureUsages, TextureViewDescriptor,
        TextureViewDimension, VertexState,
    };
    use wgpu_video::{
        ImportedPlaneFrame, PrimeDmabufFrame, VaapiBackend, VaapiVulkanFrameImporter,
        demuxer::Demuxer,
    };
    use winit::{
        application::ApplicationHandler,
        event::WindowEvent,
        event_loop::{ActiveEventLoop, EventLoop},
        window::{Window, WindowId},
    };

    const VIDEO_PATH: &str = "examples/asset/test.mp4";
    static DUMPED_IMPORTED_FRAME: AtomicBool = AtomicBool::new(false);

    const VIDEO_SHADER: &str = r#"
@group(0) @binding(0)
var y_tex: texture_2d<f32>;

@group(0) @binding(1)
var uv_tex: texture_2d<f32>;

@group(0) @binding(2)
var video_sampler: sampler;

struct VSOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) idx: u32) -> VSOut {
    var out: VSOut;
    let x = f32((idx << 1u) & 2u);
    let y = f32(idx & 2u);
    out.pos = vec4<f32>(x * 2.0 - 1.0, 1.0 - y * 2.0, 0.0, 1.0);
    out.uv = vec2<f32>(x, y);
    return out;
}

fn yuv_to_rgb_bt709_limited(y: f32, uv: vec2<f32>) -> vec3<f32> {
    let y_limited = max(y - (16.0 / 255.0), 0.0) * (255.0 / 219.0);
    let u = uv.x - 0.5;
    let v = uv.y - 0.5;

    let r = y_limited + 1.5748 * v;
    let g = y_limited - 0.1873 * u - 0.4681 * v;
    let b = y_limited + 1.8556 * u;
    return clamp(vec3<f32>(r, g, b), vec3<f32>(0.0), vec3<f32>(1.0));
}

@fragment
fn fs_main(in: VSOut) -> @location(0) vec4<f32> {
    let y = textureSample(y_tex, video_sampler, in.uv).r;
    let uv = textureSample(uv_tex, video_sampler, in.uv).rg;
    let rgb = yuv_to_rgb_bt709_limited(y, uv);
    return vec4<f32>(rgb, 1.0);
}
"#;

    struct VideoRenderer {
        pipeline: RenderPipeline,
        bind_group_layout: BindGroupLayout,
        sampler: Sampler,
    }

    impl VideoRenderer {
        fn new(device: &Device, surface_format: TextureFormat) -> Self {
            let bind_group_layout =
                device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("vaapi-video-bind-group-layout"),
                    entries: &[
                        wgpu::BindGroupLayoutEntry {
                            binding: 0,
                            visibility: ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Texture {
                                sample_type: TextureSampleType::Float { filterable: true },
                                view_dimension: TextureViewDimension::D2,
                                multisampled: false,
                            },
                            count: None,
                        },
                        wgpu::BindGroupLayoutEntry {
                            binding: 1,
                            visibility: ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Texture {
                                sample_type: TextureSampleType::Float { filterable: true },
                                view_dimension: TextureViewDimension::D2,
                                multisampled: false,
                            },
                            count: None,
                        },
                        wgpu::BindGroupLayoutEntry {
                            binding: 2,
                            visibility: ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Sampler(SamplerBindingType::Filtering),
                            count: None,
                        },
                    ],
                });

            let sampler = device.create_sampler(&SamplerDescriptor {
                label: Some("vaapi-video-sampler"),
                mag_filter: FilterMode::Linear,
                min_filter: FilterMode::Linear,
                ..Default::default()
            });

            let shader = device.create_shader_module(ShaderModuleDescriptor {
                label: Some("vaapi-video-shader"),
                source: ShaderSource::Wgsl(VIDEO_SHADER.into()),
            });

            let pipeline_layout = device.create_pipeline_layout(&PipelineLayoutDescriptor {
                label: Some("vaapi-video-pipeline-layout"),
                bind_group_layouts: &[&bind_group_layout],
                immediate_size: 0,
            });

            let pipeline = device.create_render_pipeline(&RenderPipelineDescriptor {
                label: Some("vaapi-video-render-pipeline"),
                layout: Some(&pipeline_layout),
                vertex: VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    buffers: &[],
                    compilation_options: PipelineCompilationOptions::default(),
                },
                fragment: Some(FragmentState {
                    module: &shader,
                    entry_point: Some("fs_main"),
                    targets: &[Some(ColorTargetState {
                        format: surface_format,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: PipelineCompilationOptions::default(),
                }),
                primitive: PrimitiveState {
                    topology: PrimitiveTopology::TriangleList,
                    strip_index_format: None,
                    front_face: FrontFace::Ccw,
                    cull_mode: None,
                    unclipped_depth: false,
                    polygon_mode: PolygonMode::Fill,
                    conservative: false,
                },
                depth_stencil: None,
                multisample: MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            });

            Self {
                pipeline,
                bind_group_layout,
                sampler,
            }
        }

        fn bind_group_for_frame(&self, device: &Device, frame: &UploadedFrame) -> BindGroup {
            let y_view = frame
                .y_texture
                .create_view(&TextureViewDescriptor::default());
            let uv_view = frame
                .uv_texture
                .create_view(&TextureViewDescriptor::default());
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("vaapi-video-bind-group"),
                layout: &self.bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&y_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&uv_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                ],
            })
        }
    }

    struct UploadedFrame {
        timestamp: u64,
        width: u32,
        height: u32,
        y_texture: Texture,
        uv_texture: Texture,
    }

    struct DisplayedFrame {
        frame: UploadedFrame,
        bind_group: BindGroup,
    }

    struct PlaybackTiming {
        frame_interval: Duration,
        expected_duration: Duration,
    }

    enum RenderOutcome {
        Continue,
        Finished,
    }

    struct PlayerState {
        _window: Arc<Window>,
        surface: Surface<'static>,
        surface_config: SurfaceConfiguration,
        device: Arc<Device>,
        queue: Arc<Queue>,
        renderer: VideoRenderer,
        importer: VaapiVulkanFrameImporter,
        frame_rx: Receiver<PrimeDmabufFrame>,
        expected_duration: Duration,
        current_frame: Option<DisplayedFrame>,
        playback_started_at: Option<Instant>,
        playback_finished_at: Option<Instant>,
        decoder_finished: bool,
    }

    impl PlayerState {
        fn new(window: Arc<Window>) -> Result<Self, String> {
            let instance = Instance::new(&InstanceDescriptor::default());
            let surface = instance
                .create_surface(window.clone())
                .map_err(|err| format!("surface creation failed: {err}"))?;

            let adapter = pollster::block_on(instance.request_adapter(&RequestAdapterOptions {
                power_preference: PowerPreference::HighPerformance,
                force_fallback_adapter: false,
                compatible_surface: Some(&surface),
            }))
            .map_err(|err| format!("adapter request failed: {err}"))?;

            let (device, queue) = pollster::block_on(adapter.request_device(&DeviceDescriptor {
                label: Some("wgpu-video-vaapi-device"),
                required_features: Features::empty(),
                required_limits: Limits::default(),
                memory_hints: wgpu::MemoryHints::default(),
                trace: wgpu::Trace::Off,
                experimental_features: wgpu::ExperimentalFeatures::disabled(),
            }))
            .map_err(|err| format!("device request failed: {err}"))?;

            let caps = surface.get_capabilities(&adapter);
            let surface_format = caps
                .formats
                .iter()
                .copied()
                .find(|format| {
                    matches!(
                        format,
                        TextureFormat::Bgra8Unorm | TextureFormat::Rgba8Unorm
                    )
                })
                .or_else(|| {
                    caps.formats
                        .iter()
                        .copied()
                        .find(|format| !format.is_srgb())
                })
                .or_else(|| caps.formats.first().copied())
                .ok_or_else(|| "surface reports no supported formats".to_owned())?;

            let size = window.inner_size();
            let surface_config = SurfaceConfiguration {
                usage: TextureUsages::RENDER_ATTACHMENT,
                format: surface_format,
                width: size.width.max(1),
                height: size.height.max(1),
                present_mode: caps
                    .present_modes
                    .iter()
                    .copied()
                    .find(|mode| *mode == PresentMode::Fifo)
                    .unwrap_or(PresentMode::Fifo),
                alpha_mode: caps
                    .alpha_modes
                    .first()
                    .copied()
                    .unwrap_or(CompositeAlphaMode::Auto),
                view_formats: vec![],
                desired_maximum_frame_latency: 2,
            };
            surface.configure(&device, &surface_config);

            let device = Arc::new(device);
            let queue = Arc::new(queue);
            if !VaapiVulkanFrameImporter::is_supported(&device) {
                return Err("example requires a Vulkan-backed wgpu device".to_owned());
            }
            let renderer = VideoRenderer::new(&device, surface_format);
            let importer = VaapiVulkanFrameImporter::new(device.clone(), queue.clone())
                .map_err(|err| format!("failed to initialize Vulkan PRIME importer: {err:#}"))?;
            let (frame_rx, playback_timing) =
                spawn_decoder_thread(VIDEO_PATH).map_err(|err| format!("{err:#}"))?;

            Ok(Self {
                _window: window,
                surface,
                surface_config,
                device,
                queue,
                renderer,
                importer,
                frame_rx,
                expected_duration: playback_timing.expected_duration,
                current_frame: None,
                playback_started_at: None,
                playback_finished_at: None,
                decoder_finished: false,
            })
        }

        fn resize(&mut self, width: u32, height: u32) {
            if width == 0 || height == 0 {
                return;
            }
            self.surface_config.width = width;
            self.surface_config.height = height;
            self.surface.configure(&self.device, &self.surface_config);
        }

        fn pump_frames(&mut self) -> Result<(), String> {
            let mut newest = None;
            loop {
                match self.frame_rx.try_recv() {
                    Ok(frame) => newest = Some(frame),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        self.decoder_finished = true;
                        break;
                    }
                }
            }

            if let Some(frame) = newest {
                let uploaded = upload_prime_frame(&mut self.importer, frame)
                    .map_err(|err| format!("failed to import decoded frame: {err:#}"))?;
                maybe_dump_imported_frame(&self.device, &self.queue, &uploaded)
                    .map_err(|err| format!("failed to dump imported frame: {err:#}"))?;
                let bind_group = self.renderer.bind_group_for_frame(&self.device, &uploaded);
                self.current_frame = Some(DisplayedFrame {
                    frame: uploaded,
                    bind_group,
                });
            }

            Ok(())
        }

        fn render(&mut self) -> Result<RenderOutcome, String> {
            self.pump_frames()?;

            let surface_texture = match self.surface.get_current_texture() {
                Ok(surface_texture) => surface_texture,
                Err(SurfaceError::Outdated | SurfaceError::Lost) => {
                    self.surface.configure(&self.device, &self.surface_config);
                    return Ok(RenderOutcome::Continue);
                }
                Err(SurfaceError::Timeout) => return Ok(RenderOutcome::Continue),
                Err(err) => return Err(format!("failed to acquire swapchain texture: {err}")),
            };
            let view = surface_texture
                .texture
                .create_view(&TextureViewDescriptor::default());
            let mut encoder = self
                .device
                .create_command_encoder(&CommandEncoderDescriptor {
                    label: Some("vaapi-video-render-encoder"),
                });

            {
                let mut pass = encoder.begin_render_pass(&RenderPassDescriptor {
                    label: Some("vaapi-video-render-pass"),
                    color_attachments: &[Some(RenderPassColorAttachment {
                        view: &view,
                        resolve_target: None,
                        depth_slice: None,
                        ops: Operations {
                            load: LoadOp::Clear(Color::BLACK),
                            store: StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });

                if let Some(frame) = self.current_frame.as_ref() {
                    self.playback_started_at.get_or_insert_with(Instant::now);
                    let _ = (frame.frame.width, frame.frame.height, frame.frame.timestamp);
                    pass.set_pipeline(&self.renderer.pipeline);
                    pass.set_bind_group(0, &frame.bind_group, &[]);
                    pass.draw(0..3, 0..1);
                }
            }

            self.queue.submit([encoder.finish()]);
            surface_texture.present();

            if self.decoder_finished {
                let finished_at = *self.playback_finished_at.get_or_insert_with(Instant::now);
                if let Some(started_at) = self.playback_started_at {
                    let actual_duration = finished_at.saturating_duration_since(started_at);
                    eprintln!(
                        "playback finished: actual={:.3}s expected={:.3}s delta={:+.3}s",
                        actual_duration.as_secs_f64(),
                        self.expected_duration.as_secs_f64(),
                        actual_duration.as_secs_f64() - self.expected_duration.as_secs_f64(),
                    );
                } else {
                    eprintln!(
                        "playback finished before any frame was presented; expected duration {:.3}s",
                        self.expected_duration.as_secs_f64()
                    );
                }
                return Ok(RenderOutcome::Finished);
            }

            Ok(RenderOutcome::Continue)
        }
    }

    fn spawn_decoder_thread(
        path: &str,
    ) -> anyhow::Result<(Receiver<PrimeDmabufFrame>, PlaybackTiming)> {
        let mut demuxer = Demuxer::new(Path::new(path))?;
        let track_id = demuxer.find_video_track()?;
        let playback_timing = analyze_track_timing(&mut demuxer, track_id)?;

        let (tx, rx) = bounded(2);
        let drop_rx = rx.clone();
        let path = path.to_owned();
        let frame_interval = playback_timing.frame_interval;
        thread::spawn(move || {
            if let Err(err) = decode_loop(&path, tx, drop_rx, frame_interval) {
                eprintln!("decoder thread failed: {err:#}");
            }
        });
        Ok((rx, playback_timing))
    }

    fn decode_loop(
        path: &str,
        tx: Sender<PrimeDmabufFrame>,
        drop_rx: Receiver<PrimeDmabufFrame>,
        frame_interval: Duration,
    ) -> anyhow::Result<()> {
        let mut demuxer = Demuxer::new(Path::new(path))?;
        let track_id = demuxer.find_video_track()?;
        let mut backend = VaapiBackend::new()?;
        let mut playback_start = None;
        let mut frame_index = 0u64;
        backend.decode_video_track_with_prime_frames(&mut demuxer, track_id, |frame| {
            let start_instant = *playback_start.get_or_insert_with(Instant::now);
            let target_offset = frame_interval.saturating_mul(frame_index as u32);
            let target_time = start_instant + target_offset;
            if let Some(remaining) = target_time.checked_duration_since(Instant::now()) {
                if !remaining.is_zero() {
                    thread::sleep(remaining);
                }
            }
            if !try_send_latest(&tx, &drop_rx, frame) {
                anyhow::bail!("render thread dropped the decoder channel")
            }
            frame_index = frame_index.saturating_add(1);
            Ok(())
        })?;
        Ok(())
    }

    fn analyze_track_timing(
        demuxer: &mut Demuxer,
        track_id: u32,
    ) -> anyhow::Result<PlaybackTiming> {
        let timescale = demuxer.get_track_config(track_id)?.timescale.max(1);
        let sample_count = demuxer.sample_count(track_id)?;
        let mut previous_start_time = None;
        let mut first_start_time = None;
        let mut last_start_time = None;
        let mut deltas = Vec::new();

        for sample_id in 1..=sample_count {
            let Some(sample) = demuxer.read_sample(track_id, sample_id)? else {
                continue;
            };
            first_start_time.get_or_insert(sample.start_time);
            last_start_time = Some(sample.start_time);
            if let Some(previous) = previous_start_time.replace(sample.start_time) {
                let delta = sample.start_time.saturating_sub(previous);
                if delta != 0 {
                    deltas.push(delta);
                }
            }
        }

        let nominal_delta = if deltas.is_empty() {
            1
        } else {
            deltas.sort_unstable();
            deltas[deltas.len() / 2]
        };

        let frame_interval = timestamp_delta_to_duration(nominal_delta, timescale);
        let expected_duration = match (first_start_time, last_start_time) {
            (Some(first), Some(last)) => timestamp_delta_to_duration(
                last.saturating_sub(first).saturating_add(nominal_delta),
                timescale,
            ),
            _ => frame_interval,
        };

        Ok(PlaybackTiming {
            frame_interval,
            expected_duration,
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

    fn try_send_latest(
        tx: &Sender<PrimeDmabufFrame>,
        drop_rx: &Receiver<PrimeDmabufFrame>,
        frame: PrimeDmabufFrame,
    ) -> bool {
        let mut pending = Some(frame);
        loop {
            match tx.try_send(pending.take().expect("frame should be present")) {
                Ok(()) => return true,
                Err(TrySendError::Full(frame)) => match drop_rx.try_recv() {
                    Ok(_) | Err(TryRecvError::Empty) => {
                        pending = Some(frame);
                    }
                    Err(TryRecvError::Disconnected) => return false,
                },
                Err(TrySendError::Disconnected(_)) => return false,
            }
        }
    }

    fn upload_prime_frame(
        importer: &mut VaapiVulkanFrameImporter,
        frame: PrimeDmabufFrame,
    ) -> anyhow::Result<UploadedFrame> {
        let ImportedPlaneFrame {
            timestamp,
            width,
            height,
            y_texture,
            uv_texture,
        } = importer.import_prime_frame(frame)?;

        Ok(UploadedFrame {
            timestamp,
            width,
            height,
            y_texture,
            uv_texture,
        })
    }

    fn maybe_dump_imported_frame(
        device: &Device,
        queue: &Queue,
        frame: &UploadedFrame,
    ) -> anyhow::Result<()> {
        if DUMPED_IMPORTED_FRAME.swap(true, Ordering::Relaxed) {
            return Ok(());
        }

        let y_data = read_texture_plane(
            device,
            queue,
            &frame.y_texture,
            frame.width,
            frame.height,
            1,
        )?;
        let uv_data = read_texture_plane(
            device,
            queue,
            &frame.uv_texture,
            frame.width / 2,
            frame.height / 2,
            2,
        )?;

        let dump_path = std::env::current_dir()?.join("target/imported_frame.ppm");
        std::fs::write(
            std::env::current_dir()?.join("target/imported_y.raw"),
            &y_data,
        )?;
        std::fs::write(
            std::env::current_dir()?.join("target/imported_uv.raw"),
            &uv_data,
        )?;
        write_nv12_like_ppm(
            &dump_path,
            frame.width as usize,
            frame.height as usize,
            &y_data,
            &uv_data,
        )?;

        eprintln!(
            "dumped imported frame to {} y[:16]={:?} uv[:16]={:?}",
            dump_path.display(),
            &y_data[..y_data.len().min(16)],
            &uv_data[..uv_data.len().min(16)]
        );

        Ok(())
    }

    fn read_texture_plane(
        device: &Device,
        queue: &Queue,
        texture: &Texture,
        width: u32,
        height: u32,
        bytes_per_pixel: u32,
    ) -> anyhow::Result<Vec<u8>> {
        let unpadded_bytes_per_row = width * bytes_per_pixel;
        let padded_bytes_per_row = unpadded_bytes_per_row.div_ceil(256) * 256;
        let buffer_size = padded_bytes_per_row as u64 * height as u64;

        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("video-debug-readback"),
            size: buffer_size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        let mut encoder = device.create_command_encoder(&CommandEncoderDescriptor {
            label: Some("video-debug-readback-encoder"),
        });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_bytes_per_row),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        let submission = queue.submit([encoder.finish()]);

        let slice = buffer.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = tx.send(result);
        });
        let _ = device.poll(wgpu::PollType::Wait {
            submission_index: Some(submission),
            timeout: None,
        });
        rx.recv()
            .map_err(|err| anyhow::anyhow!("failed to receive map result: {err}"))?
            .map_err(|err| anyhow::anyhow!("failed to map readback buffer: {err}"))?;

        let mapped = slice.get_mapped_range();
        let mut output = Vec::with_capacity((unpadded_bytes_per_row * height) as usize);
        for row in 0..height as usize {
            let start = row * padded_bytes_per_row as usize;
            let end = start + unpadded_bytes_per_row as usize;
            output.extend_from_slice(&mapped[start..end]);
        }
        drop(mapped);
        buffer.unmap();

        Ok(output)
    }

    fn write_nv12_like_ppm(
        path: &std::path::Path,
        width: usize,
        height: usize,
        y_plane: &[u8],
        uv_plane: &[u8],
    ) -> anyhow::Result<()> {
        let mut file = File::create(path)?;
        write!(file, "P6\n{} {}\n255\n", width, height)?;

        for y in 0..height {
            for x in 0..width {
                let luma = y_plane[y * width + x] as f32;
                let uv_index = (y / 2) * width + (x / 2) * 2;
                let u = uv_plane[uv_index] as f32;
                let v = uv_plane[uv_index + 1] as f32;

                let y_limited = (luma - 16.0).max(0.0) * (255.0 / 219.0);
                let u = u - 128.0;
                let v = v - 128.0;
                let r = (y_limited + 1.5748 * v).clamp(0.0, 255.0) as u8;
                let g = (y_limited - 0.1873 * u - 0.4681 * v).clamp(0.0, 255.0) as u8;
                let b = (y_limited + 1.8556 * u).clamp(0.0, 255.0) as u8;
                file.write_all(&[r, g, b])?;
            }
        }

        Ok(())
    }

    #[derive(Default)]
    struct App {
        state: Option<PlayerState>,
    }

    impl ApplicationHandler for App {
        fn resumed(&mut self, event_loop: &ActiveEventLoop) {
            if self.state.is_some() {
                return;
            }
            let window = Arc::new(
                event_loop
                    .create_window(
                        Window::default_attributes().with_title("wgpu-video VA-API + wgpu"),
                    )
                    .expect("window creation should succeed"),
            );
            match PlayerState::new(window.clone()) {
                Ok(state) => {
                    window.request_redraw();
                    self.state = Some(state);
                }
                Err(err) => {
                    eprintln!("failed to initialize player state: {err}");
                    event_loop.exit();
                }
            }
        }

        fn window_event(
            &mut self,
            event_loop: &ActiveEventLoop,
            _window_id: WindowId,
            event: WindowEvent,
        ) {
            let Some(state) = self.state.as_mut() else {
                return;
            };
            match event {
                WindowEvent::CloseRequested => event_loop.exit(),
                WindowEvent::Resized(size) => state.resize(size.width, size.height),
                WindowEvent::RedrawRequested => match state.render() {
                    Ok(RenderOutcome::Continue) => state._window.request_redraw(),
                    Ok(RenderOutcome::Finished) => event_loop.exit(),
                    Err(err) => {
                        eprintln!("render failed: {err}");
                        event_loop.exit();
                    }
                },
                _ => {}
            }
        }
    }

    pub fn run() -> Result<(), String> {
        let event_loop =
            EventLoop::new().map_err(|err| format!("event loop creation failed: {err}"))?;
        let mut app = App::default();
        event_loop
            .run_app(&mut app)
            .map_err(|err| format!("event loop failed: {err}"))
    }
}

#[cfg(target_os = "linux")]
fn main() -> Result<(), String> {
    app::run()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("This example is only available on Linux.");
}
