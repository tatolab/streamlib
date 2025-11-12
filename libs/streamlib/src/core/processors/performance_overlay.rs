use crate::core::{
    GpuContext, Result, StreamError, StreamInput,
    StreamOutput, VideoFrame,
};
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};
use streamlib_macros::StreamProcessor;

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct PerformanceOverlayConfig {
}

#[cfg(feature = "debug-overlay")]
use vello::{
    kurbo::{Affine, Line, Rect},
    peniko::Color,
    util::RenderContext,
    AaSupport, RenderParams, Renderer, RendererOptions, Scene,
};

struct PerformanceMetrics {
    frame_times: VecDeque<Duration>,
    last_frame_time: Option<Instant>,
    fps_current: f32,
    fps_min: f32,
    fps_max: f32,
    gpu_frame_times: VecDeque<Duration>,
    gpu_fps_current: f32,
    gpu_fps_min: f32,
    gpu_fps_max: f32,
    gpu_memory_mb: f32,
}

impl Default for PerformanceMetrics {
    fn default() -> Self {
        Self {
            frame_times: VecDeque::with_capacity(120),
            last_frame_time: None,
            fps_current: 0.0,
            fps_min: 0.0,
            fps_max: 0.0,
            gpu_frame_times: VecDeque::with_capacity(120),
            gpu_fps_current: 0.0,
            gpu_fps_min: 0.0,
            gpu_fps_max: 0.0,
            gpu_memory_mb: 0.0,
        }
    }
}

impl PerformanceMetrics {
    fn update(&mut self, frame: &VideoFrame) {
        let now = Instant::now();
        let frame_timestamp_secs = frame.timestamp;

        if let Some(last_time) = self.last_frame_time {
            // TODO: Use frame timestamps when we have stable monotonic camera timestamps
            let frame_duration = now.duration_since(last_time);

            self.frame_times.push_back(frame_duration);
            if self.frame_times.len() > 120 {
                self.frame_times.pop_front();
            }

            if frame_duration.as_secs_f32() > 0.0 {
                self.fps_current = 1.0 / frame_duration.as_secs_f32();
            }

            if !self.frame_times.is_empty() {
                let fps_values: Vec<f32> = self
                    .frame_times
                    .iter()
                    .filter_map(|d| {
                        let secs = d.as_secs_f32();
                        if secs > 0.0 {
                            Some(1.0 / secs)
                        } else {
                            None
                        }
                    })
                    .collect();

                self.fps_min = fps_values.iter().cloned().fold(f32::INFINITY, f32::min);
                self.fps_max = fps_values.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
            }
        }

        self.last_frame_time = Some(now);

        // TODO: Query GPU memory from frame metadata when available
        if let Some(metadata) = &frame.metadata {
            if let Some(mem_value) = metadata.get("gpu_memory_mb") {
                if let crate::MetadataValue::Float(mb) = mem_value {
                    self.gpu_memory_mb = *mb as f32;
                }
            }
        }
    }

    fn update_gpu(&mut self, gpu_duration: Duration) {
        self.gpu_frame_times.push_back(gpu_duration);
        if self.gpu_frame_times.len() > 120 {
            self.gpu_frame_times.pop_front();
        }

        if gpu_duration.as_secs_f32() > 0.0 {
            self.gpu_fps_current = 1.0 / gpu_duration.as_secs_f32();
        }

        if !self.gpu_frame_times.is_empty() {
            let gpu_fps_values: Vec<f32> = self
                .gpu_frame_times
                .iter()
                .filter_map(|d| {
                    let secs = d.as_secs_f32();
                    if secs > 0.0 {
                        Some(1.0 / secs)
                    } else {
                        None
                    }
                })
                .collect();

            self.gpu_fps_min = gpu_fps_values.iter().cloned().fold(f32::INFINITY, f32::min);
            self.gpu_fps_max = gpu_fps_values
                .iter()
                .cloned()
                .fold(f32::NEG_INFINITY, f32::max);
        }
    }
}

#[cfg(feature = "debug-overlay")]
#[derive(StreamProcessor)]
#[processor(
    mode = Pull,
    description = "Composites real-time performance metrics (FPS, GPU memory, frame time graph) onto video frames using Vello 2D graphics"
)]
pub struct PerformanceOverlayProcessor {
    #[input(description = "Input video frames to overlay performance metrics on")]
    video: StreamInput<VideoFrame>,

    #[output(description = "Output video frames with performance overlay composited")]
    video_out: StreamOutput<VideoFrame>,

