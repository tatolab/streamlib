//! Performance Overlay Processor
//!
//! GPU-accelerated performance monitoring overlay using Vello for rendering.
//! Displays FPS, GPU memory usage, and a real-time performance graph.
//!
//! Only available with the `debug-overlay` feature flag.

use crate::{
    StreamProcessor, StreamInput, StreamOutput, VideoFrame,
    Result, StreamError, GpuContext,
};
use std::sync::Arc;
use std::time::{Duration, Instant};
use std::collections::VecDeque;

#[cfg(feature = "debug-overlay")]
use vello::{
    kurbo::{Affine, Rect, Line},
    peniko::Color,
    util::RenderContext,
    AaSupport, RenderParams, Renderer, RendererOptions, Scene,
};

/// Performance metrics tracking
struct PerformanceMetrics {
    /// Rolling buffer of frame times (last 120 frames = 2 seconds @ 60fps)
    frame_times: VecDeque<Duration>,
    /// Last frame timestamp
    last_frame_time: Option<Instant>,
    /// Current CPU FPS (wall-clock measurement)
    fps_current: f32,
    /// Minimum CPU FPS (over window)
    fps_min: f32,
    /// Maximum CPU FPS (over window)
    fps_max: f32,
    /// Rolling buffer of GPU render times (from timestamp queries)
    gpu_frame_times: VecDeque<Duration>,
    /// Current GPU FPS (actual GPU execution time)
    gpu_fps_current: f32,
    /// Minimum GPU FPS (over window)
    gpu_fps_min: f32,
    /// Maximum GPU FPS (over window)
    gpu_fps_max: f32,
    /// GPU memory usage in MB (platform-dependent)
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
        // Use frame timestamp for accurate measurement (not wall clock)
        let now = Instant::now();
        let frame_timestamp_secs = frame.timestamp;

        if let Some(last_time) = self.last_frame_time {
            // Calculate duration since last frame using wall clock for now
            // TODO: Use frame timestamps when we have stable monotonic camera timestamps
            let frame_duration = now.duration_since(last_time);

            // Add to rolling buffer
            self.frame_times.push_back(frame_duration);
            if self.frame_times.len() > 120 {
                self.frame_times.pop_front();
            }

            // Calculate FPS
            if frame_duration.as_secs_f32() > 0.0 {
                self.fps_current = 1.0 / frame_duration.as_secs_f32();
            }

            // Calculate min/max FPS
            if !self.frame_times.is_empty() {
                let fps_values: Vec<f32> = self.frame_times
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
        // For now, this would come from platform-specific display processor
        if let Some(metadata) = &frame.metadata {
            if let Some(mem_value) = metadata.get("gpu_memory_mb") {
                if let crate::MetadataValue::Float(mb) = mem_value {
                    self.gpu_memory_mb = *mb as f32;
                }
            }
        }
    }

    /// Update GPU metrics from timestamp query results
    fn update_gpu(&mut self, gpu_duration: Duration) {
        // Add to rolling buffer
        self.gpu_frame_times.push_back(gpu_duration);
        if self.gpu_frame_times.len() > 120 {
            self.gpu_frame_times.pop_front();
        }

        // Calculate GPU FPS from render time
        if gpu_duration.as_secs_f32() > 0.0 {
            self.gpu_fps_current = 1.0 / gpu_duration.as_secs_f32();
        }

        // Calculate min/max GPU FPS
        if !self.gpu_frame_times.is_empty() {
            let gpu_fps_values: Vec<f32> = self.gpu_frame_times
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
            self.gpu_fps_max = gpu_fps_values.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
        }
    }
}

/// Input ports for performance overlay processor
pub struct PerformanceOverlayInputPorts {
    /// Video input
    pub video: StreamInput<VideoFrame>,
}

/// Output ports for performance overlay processor
pub struct PerformanceOverlayOutputPorts {
    /// Video output with performance overlay composited
    pub video: StreamOutput<VideoFrame>,
}

/// Performance overlay processor
///
/// Renders real-time performance metrics on top of video frames using Vello.
/// Tracks FPS, GPU memory, and displays a performance graph.
#[cfg(feature = "debug-overlay")]
pub struct PerformanceOverlayProcessor {
    gpu_context: Option<GpuContext>,
    input_ports: PerformanceOverlayInputPorts,
    output_ports: PerformanceOverlayOutputPorts,

