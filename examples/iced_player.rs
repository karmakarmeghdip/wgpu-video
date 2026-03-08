#[cfg(target_os = "linux")]
mod app {
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use iced::mouse;
    use iced::theme;
    use iced::time;
    use iced::widget::button as button_style;
    use iced::widget::container as container_style;
    use iced::widget::shader::{self as shader_program, Action as ShaderAction, Viewport};
    use iced::widget::{
        button, checkbox, column, container, progress_bar, row, shader, text, text_input,
    };
    use iced::{
        Background, Border, Color, Element, Event, Fill, FillPortion, Rectangle, Subscription,
        Theme,
    };
    use wgpu_video::{BackendKind, PlaybackDiagnostics, PlayerConfig, VideoPlayer};

    const VIDEO_TEXTURE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
    const RGBA_VIDEO_SHADER: &str = r#"
@group(0) @binding(0)
var frame_tex: texture_2d<f32>;

@group(0) @binding(1)
var frame_sampler: sampler;

struct VSOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> VSOut {
    var out: VSOut;
    let x = f32((index << 1u) & 2u);
    let y = f32(index & 2u);
    out.position = vec4<f32>(x * 2.0 - 1.0, 1.0 - y * 2.0, 0.0, 1.0);
    out.uv = vec2<f32>(x, y);
    return out;
}

@fragment
fn fs_main(in: VSOut) -> @location(0) vec4<f32> {
    return textureSample(frame_tex, frame_sampler, in.uv);
}
"#;

    #[derive(Debug, Clone)]
    enum Message {
        PathEdited(String),
        OpenPressed,
        TogglePlayback,
        RestartPressed,
        ToggleLoop(bool),
        UiRefresh,
    }

    pub fn run() -> iced::Result {
        iced::application(PlayerDemo::new, PlayerDemo::update, PlayerDemo::view)
            .subscription(PlayerDemo::subscription)
            .theme(PlayerDemo::theme)
            .window_size(iced::Size::new(1280.0, 820.0))
            .run()
    }

    struct PlayerDemo {
        path_input: String,
        loop_playback: bool,
        shared: Arc<Mutex<SharedPlayback>>,
        surface: VideoSurface,
    }

    impl PlayerDemo {
        fn new() -> Self {
            let default_path = "examples/asset/test.mp4".to_owned();
            let shared = Arc::new(Mutex::new(SharedPlayback::default()));

            {
                let mut playback = shared.lock().expect("playback state lock should succeed");
                playback.request_open(default_path.clone(), true, false);
            }

            Self {
                path_input: default_path,
                loop_playback: false,
                surface: VideoSurface {
                    shared: shared.clone(),
                },
                shared,
            }
        }

        fn update(&mut self, message: Message) {
            match message {
                Message::PathEdited(path) => {
                    self.path_input = path;
                }
                Message::OpenPressed => {
                    let path = self.path_input.trim();
                    let mut playback = self
                        .shared
                        .lock()
                        .expect("playback state lock should succeed");

                    if path.is_empty() {
                        playback.status.error =
                            Some("Enter a video file path before opening.".to_owned());
                    } else {
                        playback.request_open(path.to_owned(), true, self.loop_playback);
                    }
                }
                Message::TogglePlayback => {
                    let mut playback = self
                        .shared
                        .lock()
                        .expect("playback state lock should succeed");

                    if playback.status.loaded_path.is_none() {
                        let path = self.path_input.trim();
                        if path.is_empty() {
                            playback.status.error = Some(
                                "Enter a video file path before starting playback.".to_owned(),
                            );
                        } else {
                            playback.request_open(path.to_owned(), true, self.loop_playback);
                        }
                    } else {
                        let should_play = !playback.status.is_playing;
                        playback.set_playing(should_play);
                    }
                }
                Message::RestartPressed => {
                    let mut playback = self
                        .shared
                        .lock()
                        .expect("playback state lock should succeed");

                    if playback.status.loaded_path.is_some() {
                        playback.request_restart();
                    } else {
                        let path = self.path_input.trim();
                        if path.is_empty() {
                            playback.status.error =
                                Some("Enter a video file path before reloading.".to_owned());
                        } else {
                            playback.request_open(path.to_owned(), true, self.loop_playback);
                        }
                    }
                }
                Message::ToggleLoop(enabled) => {
                    self.loop_playback = enabled;
                }
                Message::UiRefresh => {}
            }
        }