    #[config]
    config: PerformanceOverlayConfig,

    // Runtime state fields - auto-detected (no attribute needed)
    gpu_context: Option<GpuContext>,
    metrics: PerformanceMetrics,

    #[cfg(feature = "debug-overlay")]
    profiler: Option<wgpu_profiler::GpuProfiler>,

    vello_renderer: Option<Renderer>,
    render_context: Option<RenderContext>,

    roboto_font: Vec<u8>,

    frame_width: u32,
    frame_height: u32,

    composite_pipeline: Option<wgpu::RenderPipeline>,
    composite_bind_group_layout: Option<wgpu::BindGroupLayout>,
}

#[cfg(feature = "debug-overlay")]
impl PerformanceOverlayProcessor {
    // Note: from_config() is auto-generated by macro. This helper can be removed if not needed.
    pub fn new() -> Result<Self> {
        let roboto_font = include_bytes!("./assets/Roboto-Regular.ttf").to_vec();

        Ok(Self {
            // Ports
            video: StreamInput::new("video"),
            video_out: StreamOutput::new("video"),

            // Config
            config: PerformanceOverlayConfig::default(),

            // Runtime state
            gpu_context: None,
            metrics: PerformanceMetrics::default(),
            profiler: None,
            vello_renderer: None,
            render_context: None,
            roboto_font,
            frame_width: 0,
            frame_height: 0,
            composite_pipeline: None,
            composite_bind_group_layout: None,
        })
    }

    fn draw_text(&self, scene: &mut Scene, text: &str, x: f32, y: f32, size: f32) {
        use skrifa::{raw::FileRef, raw::TableProvider, MetadataProvider};
        use vello::peniko::{Blob, Fill, Font};

        let font_ref = FileRef::new(&self.roboto_font).expect("Failed to parse font");
        let font = Font::new(Blob::new(Arc::new(self.roboto_font.clone())), 0);

        let font_ref = match font_ref {
            FileRef::Font(f) => f,
            FileRef::Collection(_) => panic!("Expected a single font"),
        };

        let charmap = font_ref.charmap();
        let head = font_ref.head().expect("Font missing head table");
        let units_per_em = head.units_per_em() as f32;
        let glyph_metrics = font_ref.glyph_metrics(
            skrifa::instance::Size::unscaled(),
            skrifa::instance::LocationRef::default(),
        );

        let scale = size / units_per_em;

        let mut pen_x = 0.0f32;
        scene
            .draw_glyphs(&font)
            .font_size(size)
            .transform(Affine::translate((x as f64, y as f64)))
            .brush(&vello::peniko::Brush::Solid(Color::from_rgb8(
                255, 255, 255,
            )))
            .draw(
                Fill::NonZero,
                text.chars().filter_map(|ch| {
                    let gid = charmap.map(ch)?;
                    let advance = glyph_metrics.advance_width(gid).unwrap_or_default();
                    let glyph_x = pen_x;
                    pen_x += advance * scale;
                    Some(vello::Glyph {
                        id: gid.to_u32(),
                        x: glyph_x,
                        y: 0.0,
                    })
                }),
            );
    }

    fn init_vello(&mut self, width: u32, height: u32) -> Result<()> {
        if self.vello_renderer.is_some() {
            return Ok(());
        }

        let gpu_context = self
            .gpu_context
            .as_ref()
            .ok_or_else(|| StreamError::Configuration("GPU context not initialized".into()))?;

        let render_context = RenderContext::new();

        let renderer = Renderer::new(
            gpu_context.device(),
            RendererOptions {
                use_cpu: false,
                antialiasing_support: AaSupport::all(),
                num_init_threads: None,
                pipeline_cache: None,
            },
        )
        .map_err(|e| StreamError::GpuError(format!("Failed to create Vello renderer: {}", e)))?;

        self.render_context = Some(render_context);
        self.vello_renderer = Some(renderer);
        self.frame_width = width;
        self.frame_height = height;

        tracing::info!(
            "PerformanceOverlay: Initialized Vello renderer ({}x{})",
            width,
            height
        );
        Ok(())
    }

