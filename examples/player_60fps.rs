use std::{
    fs::File,
    sync::Arc,
    time::{Duration, Instant},
};

use wgpu::{
    BindGroupLayout, Color, CommandEncoderDescriptor, CompositeAlphaMode, Device, Features,
    FilterMode, FragmentState, FrontFace, Limits, LoadOp, MultisampleState, Operations,
    PipelineCompilationOptions, PipelineLayoutDescriptor, PolygonMode, PresentMode, PrimitiveState,
    PrimitiveTopology, Queue, RenderPassColorAttachment, RenderPassDescriptor, RenderPipeline,
    RenderPipelineDescriptor, Sampler, SamplerBindingType, SamplerDescriptor, ShaderModuleDescriptor,
    ShaderSource, ShaderStages, StoreOp, Surface, SurfaceConfiguration, SurfaceError, TextureFormat,
    TextureSampleType, TextureUsages, TextureViewDimension, VertexState,
};
use wgpu_video::{PlayerConfig, VideoPlayer};
use winit::{
    application::ApplicationHandler,
    event::WindowEvent,
    event_loop::ActiveEventLoop,
    window::{Window, WindowId},
};

const VIDEO_PATH: &str = "examples/asset/output.h264";
const TICK_60FPS: Duration = Duration::from_nanos(16_666_667);

const VIDEO_SHADER: &str = r#"
@group(0) @binding(0)
var video_tex: texture_2d<f32>;

@group(0) @binding(1)
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

@fragment
fn fs_main(in: VSOut) -> @location(0) vec4<f32> {
    return textureSample(video_tex, video_sampler, in.uv);
}
"#;

struct VideoRenderer {
    pipeline: RenderPipeline,
    bind_group_layout: BindGroupLayout,
    sampler: Sampler,
}

impl VideoRenderer {
    fn new(device: &Device, surface_format: TextureFormat) -> Self {
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("video-bind-group-layout"),
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
                    ty: wgpu::BindingType::Sampler(SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let sampler = device.create_sampler(&SamplerDescriptor {
            label: Some("video-sampler"),
            mag_filter: FilterMode::Linear,
            min_filter: FilterMode::Linear,
            ..Default::default()
        });

        let shader = device.create_shader_module(ShaderModuleDescriptor {
            label: Some("video-shader"),
            source: ShaderSource::Wgsl(VIDEO_SHADER.into()),
        });

        let pipeline_layout = device.create_pipeline_layout(&PipelineLayoutDescriptor {
            label: Some("video-pipeline-layout"),
            bind_group_layouts: &[&bind_group_layout],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&RenderPipelineDescriptor {
            label: Some("video-render-pipeline"),
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
                targets: &[Some(wgpu::ColorTargetState {
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
}

struct PlayerState {
    _window: Arc<Window>,
    surface: Surface<'static>,
    surface_config: SurfaceConfiguration,
    device: Arc<Device>,
    queue: Arc<Queue>,
    renderer: VideoRenderer,
    player: VideoPlayer,
    last_tick: Instant,
    accumulator: Duration,
}

impl PlayerState {
    fn new(window: Arc<Window>) -> Result<Self, String> {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
        let surface = instance
            .create_surface(window.clone())
            .map_err(|err| format!("surface creation failed: {err}"))?;

        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: Some(&surface),
        }))
        .map_err(|err| format!("adapter request failed: {err}"))?;

        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("wgpu-video-example-device"),
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
            .find(TextureFormat::is_srgb)
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
        let renderer = VideoRenderer::new(&device, surface_format);

        let source = Box::new(
            File::open(VIDEO_PATH).map_err(|err| format!("failed to open {VIDEO_PATH}: {err}"))?,
        );

        let player = VideoPlayer::new(
            device.clone(),
            queue.clone(),
            source,
            PlayerConfig {
                target_format: surface_format,
                autoplay: true,
                loop_playback: true,
            },
        );

        Ok(Self {
            _window: window,
            surface,
            surface_config,
            device,
            queue,
            renderer,
            player,
            last_tick: Instant::now(),
            accumulator: Duration::ZERO,
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

    fn tick_60fps(&mut self) -> Result<(), String> {
        let now = Instant::now();
        self.accumulator += now.saturating_duration_since(self.last_tick);
        self.last_tick = now;

        while self.accumulator >= TICK_60FPS {
            self.player
                .tick(TICK_60FPS)
                .map_err(|err| format!("player tick failed: {err:?}"))?;
            self.accumulator -= TICK_60FPS;
        }

        Ok(())
    }

    fn render(&mut self) -> Result<(), String> {
        let surface_frame = match self.surface.get_current_texture() {
            Ok(frame) => frame,
            Err(SurfaceError::Lost | SurfaceError::Outdated) => {
                self.surface.configure(&self.device, &self.surface_config);
                return Ok(());
            }
            Err(SurfaceError::Timeout) => {
                return Ok(());
            }
            Err(SurfaceError::OutOfMemory) => {
                return Err("surface out of memory".to_owned());
            }
            Err(SurfaceError::Other) => {
                return Ok(());
            }
        };

        let view = surface_frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = self
            .device
            .create_command_encoder(&CommandEncoderDescriptor {
                label: Some("video-render-encoder"),
            });

        if let Some(video_view) = self.player.texture_view() {
            let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("video-bind-group"),
                layout: &self.renderer.bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(video_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&self.renderer.sampler),
                    },
                ],
            });

            let mut pass = encoder.begin_render_pass(&RenderPassDescriptor {
                label: Some("video-render-pass"),
                color_attachments: &[Some(RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: Operations {
                        load: LoadOp::Clear(Color::BLACK),
                        store: StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                occlusion_query_set: None,
                timestamp_writes: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.renderer.pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.draw(0..3, 0..1);
        } else {
            let _pass = encoder.begin_render_pass(&RenderPassDescriptor {
                label: Some("clear-pass"),
                color_attachments: &[Some(RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: Operations {
                        load: LoadOp::Clear(Color::BLACK),
                        store: StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                occlusion_query_set: None,
                timestamp_writes: None,
                multiview_mask: None,
            });
        }

        self.queue.submit(std::iter::once(encoder.finish()));
        surface_frame.present();

        Ok(())
    }
}

#[derive(Default)]
struct App {
    window: Option<Arc<Window>>,
    player: Option<PlayerState>,
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_none() {
            let window = Arc::new(
                event_loop
                    .create_window(
                        Window::default_attributes()
                            .with_title("wgpu-video player (60fps)")
                            .with_resizable(true),
                    )
                    .expect("failed to create window"),
            );

            match PlayerState::new(window.clone()) {
                Ok(player) => {
                    self.player = Some(player);
                    self.window = Some(window);
                }
                Err(err) => {
                    eprintln!("{err}");
                    event_loop.exit();
                }
            }
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(player) = self.player.as_mut() else {
            return;
        };

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                player.resize(size.width, size.height);
            }
            WindowEvent::RedrawRequested => {
                if let Err(err) = player.tick_60fps().and_then(|_| player.render()) {
                    eprintln!("render loop failed: {err}");
                    event_loop.exit();
                }
            }
            _ => {}
        }
    }
}

fn main() {
    let event_loop = winit::event_loop::EventLoop::new().expect("failed to create event loop");
    let mut app = App::default();
    event_loop.run_app(&mut app).expect("event loop crashed");
}