        fn subscription(&self) -> Subscription<Message> {
            time::every(Duration::from_millis(50)).map(|_| Message::UiRefresh)
        }

        fn theme(&self) -> Theme {
            Theme::custom(
                "Copper Coast",
                theme::Palette {
                    background: Color::from_rgb8(0xF4, 0xEE, 0xE6),
                    text: Color::from_rgb8(0x1D, 0x24, 0x2C),
                    primary: Color::from_rgb8(0x0E, 0x74, 0x74),
                    success: Color::from_rgb8(0x2E, 0x8B, 0x57),
                    warning: Color::from_rgb8(0xD4, 0x8A, 0x30),
                    danger: Color::from_rgb8(0xB4, 0x3F, 0x3F),
                },
            )
        }

        fn view(&self) -> Element<'_, Message> {
            let snapshot = self.snapshot();
            let progress = if snapshot.duration.is_zero() {
                0.0
            } else {
                (snapshot.position.as_secs_f32() / snapshot.duration.as_secs_f32()).clamp(0.0, 1.0)
            };

            let play_label = if snapshot.is_playing { "Pause" } else { "Play" };
            let backend_label = snapshot
                .backend
                .clone()
                .unwrap_or_else(|| "waiting for renderer".to_owned());
            let dimensions_label = snapshot
                .dimensions
                .map(|(width, height)| format!("{width}x{height}"))
                .unwrap_or_else(|| "--".to_owned());
            let path_label = snapshot
                .loaded_path
                .clone()
                .unwrap_or_else(|| "No file loaded".to_owned());
            let status_line = if let Some(error) = &snapshot.error {
                error.clone()
            } else if snapshot.reached_end {
                "Playback finished. Press Play to restart or enable Loop on the next open."
                    .to_owned()
            } else if snapshot.loaded_path.is_some() {
                format!(
                    "{} / {}",
                    format_duration(snapshot.position),
                    format_duration(snapshot.duration)
                )
            } else {
                "Paste a local video file path and press Open.".to_owned()
            };
            let diagnostics = snapshot.diagnostics;
            let diagnostics_line = format!(
                "presented={} dropped={} late={} buffered={} preroll={} last_late_ms={:.2} max_late_ms={:.2}",
                diagnostics.presented_frames,
                diagnostics.dropped_frames,
                diagnostics.late_frames,
                diagnostics.buffered_frames,
                diagnostics.waiting_for_preroll,
                diagnostics.last_frame_lateness.as_secs_f64() * 1000.0,
                diagnostics.max_frame_lateness.as_secs_f64() * 1000.0,
            );

            let header = container(
                column![
                    text("wgpu-video / iced player").size(14).color(Color::from_rgb8(0x56, 0x68, 0x74)),
                    text("A native demo player for `VideoPlayer`").size(34),
                    text("Open a local file, keep playback on the GPU, and present frames inside an iced interface.")
                        .size(18)
                        .color(Color::from_rgb8(0x4E, 0x5C, 0x66)),
                ]
                .spacing(8),
            )
            .padding(24)
            .style(card_style(Color::from_rgb8(0xFF, 0xFA, 0xF5), Color::from_rgb8(0xDA, 0xD1, 0xC7)));

            let input_row = container(
                row![
                    text_input("/path/to/video.(mp4|mkv|webm)", &self.path_input)
                        .on_input(Message::PathEdited)
                        .on_submit(Message::OpenPressed)
                        .padding(14)
                        .size(18)
                        .width(Fill),
                    button("Open")
                        .on_press(Message::OpenPressed)
                        .padding([14, 20])
                        .style(button_style::primary),
                ]
                .spacing(14)
                .align_y(iced::Alignment::Center),
            )
            .padding(20)
            .style(card_style(
                Color::from_rgb8(0xFC, 0xF7, 0xF1),
                Color::from_rgb8(0xDA, 0xD1, 0xC7),
            ));