    fn init_composite_pipeline(&mut self) -> Result<()> {
        if self.composite_pipeline.is_some() {
            return Ok(());
        }

        let gpu_context = self
            .gpu_context
            .as_ref()
            .ok_or_else(|| StreamError::Configuration("GPU context not initialized".into()))?;
        let device = gpu_context.device();

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Performance Overlay Composite Shader"),
            source: wgpu::ShaderSource::Wgsl(std::borrow::Cow::Borrowed(
                r#"
@group(0) @binding(0) var camera_texture: texture_2d<f32>;
@group(0) @binding(1) var overlay_texture: texture_2d<f32>;
@group(0) @binding(2) var texture_sampler: sampler;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) tex_coords: vec2<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    var out: VertexOutput;
    let x = f32((vertex_index & 1u) << 2u) - 1.0;
    let y = 1.0 - f32((vertex_index & 2u) << 1u);
    out.position = vec4<f32>(x, y, 0.0, 1.0);
    out.tex_coords = vec2<f32>((x + 1.0) * 0.5, (1.0 - y) * 0.5);
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let camera = textureSample(camera_texture, texture_sampler, in.tex_coords);
    let overlay = textureSample(overlay_texture, texture_sampler, in.tex_coords);

    let overlay_rgb = vec3<f32>(overlay.b, overlay.g, overlay.r);

    let alpha = overlay.a;
    let blended = overlay_rgb * alpha + camera.rgb * (1.0 - alpha);

    return vec4<f32>(blended, 1.0);
}
            "#,
            )),
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Performance Overlay Bind Group Layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Performance Overlay Pipeline Layout"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Performance Overlay Pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                unclipped_depth: false,
                polygon_mode: wgpu::PolygonMode::Fill,
                conservative: false,
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            multiview: None,
            cache: None,
        });

        self.composite_bind_group_layout = Some(bind_group_layout);
        self.composite_pipeline = Some(pipeline);

        tracing::info!("PerformanceOverlay: Initialized compositing pipeline");
        Ok(())
    }

    fn render_overlay(&mut self, input: &VideoFrame) -> Result<VideoFrame> {
        if self.vello_renderer.is_none() {
            self.init_vello(input.width, input.height)?;
        }
        if self.composite_pipeline.is_none() {
            self.init_composite_pipeline()?;
        }

        let gpu_context = self
            .gpu_context
            .as_ref()
            .ok_or_else(|| StreamError::Configuration("GPU context not initialized".into()))?;
        let device = gpu_context.device();
        let queue = gpu_context.queue();

        let vello_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Performance Overlay Texture"),
            size: wgpu::Extent3d {
                width: input.width,
                height: input.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::STORAGE_BINDING,
            view_formats: &[],
        });

        let output_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Performance Overlay Output"),
            size: wgpu::Extent3d {
                width: input.width,
                height: input.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });

        let vello_view = vello_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut scene = Scene::new();

        let padding = 20.0;
        let line_height = 25.0;
        let graph_height = 100.0;
        let graph_width = 200.0;

        let bg_rect = Rect::new(
            padding - 10.0,
            padding - 10.0,
            padding + graph_width + 10.0,
            padding + line_height * 5.0 + graph_height + 20.0,
        );
        scene.fill(
            vello::peniko::Fill::NonZero,
            Affine::IDENTITY,
            Color::from_rgba8(0, 0, 0, 180),
            None,
            &bg_rect,
        );

        let font_size = 16.0;
        let mut line_offset = 0.0;

        let fps_text = format!("CPU FPS: {:.1}", self.metrics.fps_current);
        self.draw_text(
            &mut scene,
            &fps_text,
            padding as f32,
            (padding + 15.0 + line_offset) as f32,
            font_size,
        );
        line_offset += line_height;

        let minmax_text = if self.metrics.fps_min < f32::INFINITY {
            format!(
                "  Min: {:.1}  Max: {:.1}",
                self.metrics.fps_min, self.metrics.fps_max
            )
        } else {
            "  Min: --  Max: --".to_string()
        };
        self.draw_text(
            &mut scene,
            &minmax_text,
            padding as f32,
            (padding + 15.0 + line_offset) as f32,
            font_size,
        );
        line_offset += line_height;

        if self.metrics.gpu_fps_current > 0.0 {
            let gpu_fps_text = format!("GPU FPS: {:.1}", self.metrics.gpu_fps_current);
            self.draw_text(
                &mut scene,
                &gpu_fps_text,
                padding as f32,
                (padding + 15.0 + line_offset) as f32,
                font_size,
            );
            line_offset += line_height;

            let gpu_minmax_text = if self.metrics.gpu_fps_min < f32::INFINITY {
                format!(
                    "  Min: {:.1}  Max: {:.1}",
                    self.metrics.gpu_fps_min, self.metrics.gpu_fps_max
                )
            } else {
                "  Min: --  Max: --".to_string()
            };
            self.draw_text(
                &mut scene,
                &gpu_minmax_text,
                padding as f32,
                (padding + 15.0 + line_offset) as f32,
                font_size,
            );
            line_offset += line_height;
        }

        if self.metrics.gpu_memory_mb > 0.0 {
            let mem_text = format!("GPU Mem: {:.1} MB", self.metrics.gpu_memory_mb);
            self.draw_text(
                &mut scene,
                &mem_text,
                padding as f32,
                (padding + 15.0 + line_offset) as f32,
                font_size,
            );
        }

        let graph_y = padding + line_height * 3.0;

        let graph_bg = Rect::new(
            padding,
            graph_y,
            padding + graph_width,
            graph_y + graph_height,
        );
        scene.fill(
            vello::peniko::Fill::NonZero,
            Affine::IDENTITY,
            Color::from_rgba8(40, 40, 40, 255),
            None,
            &graph_bg,
        );

        if !self.metrics.frame_times.is_empty() {
            let max_time_ms = 33.33; // 30 FPS baseline
            let x_scale = graph_width / 120.0;
            let y_scale = graph_height / max_time_ms;

            for (i, duration) in self.metrics.frame_times.iter().enumerate() {
                let time_ms = duration.as_secs_f64() * 1000.0;
                let x = padding + (i as f64) * x_scale;
                let y_offset = (time_ms.min(max_time_ms)) * y_scale;
                let y = graph_y + graph_height - y_offset;

                let color = if time_ms < 16.67 {
                    Color::from_rgb8(0, 255, 0) // >60 FPS = green
                } else if time_ms < 33.33 {
                    Color::from_rgb8(255, 255, 0) // 30-60 FPS = yellow
                } else {
                    Color::from_rgb8(255, 0, 0) // <30 FPS = red
                };

                let line = Line::new((x, graph_y + graph_height), (x, y));
                scene.stroke(
                    &vello::kurbo::Stroke::new(2.0),
                    Affine::IDENTITY,
                    color,
                    None,
                    &line,
                );
            }
        }

        let renderer = self.vello_renderer.as_mut().unwrap();
        let render_params = RenderParams {
            base_color: Color::TRANSPARENT,
            width: input.width,
            height: input.height,
            antialiasing_method: vello::AaConfig::Area,
        };

        renderer
            .render_to_texture(device, queue, &scene, &vello_view, &render_params)
            .map_err(|e| StreamError::GpuError(format!("Failed to render Vello overlay: {}", e)))?;

        let pipeline = self.composite_pipeline.as_ref().unwrap();
        let bind_group_layout = self.composite_bind_group_layout.as_ref().unwrap();

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("Performance Overlay Sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        let input_view = input
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let overlay_view = vello_texture.create_view(&wgpu::TextureViewDescriptor::default());

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Performance Overlay Bind Group"),
            layout: bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&input_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&overlay_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });

        let output_view = output_texture.create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("Performance Overlay Encoder"),
        });

        if let Some(profiler) = &self.profiler {
            let mut scope = profiler.scope("performance_overlay", &mut encoder);

            let mut render_pass = scope.scoped_render_pass(
                "composite",
                wgpu::RenderPassDescriptor {
                    label: Some("Performance Overlay Composite Pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &output_view,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                },
            );

            render_pass.set_pipeline(pipeline);
            render_pass.set_bind_group(0, &bind_group, &[]);
            render_pass.draw(0..3, 0..1);
        } else {
            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Performance Overlay Composite Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &output_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            render_pass.set_pipeline(pipeline);
            render_pass.set_bind_group(0, &bind_group, &[]);
            render_pass.draw(0..3, 0..1);
        }

        if let Some(profiler) = &mut self.profiler {
            profiler.resolve_queries(&mut encoder);
        }

        queue.submit(Some(encoder.finish()));

        if let Some(profiler) = &mut self.profiler {
            profiler.end_frame().map_err(|e| {
                StreamError::GpuError(format!("Failed to end profiler frame: {}", e))
            })?;

            if let Some(frame_data) = profiler.process_finished_frame(queue.get_timestamp_period())
            {
                tracing::debug!(
                    "PerformanceOverlay: Got {} profiler scopes",
                    frame_data.len()
                );

                for scope in &frame_data {
                    tracing::debug!(
                        "PerformanceOverlay: Scope '{}' - time: {:?}",
                        scope.label,
                        scope.time
                    );

                    if scope.label == "performance_overlay" {
                        if let Some(time_range) = &scope.time {
                            let gpu_duration_ns =
                                ((time_range.end - time_range.start) * 1_000_000_000.0) as u64;
                            let gpu_duration = Duration::from_nanos(gpu_duration_ns);
                            tracing::info!(
                                "PerformanceOverlay: GPU frame time: {:?} ({:.2} ms)",
                                gpu_duration,
                                gpu_duration.as_secs_f64() * 1000.0
                            );
                            self.metrics.update_gpu(gpu_duration);
                            break;
                        } else {
                            tracing::warn!("PerformanceOverlay: Found scope but no time data");
                        }
                    }
                }
            } else {
                tracing::debug!("PerformanceOverlay: No profiler frame data available yet");
            }
        }

        Ok(VideoFrame {
            texture: Arc::new(output_texture),
            format: wgpu::TextureFormat::Rgba8Unorm,
            width: input.width,
            height: input.height,
            frame_number: input.frame_number,
            timestamp: input.timestamp,
            metadata: input.metadata.clone(),
        })
    }

    // Lifecycle - auto-detected by macro
    fn setup(&mut self, ctx: &crate::core::RuntimeContext) -> Result<()> {
        let gpu_context = &ctx.gpu;
        self.gpu_context = Some(gpu_context.clone());

        tracing::info!("PerformanceOverlay: Creating GPU profiler...");
        let profiler = wgpu_profiler::GpuProfiler::new(
            gpu_context.device(),
            wgpu_profiler::GpuProfilerSettings {
                max_num_pending_frames: 3,
                enable_timer_queries: true,
                enable_debug_groups: true,
            },
        )
        .map_err(|e| {
            tracing::error!("PerformanceOverlay: Failed to create GPU profiler: {:?}", e);
            StreamError::GpuError(format!("Failed to create GPU profiler: {:?}", e))
        })?;

        self.profiler = Some(profiler);

        tracing::info!("PerformanceOverlay: GPU profiler initialized successfully");
        Ok(())
    }

    fn teardown(&mut self) -> Result<()> {
        Ok(())
    }

    // Business logic - called by macro-generated process()
    fn process(&mut self) -> Result<()> {
        let input = match self.video.read_latest() {
            Some(frame) => frame,
            None => {
                return Ok(());
            }
        };

        self.metrics.update(&input);

        let output = match self.render_overlay(&input) {
            Ok(frame) => frame,
            Err(e) => {
                tracing::warn!(
                    "PerformanceOverlay: Failed to render overlay, passing through: {}",
                    e
                );
                input // Pass through on error
            }
        };

        self.video_out.write(output);

        Ok(())
    }
}

