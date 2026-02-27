use std::{
    collections::VecDeque,
    fs::File,
    io::Read,
    sync::Arc,
    time::{Duration, Instant},
};

use vk_video::{EncodedInputChunk, VulkanDevice, VulkanInstance, parameters::DecoderParameters};
use wgpu::{
    BindGroup, BindGroupLayout, Color, CommandEncoderDescriptor, CompositeAlphaMode, Device,
    Features, FilterMode, FragmentState, FrontFace, Limits, LoadOp, MultisampleState, Operations,
    PipelineCompilationOptions, PipelineLayoutDescriptor, PolygonMode, PresentMode, PrimitiveState,
    PrimitiveTopology, Queue, RenderPassColorAttachment, RenderPassDescriptor, RenderPipeline,
    RenderPipelineDescriptor, Sampler, SamplerBindingType, SamplerDescriptor,
    ShaderModuleDescriptor, ShaderSource, ShaderStages, StoreOp, Surface, SurfaceConfiguration,
    SurfaceError, Texture, TextureAspect, TextureFormat, TextureSampleType, TextureUsages,
    TextureViewDescriptor, TextureViewDimension, VertexState,
};
use winit::{
    application::ApplicationHandler,
    event::WindowEvent,
    event_loop::ActiveEventLoop,
    window::{Window, WindowId},
};

const VIDEO_PATH: &str = "examples/asset/output.h264";
const TICK_25FPS: Duration = Duration::from_millis(40);

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

