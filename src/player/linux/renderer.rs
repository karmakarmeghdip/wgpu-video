use wgpu::{
    BindGroupLayout, ColorTargetState, CommandEncoderDescriptor, Device, FilterMode, FragmentState,
    FrontFace, LoadOp, MultisampleState, Operations, PipelineCompilationOptions,
    PipelineLayoutDescriptor, PrimitiveState, PrimitiveTopology, RenderPassColorAttachment,
    RenderPassDescriptor, RenderPipeline, RenderPipelineDescriptor, Sampler, SamplerBindingType,
    SamplerDescriptor, ShaderModuleDescriptor, ShaderSource, ShaderStages, StoreOp, Texture,
    TextureDescriptor, TextureDimension, TextureFormat, TextureSampleType, TextureUsages,
    TextureView, TextureViewDimension, VertexState,
};

use crate::ImportedPlaneFrame;

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

pub(super) struct OutputFrame {
    pub width: u32,
    pub height: u32,
    pub _texture: Texture,
    pub view: TextureView,
}

pub(super) struct VideoRenderer {
    pipeline: RenderPipeline,
    bind_group_layout: BindGroupLayout,
    sampler: Sampler,
    target_format: TextureFormat,
}

impl VideoRenderer {
    pub(super) fn new(device: &Device, target_format: TextureFormat) -> Self {
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("wgpu-video-bind-group-layout"),
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
        let shader = device.create_shader_module(ShaderModuleDescriptor {
            label: Some("wgpu-video-shader"),
            source: ShaderSource::Wgsl(VIDEO_SHADER.into()),
        });
        let sampler = device.create_sampler(&SamplerDescriptor {
            label: Some("wgpu-video-sampler"),
            mag_filter: FilterMode::Linear,
            min_filter: FilterMode::Linear,
            ..Default::default()
        });
        let pipeline_layout = device.create_pipeline_layout(&PipelineLayoutDescriptor {
            label: Some("wgpu-video-pipeline-layout"),
            bind_group_layouts: &[&bind_group_layout],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&RenderPipelineDescriptor {
            label: Some("wgpu-video-render-pipeline"),
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
                    format: target_format,
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
                polygon_mode: wgpu::PolygonMode::Fill,
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
            target_format,
        }
    }

    fn ensure_output_frame(
        &self,
        device: &Device,
        output: &mut Option<OutputFrame>,
        width: u32,
        height: u32,
    ) {
        let needs_recreate = output
            .as_ref()
            .map(|current| current.width != width || current.height != height)
            .unwrap_or(true);
        if !needs_recreate {
            return;
        }

        let texture = device.create_texture(&TextureDescriptor {
            label: Some("wgpu-video-output-texture"),
            size: wgpu::Extent3d {
                width: width.max(1),
                height: height.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: TextureDimension::D2,
            format: self.target_format,
            usage: TextureUsages::TEXTURE_BINDING | TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        *output = Some(OutputFrame {
            width,
            height,
            _texture: texture,
            view,
        });
    }

    pub(super) fn render_frame(
        &self,
        device: &Device,
        queue: &wgpu::Queue,
        output: &mut Option<OutputFrame>,
        imported: &ImportedPlaneFrame,
    ) {
        self.ensure_output_frame(device, output, imported.width, imported.height);
        let Some(target) = output.as_ref() else {
            return;
        };

        let y_view = imported
            .y_texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let uv_view = imported
            .uv_texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("wgpu-video-bind-group"),
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
        });

        let mut encoder = device.create_command_encoder(&CommandEncoderDescriptor {
            label: Some("wgpu-video-render-encoder"),
        });
        {
            let mut pass = encoder.begin_render_pass(&RenderPassDescriptor {
                label: Some("wgpu-video-render-pass"),
                color_attachments: &[Some(RenderPassColorAttachment {
                    view: &target.view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: Operations {
                        load: LoadOp::Clear(wgpu::Color::BLACK),
                        store: StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        queue.submit([encoder.finish()]);
    }
}