#[cfg(all(test, feature = "debug-overlay"))]
mod tests {
    use crate::core::*;

    #[test]
    fn test_performance_overlay_descriptor() {
        let descriptor = PerformanceOverlayProcessor::descriptor()
            .expect("PerformanceOverlayProcessor should have descriptor");

        assert_eq!(descriptor.name, "PerformanceOverlayProcessor");
        assert!(descriptor.description.contains("performance"));
        assert!(descriptor.description.contains("FPS"));
        assert!(descriptor.usage_context.is_some());
        assert!(descriptor.usage_context.as_ref().unwrap().contains("debug"));

        assert_eq!(descriptor.inputs.len(), 1);
        assert_eq!(descriptor.inputs[0].name, "video");
        assert_eq!(descriptor.inputs[0].schema.name, "VideoFrame");
        assert!(descriptor.inputs[0].required);

        assert_eq!(descriptor.outputs.len(), 1);
        assert_eq!(descriptor.outputs[0].name, "video");
        assert_eq!(descriptor.outputs[0].schema.name, "VideoFrame");
        assert!(descriptor.outputs[0].required);

        assert!(descriptor.tags.contains(&"debug".to_string()));
        assert!(descriptor.tags.contains(&"performance".to_string()));
        assert!(descriptor.tags.contains(&"fps".to_string()));
    }

    #[test]
    fn test_performance_overlay_descriptor_serialization() {
        let descriptor = PerformanceOverlayProcessor::descriptor()
            .expect("PerformanceOverlayProcessor should have descriptor");

        let json = descriptor.to_json().expect("Failed to serialize to JSON");
        assert!(json.contains("PerformanceOverlayProcessor"));
        assert!(json.contains("performance"));
        assert!(json.contains("FPS"));

        let yaml = descriptor.to_yaml().expect("Failed to serialize to YAML");
        assert!(yaml.contains("PerformanceOverlayProcessor"));
        assert!(yaml.contains("debug"));
    }
}