            let video_surface = container(shader(self.surface.clone()).width(Fill).height(Fill))
                .width(Fill)
                .height(Fill)
                .padding(12)
                .style(video_stage_style);

            let controls = container(
                column![
                    row![
                        button(play_label)
                            .on_press(Message::TogglePlayback)
                            .padding([12, 22])
                            .style(button_style::primary),
                        button("Restart")
                            .on_press(Message::RestartPressed)
                            .padding([12, 22]),
                        checkbox(self.loop_playback)
                            .label("Loop on open")
                            .on_toggle(Message::ToggleLoop),
                        text(format!("Backend: {backend_label}")),
                        text(format!("Frame: {dimensions_label}")),
                    ]
                    .spacing(16)
                    .align_y(iced::Alignment::Center),
                    progress_bar(0.0..=1.0, progress),
                    text(status_line)
                        .size(16)
                        .color(if snapshot.error.is_some() {
                            Color::from_rgb8(0xB4, 0x3F, 0x3F)
                        } else {
                            Color::from_rgb8(0x44, 0x53, 0x5D)
                        }),
                    text(path_label)
                        .size(14)
                        .color(Color::from_rgb8(0x6D, 0x77, 0x80)),
                    text(diagnostics_line)
                        .size(13)
                        .color(Color::from_rgb8(0x6A, 0x63, 0x58)),
                ]
                .spacing(14),
            )
            .padding(20)
            .style(card_style(
                Color::from_rgb8(0xFF, 0xFB, 0xF8),
                Color::from_rgb8(0xDA, 0xD1, 0xC7),
            ));

            container(
                column![
                    header,
                    input_row,
                    video_surface.height(FillPortion(6)),
                    controls,
                ]
                .spacing(18),
            )
            .width(Fill)
            .height(Fill)
            .padding(22)
            .style(app_shell_style)
            .into()
        }