@fragment
fn fs_main(in: VSOut) -> @location(0) vec4<f32> {
    let y = textureSample(y_tex, video_sampler, in.uv).r;
    let uv = textureSample(uv_tex, video_sampler, in.uv).rg - vec2<f32>(0.5, 0.5);

    // NV12 (YCbCr) to RGB conversion.
    let r = y + 1.402 * uv.y;
    let g = y - 0.344136 * uv.x - 0.714136 * uv.y;
    let b = y + 1.772 * uv.x;

    return vec4<f32>(r, g, b, 1.0);
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

    fn bind_group_for_texture(&self, device: &Device, texture: &Texture) -> BindGroup {
        let y_view = texture.create_view(&TextureViewDescriptor {
            label: Some("video-y-plane"),
            format: Some(TextureFormat::R8Unorm),
            dimension: Some(TextureViewDimension::D2),
            usage: Some(TextureUsages::TEXTURE_BINDING),
            aspect: TextureAspect::Plane0,
            base_mip_level: 0,
            mip_level_count: Some(1),
            base_array_layer: 0,
            array_layer_count: Some(1),
        });

        let uv_view = texture.create_view(&TextureViewDescriptor {
            label: Some("video-uv-plane"),
            format: Some(TextureFormat::Rg8Unorm),
            dimension: Some(TextureViewDimension::D2),
            usage: Some(TextureUsages::TEXTURE_BINDING),
            aspect: TextureAspect::Plane1,
            base_mip_level: 0,
            mip_level_count: Some(1),
            base_array_layer: 0,
            array_layer_count: Some(1),
        });

        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("video-bind-group"),
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

struct PlayerState {
    _instance: Arc<VulkanInstance>,
    _window: Arc<Window>,
    surface: Surface<'static>,
    surface_config: SurfaceConfiguration,
    device: Arc<VulkanDevice>,
    queue: Queue,
    renderer: VideoRenderer,
    decoder: vk_video::WgpuTexturesDecoder,
    input: File,
    read_buffer: Vec<u8>,
    decoded_queue: VecDeque<Texture>,
    current_texture: Option<Texture>,
    stream_done: bool,
    last_present: Instant,
}

impl PlayerState {
    fn new(window: Arc<Window>) -> Result<Self, String> {
        let instance = VulkanInstance::new().map_err(|err| format!("vulkan init failed: {err}"))?;
        let surface = instance
            .wgpu_instance()
            .create_surface(window.clone())
            .map_err(|err| format!("surface creation failed: {err}"))?;

        let compatible_decode_adapter = instance
            .iter_adapters(Some(&surface))
            .map_err(|err| format!("failed to enumerate adapters: {err}"))?
            .find(|adapter| adapter.supports_decoding());

        let adapter = if let Some(adapter) = compatible_decode_adapter {
            adapter
        } else {
            let decode_only_names = instance
                .iter_adapters(None)
                .map_err(|err| format!("failed to enumerate all adapters: {err}"))?
                .filter(|adapter| adapter.supports_decoding())
                .map(|adapter| adapter.info().name.clone())
                .collect::<Vec<_>>();

            if decode_only_names.is_empty() {
                return Err("no Vulkan video decode-capable adapter found".to_owned());
            }

            return Err(format!(
                "no adapter supports both presentation and Vulkan decode (zero-copy impossible on this setup). Decode-capable adapters: {decode_only_names:?}"
            ));
        };

        let device = adapter
            .create_device(
                Features::empty(),
                wgpu::ExperimentalFeatures::disabled(),
                Limits::default(),
            )
            .map_err(|err| format!("failed to create device: {err}"))?;

        let caps = surface.get_capabilities(&device.wgpu_adapter());
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
        surface.configure(&device.wgpu_device(), &surface_config);

        let renderer = VideoRenderer::new(&device.wgpu_device(), surface_format);
        let decoder = device
            .create_wgpu_textures_decoder(DecoderParameters::default())
            .map_err(|err| format!("failed to create decoder: {err}"))?;

        let input =
            File::open(VIDEO_PATH).map_err(|err| format!("failed to open {VIDEO_PATH}: {err}"))?;

        Ok(Self {
            _instance: instance,
            _window: window,
            surface,
            surface_config,
            queue: device.wgpu_queue(),
            renderer,
            decoder,
            device,
            input,
            read_buffer: vec![0u8; 4096],
            decoded_queue: VecDeque::new(),
            current_texture: None,
            stream_done: false,
            last_present: Instant::now(),
        })
    }

    fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.surface_config.width = width;
        self.surface_config.height = height;
        self.surface
            .configure(&self.device.wgpu_device(), &self.surface_config);
    }

    fn decode_until_frame_available(&mut self) -> Result<(), String> {
        if !self.decoded_queue.is_empty() || self.stream_done {
            return Ok(());
        }

        while self.decoded_queue.is_empty() && !self.stream_done {
            let n = self
                .input
                .read(&mut self.read_buffer)
                .map_err(|err| format!("failed to read bitstream: {err}"))?;

            if n == 0 {
                self.stream_done = true;
                let drained = self
                    .decoder
                    .flush()
                    .map_err(|err| format!("decoder flush failed: {err}"))?;
                for frame in drained {
                    self.decoded_queue.push_back(frame.data);
                }
                break;
            }

            let decoded = self
                .decoder
                .decode(EncodedInputChunk {
                    data: &self.read_buffer[..n],
                    pts: None,
                })
                .map_err(|err| format!("decode failed: {err}"))?;

            for frame in decoded {
                self.decoded_queue.push_back(frame.data);
            }
        }

        Ok(())
    }

    fn tick_frame(&mut self) -> Result<(), String> {
        if self.last_present.elapsed() < TICK_25FPS {
            return Ok(());
        }

        self.last_present = Instant::now();
        self.decode_until_frame_available()?;

        if let Some(next) = self.decoded_queue.pop_front() {
            self.current_texture = Some(next);
        }

        Ok(())
    }

    fn render(&mut self) -> Result<(), String> {
        let surface_frame = match self.surface.get_current_texture() {
            Ok(frame) => frame,
            Err(SurfaceError::Lost | SurfaceError::Outdated) => {
                self.surface
                    .configure(&self.device.wgpu_device(), &self.surface_config);
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
            .create_view(&TextureViewDescriptor::default());
        let mut encoder =
            self.device
                .wgpu_device()
                .create_command_encoder(&CommandEncoderDescriptor {
                    label: Some("video-render-encoder"),
                });

        if let Some(texture) = self.current_texture.as_ref() {
            let bind_group = self
                .renderer
                .bind_group_for_texture(&self.device.wgpu_device(), texture);

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
                            .with_title("vk-video player (25fps, zero-copy)"),
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
                if let Err(err) = player.tick_frame().and_then(|_| player.render()) {
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