    // Performance tracking
    metrics: PerformanceMetrics,

    // GPU profiler for accurate GPU timing (using timestamp queries)
    #[cfg(feature = "debug-overlay")]
    profiler: Option<wgpu_profiler::GpuProfiler>,

    // Vello renderer for overlay graphics
    vello_renderer: Option<Renderer>,
    render_context: Option<RenderContext>,

    // Font data for text rendering
    roboto_font: Vec<u8>,

    // Frame dimensions
    frame_width: u32,
    frame_height: u32,

    // Alpha compositing pipeline (to blend overlay on camera feed)
    composite_pipeline: Option<wgpu::RenderPipeline>,
    composite_bind_group_layout: Option<wgpu::BindGroupLayout>,
}

#[cfg(feature = "debug-overlay")]
impl PerformanceOverlayProcessor {
    /// Create a new performance overlay processor
    pub fn new() -> Result<Self> {
        // Load Roboto font data at initialization
        let roboto_font = include_bytes!("../../assets/Roboto-Regular.ttf").to_vec();

        Ok(Self {
            gpu_context: None,
            input_ports: PerformanceOverlayInputPorts {
                video: StreamInput::new("video"),
            },
            output_ports: PerformanceOverlayOutputPorts {
                video: StreamOutput::new("video"),
            },
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

    /// Get mutable reference to input ports
    pub fn input_ports(&mut self) -> &mut PerformanceOverlayInputPorts {
        &mut self.input_ports
    }

    /// Get mutable reference to output ports
    pub fn output_ports(&mut self) -> &mut PerformanceOverlayOutputPorts {
        &mut self.output_ports
    }

    /// Render text using Vello's draw_glyphs (simple method from vello examples)
    fn draw_text(&self, scene: &mut Scene, text: &str, x: f32, y: f32, size: f32) {
        use skrifa::{raw::FileRef, MetadataProvider, raw::TableProvider};
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
        let glyph_metrics = font_ref.glyph_metrics(skrifa::instance::Size::unscaled(), skrifa::instance::LocationRef::default());

        // Scale factor to convert font units to pixels at the given font size
        let scale = size / units_per_em;

        let mut pen_x = 0.0f32;
        scene
            .draw_glyphs(&font)
            .font_size(size)
            .transform(Affine::translate((x as f64, y as f64)))
            .brush(&vello::peniko::Brush::Solid(Color::from_rgb8(255, 255, 255)))
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

    /// Initialize Vello renderer (called lazily on first frame)
    fn init_vello(&mut self, width: u32, height: u32) -> Result<()> {
        if self.vello_renderer.is_some() {
            return Ok(());
        }

        let gpu_context = self.gpu_context.as_ref()
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

        tracing::info!("PerformanceOverlay: Initialized Vello renderer ({}x{})", width, height);
        Ok(())
    }

    /// Initialize alpha compositing pipeline (called lazily on first frame)
    fn init_composite_pipeline(&mut self) -> Result<()> {
        if self.composite_pipeline.is_some() {
            return Ok(());
        }

        let gpu_context = self.gpu_context.as_ref()
            .ok_or_else(|| StreamError::Configuration("GPU context not initialized".into()))?;
        let device = gpu_context.device();

        // Shader for alpha blending two textures
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Performance Overlay Composite Shader"),
            source: wgpu::ShaderSource::Wgsl(std::borrow::Cow::Borrowed(r#"
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
    // Full-screen triangle
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

    // Vello outputs BGR, so swap R and B channels
    let overlay_rgb = vec3<f32>(overlay.b, overlay.g, overlay.r);

    // Standard alpha blending: result = overlay * alpha + camera * (1 - alpha)
    let alpha = overlay.a;
    let blended = overlay_rgb * alpha + camera.rgb * (1.0 - alpha);

    return vec4<f32>(blended, 1.0);
}
            "#)),
        });

        // Create bind group layout
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

    /// Render performance overlay using Vello
    fn render_overlay(&mut self, input: &VideoFrame) -> Result<VideoFrame> {
        // Initialize Vello and compositing pipeline on first frame
        if self.vello_renderer.is_none() {
            self.init_vello(input.width, input.height)?;
        }
        if self.composite_pipeline.is_none() {
            self.init_composite_pipeline()?;
        }

        let gpu_context = self.gpu_context.as_ref()
            .ok_or_else(|| StreamError::Configuration("GPU context not initialized".into()))?;
        let device = gpu_context.device();
        let queue = gpu_context.queue();

        // Create Vello overlay texture (transparent background)
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

        // Create final output texture
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

        // Step 1: Render Vello overlay with transparent background
        let vello_view = vello_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut scene = Scene::new();

        // Draw performance stats in top-left corner
        let padding = 20.0;
        let line_height = 25.0;
        let graph_height = 100.0;
        let graph_width = 200.0;

        // Semi-transparent black background for text (larger to accommodate GPU stats)
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

        // Render FPS text
        let font_size = 16.0;
        let mut line_offset = 0.0;

        // Line 1: CPU FPS (wall-clock timing)
        let fps_text = format!("CPU FPS: {:.1}", self.metrics.fps_current);
        self.draw_text(&mut scene, &fps_text, padding as f32, (padding + 15.0 + line_offset) as f32, font_size);
        line_offset += line_height;

        // Line 2: CPU Min/Max FPS
        let minmax_text = if self.metrics.fps_min < f32::INFINITY {
            format!("  Min: {:.1}  Max: {:.1}", self.metrics.fps_min, self.metrics.fps_max)
        } else {
            "  Min: --  Max: --".to_string()
        };
        self.draw_text(&mut scene, &minmax_text, padding as f32, (padding + 15.0 + line_offset) as f32, font_size);
        line_offset += line_height;

        // Line 3: GPU FPS (timestamp query timing)
        if self.metrics.gpu_fps_current > 0.0 {
            let gpu_fps_text = format!("GPU FPS: {:.1}", self.metrics.gpu_fps_current);
            self.draw_text(&mut scene, &gpu_fps_text, padding as f32, (padding + 15.0 + line_offset) as f32, font_size);
            line_offset += line_height;

            // Line 4: GPU Min/Max FPS
            let gpu_minmax_text = if self.metrics.gpu_fps_min < f32::INFINITY {
                format!("  Min: {:.1}  Max: {:.1}", self.metrics.gpu_fps_min, self.metrics.gpu_fps_max)
            } else {
                "  Min: --  Max: --".to_string()
            };
            self.draw_text(&mut scene, &gpu_minmax_text, padding as f32, (padding + 15.0 + line_offset) as f32, font_size);
            line_offset += line_height;
        }

        // Line N: GPU Memory (if available)
        if self.metrics.gpu_memory_mb > 0.0 {
            let mem_text = format!("GPU Mem: {:.1} MB", self.metrics.gpu_memory_mb);
            self.draw_text(&mut scene, &mem_text, padding as f32, (padding + 15.0 + line_offset) as f32, font_size);
        }

        // Draw FPS graph (last 120 frames)
        let graph_y = padding + line_height * 3.0;

        // Graph background
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

        // Draw frame time lines
        if !self.metrics.frame_times.is_empty() {
            let max_time_ms = 33.33; // 30 FPS baseline
            let x_scale = graph_width / 120.0;
            let y_scale = graph_height / max_time_ms;

            for (i, duration) in self.metrics.frame_times.iter().enumerate() {
                let time_ms = duration.as_secs_f64() * 1000.0;
                let x = padding + (i as f64) * x_scale;
                let y_offset = (time_ms.min(max_time_ms)) * y_scale;
                let y = graph_y + graph_height - y_offset;

                // Color based on performance (green = good, yellow = ok, red = bad)
                let color = if time_ms < 16.67 {
                    Color::from_rgb8(0, 255, 0)  // >60 FPS = green
                } else if time_ms < 33.33 {
                    Color::from_rgb8(255, 255, 0)  // 30-60 FPS = yellow
                } else {
                    Color::from_rgb8(255, 0, 0)  // <30 FPS = red
                };

                let line = Line::new(
                    (x, graph_y + graph_height),
                    (x, y),
                );
                scene.stroke(
                    &vello::kurbo::Stroke::new(2.0),
                    Affine::IDENTITY,
                    color,
                    None,
                    &line,
                );
            }
        }

        // Render Vello scene to texture
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

        // Step 2: Use compositing shader to blend camera + overlay
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

        let input_view = input.texture.create_view(&wgpu::TextureViewDescriptor::default());
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

        // Create GPU profiler scope for compositing pass
        if let Some(profiler) = &self.profiler {
            // Use scoped render pass for automatic timing
            let mut scope = profiler.scope("performance_overlay", &mut encoder);

            let mut render_pass = scope.scoped_render_pass("composite", wgpu::RenderPassDescriptor {
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
        } else {
            // No profiler - just do normal render pass
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

        // Resolve profiler queries
        if let Some(profiler) = &mut self.profiler {
            profiler.resolve_queries(&mut encoder);
        }

        queue.submit(Some(encoder.finish()));

        // Process profiler results
        if let Some(profiler) = &mut self.profiler {
            profiler.end_frame().map_err(|e| {
                StreamError::GpuError(format!("Failed to end profiler frame: {}", e))
            })?;

            // Extract timing data
            if let Some(frame_data) = profiler.process_finished_frame(queue.get_timestamp_period()) {
                tracing::debug!("PerformanceOverlay: Got {} profiler scopes", frame_data.len());

                // Find the performance_overlay scope timing
                for scope in &frame_data {
                    tracing::debug!("PerformanceOverlay: Scope '{}' - time: {:?}", scope.label, scope.time);

                    if scope.label == "performance_overlay" {
                        if let Some(time_range) = &scope.time {
                            let gpu_duration_ns = ((time_range.end - time_range.start) * 1_000_000_000.0) as u64;
                            let gpu_duration = Duration::from_nanos(gpu_duration_ns);
                            tracing::info!("PerformanceOverlay: GPU frame time: {:?} ({:.2} ms)", gpu_duration, gpu_duration.as_secs_f64() * 1000.0);
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
}

#[cfg(feature = "debug-overlay")]
impl StreamProcessor for PerformanceOverlayProcessor {
    fn on_start(&mut self, gpu_context: &GpuContext) -> Result<()> {
        self.gpu_context = Some(gpu_context.clone());

        // Initialize GPU profiler for accurate timing measurements
        tracing::info!("PerformanceOverlay: Creating GPU profiler...");
        let profiler = wgpu_profiler::GpuProfiler::new(
            gpu_context.device(),
            wgpu_profiler::GpuProfilerSettings {
                max_num_pending_frames: 3,
                enable_timer_queries: true,
                enable_debug_groups: true,
            }
        ).map_err(|e| {
            tracing::error!("PerformanceOverlay: Failed to create GPU profiler: {:?}", e);
            StreamError::GpuError(format!("Failed to create GPU profiler: {:?}", e))
        })?;

        self.profiler = Some(profiler);

        tracing::info!("PerformanceOverlay: GPU profiler initialized successfully");
        Ok(())
    }

    fn process(&mut self, _tick: crate::clock::TimedTick) -> Result<()> {
        // Read input frame
        let input = match self.input_ports.video.read_latest() {
            Some(frame) => frame,
            None => {
                // No input available - don't output anything
                return Ok(());
            }
        };

        // Update metrics
        self.metrics.update(&input);

        // Render overlay on top of input frame
        let output = match self.render_overlay(&input) {
            Ok(frame) => frame,
            Err(e) => {
                tracing::warn!("PerformanceOverlay: Failed to render overlay, passing through: {}", e);
                input  // Pass through on error
            }
        };

        // Write to output port
        self.output_ports.video.write(output);

        Ok(())
    }
}

// Export input/output port types
pub use PerformanceOverlayInputPorts as PerformanceOverlayInput;
pub use PerformanceOverlayOutputPorts as PerformanceOverlayOutput;