        fn snapshot(&self) -> PlaybackSnapshot {
            self.shared
                .lock()
                .expect("playback state lock should succeed")
                .snapshot()
        }
    }

    #[derive(Debug, Clone)]
    struct PlaybackSnapshot {
        loaded_path: Option<String>,
        backend: Option<String>,
        error: Option<String>,
        duration: Duration,
        position: Duration,
        dimensions: Option<(u32, u32)>,
        diagnostics: PlaybackDiagnostics,
        is_playing: bool,
        reached_end: bool,
    }

    #[derive(Debug, Clone, Default)]
    struct PlaybackStatus {
        loaded_path: Option<String>,
        backend: Option<String>,
        error: Option<String>,
        duration: Duration,
        position: Duration,
        dimensions: Option<(u32, u32)>,
        next_wakeup: Option<Duration>,
        diagnostics: PlaybackDiagnostics,
        is_playing: bool,
        reached_end: bool,
    }

    impl PlaybackStatus {
        fn snapshot(&self) -> PlaybackSnapshot {
            PlaybackSnapshot {
                loaded_path: self.loaded_path.clone(),
                backend: self.backend.clone(),
                error: self.error.clone(),
                duration: self.duration,
                position: self.position,
                dimensions: self.dimensions,
                diagnostics: self.diagnostics,
                is_playing: self.is_playing,
                reached_end: self.reached_end,
            }
        }
    }

    #[derive(Debug, Clone)]
    struct SourceRequest {
        path: String,
        loop_playback: bool,
    }

    #[derive(Debug, Default)]
    struct SharedPlayback {
        open_request: u64,
        play_request: u64,
        restart_request: u64,
        desired_playing: bool,
        source: Option<SourceRequest>,
        status: PlaybackStatus,
    }

    impl SharedPlayback {
        fn request_open(&mut self, path: String, autoplay: bool, loop_playback: bool) {
            self.open_request = self.open_request.saturating_add(1);
            self.desired_playing = autoplay;
            self.source = Some(SourceRequest {
                path,
                loop_playback,
            });
            self.status.error = None;
            self.status.reached_end = false;
            self.status.next_wakeup = Some(Duration::ZERO);
        }

        fn set_playing(&mut self, is_playing: bool) {
            self.play_request = self.play_request.saturating_add(1);
            self.desired_playing = is_playing;
            self.status.error = None;
            self.status.reached_end = false;
            self.status.next_wakeup = Some(Duration::ZERO);
        }

        fn request_restart(&mut self) {
            self.restart_request = self.restart_request.saturating_add(1);
            self.desired_playing = true;
            self.status.error = None;
            self.status.reached_end = false;
            self.status.next_wakeup = Some(Duration::ZERO);
        }

        fn snapshot(&self) -> PlaybackSnapshot {
            self.status.snapshot()
        }
    }

    #[derive(Debug, Clone)]
    struct VideoSurface {
        shared: Arc<Mutex<SharedPlayback>>,
    }

    impl<Message> shader_program::Program<Message> for VideoSurface {
        type State = ();
        type Primitive = VideoPrimitive;

        fn update(
            &self,
            _state: &mut Self::State,
            event: &Event,
            _bounds: Rectangle,
            _cursor: mouse::Cursor,
        ) -> Option<ShaderAction<Message>> {
            if matches!(
                event,
                Event::Window(iced::window::Event::RedrawRequested(_))
            ) {
                let delay = self
                    .shared
                    .lock()
                    .expect("playback state lock should succeed")
                    .status
                    .next_wakeup;

                return Some(match delay {
                    Some(delay) if delay <= Duration::from_millis(1) => {
                        ShaderAction::request_redraw()
                    }
                    Some(delay) => ShaderAction::request_redraw_at(Instant::now() + delay),
                    None => ShaderAction::request_redraw(),
                });
            }

            None
        }

        fn draw(
            &self,
            _state: &Self::State,
            _cursor: mouse::Cursor,
            bounds: Rectangle,
        ) -> Self::Primitive {
            VideoPrimitive {
                bounds,
                shared: self.shared.clone(),
            }
        }
    }

    #[derive(Debug)]
    struct VideoPrimitive {
        bounds: Rectangle,
        shared: Arc<Mutex<SharedPlayback>>,
    }

    impl shader_program::Primitive for VideoPrimitive {
        type Pipeline = VideoPipeline;

        fn prepare(
            &self,
            pipeline: &mut Self::Pipeline,
            device: &iced::wgpu::Device,
            _queue: &iced::wgpu::Queue,
            _bounds: &Rectangle,
            _viewport: &Viewport,
        ) {
            pipeline.prepare_frame(device, &self.shared, self.bounds);
        }

        fn render(
            &self,
            pipeline: &Self::Pipeline,
            encoder: &mut iced::wgpu::CommandEncoder,
            target: &iced::wgpu::TextureView,
            clip_bounds: &Rectangle<u32>,
        ) {
            pipeline.render_to(encoder, target, clip_bounds);
        }
    }

    struct VideoPipeline {
        device: Arc<wgpu::Device>,
        queue: Arc<wgpu::Queue>,
        render_pipeline: iced::wgpu::RenderPipeline,
        bind_group_layout: iced::wgpu::BindGroupLayout,
        sampler: iced::wgpu::Sampler,
        bind_group: Option<iced::wgpu::BindGroup>,
        player: Option<VideoPlayer>,
        current_source: Option<SourceRequest>,
        video_size: Option<(u32, u32)>,
        last_open_request: u64,
        last_play_request: u64,
        last_restart_request: u64,
    }

    impl VideoPipeline {
        fn open_player(
            &self,
            source: &SourceRequest,
            autoplay: bool,
        ) -> Result<VideoPlayer, String> {
            VideoPlayer::open_path(
                self.device.clone(),
                self.queue.clone(),
                source.path.clone(),
                PlayerConfig {
                    target_format: VIDEO_TEXTURE_FORMAT,
                    autoplay,
                    loop_playback: source.loop_playback,
                    ..PlayerConfig::default()
                },
            )
            .map_err(|error| error.to_string())
        }

        fn prepare_frame(
            &mut self,
            device: &iced::wgpu::Device,
            shared: &Arc<Mutex<SharedPlayback>>,
            bounds: Rectangle,
        ) {
            let (open_request, play_request, restart_request, desired_playing, source) = {
                let playback = shared.lock().expect("playback state lock should succeed");
                (
                    playback.open_request,
                    playback.play_request,
                    playback.restart_request,
                    playback.desired_playing,
                    playback.source.clone(),
                )
            };

            if self.last_open_request != open_request {
                self.last_open_request = open_request;
                self.last_play_request = play_request;
                self.last_restart_request = restart_request;
                self.bind_group = None;
                self.video_size = None;

                match source
                    .as_ref()
                    .ok_or_else(|| "No source was provided.".to_owned())
                    .and_then(|source| self.open_player(source, desired_playing))
                {
                    Ok(player) => {
                        self.current_source = source;
                        self.player = Some(player);
                        self.update_status(shared, None);
                    }
                    Err(error) => {
                        self.current_source = source;
                        self.player = None;
                        self.update_status(shared, Some(error));
                    }
                }
            }

            if self.last_restart_request != restart_request {
                self.last_restart_request = restart_request;

                if let Some(source) = self.current_source.clone() {
                    match self.open_player(&source, true) {
                        Ok(player) => {
                            self.player = Some(player);
                            self.update_status(shared, None);
                        }
                        Err(error) => {
                            self.player = None;
                            self.bind_group = None;
                            self.video_size = None;
                            self.update_status(shared, Some(error));
                        }
                    }
                }
            }

            if self.last_play_request != play_request {
                self.last_play_request = play_request;

                if let Some(player) = self.player.as_mut() {
                    let result = if desired_playing {
                        player.play().map_err(|error| error.to_string())
                    } else {
                        player.pause();
                        Ok::<(), String>(())
                    };

                    if let Err(error) = result {
                        self.update_status(shared, Some(error));
                    }
                }
            }

            if let Some(player) = self.player.as_mut() {
                match player.poll() {
                    Ok(result) => {
                        if result.presented_frame {
                            if let Some(view) = player.texture_view().cloned() {
                                self.bind_group = Some(device.create_bind_group(
                                    &iced::wgpu::BindGroupDescriptor {
                                        label: Some("iced-player-video-bind-group"),
                                        layout: &self.bind_group_layout,
                                        entries: &[
                                            iced::wgpu::BindGroupEntry {
                                                binding: 0,
                                                resource: iced::wgpu::BindingResource::TextureView(
                                                    &view,
                                                ),
                                            },
                                            iced::wgpu::BindGroupEntry {
                                                binding: 1,
                                                resource: iced::wgpu::BindingResource::Sampler(
                                                    &self.sampler,
                                                ),
                                            },
                                        ],
                                    },
                                ));
                            }
                        }

                        let dimensions = player.dimensions();
                        self.video_size = if dimensions.0 > 0 && dimensions.1 > 0 {
                            Some(dimensions)
                        } else {
                            self.video_size
                        };
                        self.update_status(shared, None);

                        if result.reached_end {
                            let mut playback =
                                shared.lock().expect("playback state lock should succeed");
                            playback.status.reached_end = true;
                        }
                    }
                    Err(error) => {
                        self.player = None;
                        self.bind_group = None;
                        self.video_size = None;
                        self.update_status(shared, Some(error.to_string()));
                    }
                }
            }

            if bounds.width <= 0.0 || bounds.height <= 0.0 {
                self.bind_group = None;
            }
        }

        fn update_status(&self, shared: &Arc<Mutex<SharedPlayback>>, error: Option<String>) {
            let mut playback = shared.lock().expect("playback state lock should succeed");
            let status = &mut playback.status;

            if let Some(player) = self.player.as_ref() {
                status.loaded_path = self
                    .current_source
                    .as_ref()
                    .map(|source| source.path.clone());
                status.backend = Some(match player.backend_kind() {
                    BackendKind::Auto => "Auto".to_owned(),
                    #[cfg(target_os = "linux")]
                    BackendKind::Libva => "libva".to_owned(),
                    #[cfg(target_os = "windows")]
                    BackendKind::Wmf => "WMF".to_owned(),
                });
                status.duration = player.duration();
                status.position = player.position();
                status.next_wakeup = player
                    .next_frame_deadline()
                    .map(|deadline| deadline.saturating_duration_since(Instant::now()));
                status.diagnostics = player.diagnostics();
                let dimensions = player.dimensions();
                status.dimensions = if dimensions.0 > 0 && dimensions.1 > 0 {
                    Some(dimensions)
                } else {
                    self.video_size
                };
                status.is_playing = player.is_playing();
            } else {
                status.loaded_path = self
                    .current_source
                    .as_ref()
                    .map(|source| source.path.clone());
                status.backend = None;
                status.duration = Duration::ZERO;
                status.position = Duration::ZERO;
                status.dimensions = self.video_size;
                status.next_wakeup = None;
                status.diagnostics = PlaybackDiagnostics::default();
                status.is_playing = false;
            }

            if error.is_some() {
                status.reached_end = false;
            }

            status.error = error;
        }

        fn render_to(
            &self,
            encoder: &mut iced::wgpu::CommandEncoder,
            target: &iced::wgpu::TextureView,
            clip_bounds: &Rectangle<u32>,
        ) {
            let Some(bind_group) = self.bind_group.as_ref() else {
                return;
            };

            let Some(video_size) = self.video_size else {
                return;
            };

            let viewport = fit_viewport(*clip_bounds, video_size);
            if viewport.2 <= 0.0 || viewport.3 <= 0.0 {
                return;
            }

            let mut pass = encoder.begin_render_pass(&iced::wgpu::RenderPassDescriptor {
                label: Some("iced-player-video-pass"),
                color_attachments: &[Some(iced::wgpu::RenderPassColorAttachment {
                    view: target,
                    resolve_target: None,
                    depth_slice: None,
                    ops: iced::wgpu::Operations {
                        load: iced::wgpu::LoadOp::Load,
                        store: iced::wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_scissor_rect(
                clip_bounds.x,
                clip_bounds.y,
                clip_bounds.width.max(1),
                clip_bounds.height.max(1),
            );
            pass.set_viewport(viewport.0, viewport.1, viewport.2, viewport.3, 0.0, 1.0);
            pass.set_pipeline(&self.render_pipeline);
            pass.set_bind_group(0, bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
    }

    impl shader_program::Pipeline for VideoPipeline {
        fn new(
            device: &iced::wgpu::Device,
            queue: &iced::wgpu::Queue,
            format: iced::wgpu::TextureFormat,
        ) -> Self {
            let bind_group_layout =
                device.create_bind_group_layout(&iced::wgpu::BindGroupLayoutDescriptor {
                    label: Some("iced-player-video-layout"),
                    entries: &[
                        iced::wgpu::BindGroupLayoutEntry {
                            binding: 0,
                            visibility: iced::wgpu::ShaderStages::FRAGMENT,
                            ty: iced::wgpu::BindingType::Texture {
                                sample_type: iced::wgpu::TextureSampleType::Float {
                                    filterable: true,
                                },
                                view_dimension: iced::wgpu::TextureViewDimension::D2,
                                multisampled: false,
                            },
                            count: None,
                        },
                        iced::wgpu::BindGroupLayoutEntry {
                            binding: 1,
                            visibility: iced::wgpu::ShaderStages::FRAGMENT,
                            ty: iced::wgpu::BindingType::Sampler(
                                iced::wgpu::SamplerBindingType::Filtering,
                            ),
                            count: None,
                        },
                    ],
                });

            let shader = device.create_shader_module(iced::wgpu::ShaderModuleDescriptor {
                label: Some("iced-player-video-shader"),
                source: iced::wgpu::ShaderSource::Wgsl(RGBA_VIDEO_SHADER.into()),
            });
            let sampler = device.create_sampler(&iced::wgpu::SamplerDescriptor {
                label: Some("iced-player-video-sampler"),
                mag_filter: iced::wgpu::FilterMode::Linear,
                min_filter: iced::wgpu::FilterMode::Linear,
                ..Default::default()
            });
            let layout = device.create_pipeline_layout(&iced::wgpu::PipelineLayoutDescriptor {
                label: Some("iced-player-video-pipeline-layout"),
                bind_group_layouts: &[&bind_group_layout],
                immediate_size: 0,
            });
            let render_pipeline =
                device.create_render_pipeline(&iced::wgpu::RenderPipelineDescriptor {
                    label: Some("iced-player-video-pipeline"),
                    layout: Some(&layout),
                    vertex: iced::wgpu::VertexState {
                        module: &shader,
                        entry_point: Some("vs_main"),
                        buffers: &[],
                        compilation_options: iced::wgpu::PipelineCompilationOptions::default(),
                    },
                    fragment: Some(iced::wgpu::FragmentState {
                        module: &shader,
                        entry_point: Some("fs_main"),
                        targets: &[Some(iced::wgpu::ColorTargetState {
                            format,
                            blend: Some(iced::wgpu::BlendState::REPLACE),
                            write_mask: iced::wgpu::ColorWrites::ALL,
                        })],
                        compilation_options: iced::wgpu::PipelineCompilationOptions::default(),
                    }),
                    primitive: iced::wgpu::PrimitiveState {
                        topology: iced::wgpu::PrimitiveTopology::TriangleList,
                        strip_index_format: None,
                        front_face: iced::wgpu::FrontFace::Ccw,
                        cull_mode: None,
                        unclipped_depth: false,
                        polygon_mode: iced::wgpu::PolygonMode::Fill,
                        conservative: false,
                    },
                    depth_stencil: None,
                    multisample: iced::wgpu::MultisampleState::default(),
                    multiview_mask: None,
                    cache: None,
                });

            Self {
                device: Arc::new(device.clone()),
                queue: Arc::new(queue.clone()),
                render_pipeline,
                bind_group_layout,
                sampler,
                bind_group: None,
                player: None,
                current_source: None,
                video_size: None,
                last_open_request: 0,
                last_play_request: 0,
                last_restart_request: 0,
            }
        }
    }

    fn format_duration(duration: Duration) -> String {
        let total_seconds = duration.as_secs();
        let hours = total_seconds / 3600;
        let minutes = (total_seconds % 3600) / 60;
        let seconds = total_seconds % 60;

        if hours > 0 {
            format!("{hours:02}:{minutes:02}:{seconds:02}")
        } else {
            format!("{minutes:02}:{seconds:02}")
        }
    }

    fn fit_viewport(bounds: Rectangle<u32>, video_size: (u32, u32)) -> (f32, f32, f32, f32) {
        let target_width = bounds.width as f32;
        let target_height = bounds.height as f32;
        let video_width = video_size.0.max(1) as f32;
        let video_height = video_size.1.max(1) as f32;
        let scale = (target_width / video_width).min(target_height / video_height);
        let width = (video_width * scale).max(1.0);
        let height = (video_height * scale).max(1.0);
        let x = bounds.x as f32 + (target_width - width) * 0.5;
        let y = bounds.y as f32 + (target_height - height) * 0.5;

        (x, y, width, height)
    }

    fn app_shell_style(_theme: &Theme) -> container_style::Style {
        container_style::Style::default()
            .background(Background::Color(Color::from_rgb8(0xEE, 0xE5, 0xDA)))
    }

    fn video_stage_style(_theme: &Theme) -> container_style::Style {
        container_style::Style::default()
            .background(Background::Color(Color::from_rgb8(0x10, 0x14, 0x18)))
            .border(Border {
                radius: 26.0.into(),
                width: 1.0,
                color: Color::from_rgb8(0x29, 0x32, 0x3B),
            })
    }

    fn card_style(
        background: Color,
        border_color: Color,
    ) -> impl Fn(&Theme) -> container_style::Style + Copy {
        move |_theme: &Theme| {
            container_style::Style::default()
                .background(Background::Color(background))
                .border(Border {
                    radius: 22.0.into(),
                    width: 1.0,
                    color: border_color,
                })
        }
    }
}

#[cfg(target_os = "linux")]
fn main() -> iced::Result {
    app::run()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("The iced player demo is currently available on Linux only.");
}
