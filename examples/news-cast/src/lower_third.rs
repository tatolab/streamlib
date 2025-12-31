// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Lower Third Processor
//!
//! GPU-native video effects processor using Vello for 2D rendering.
//! Renders graphics overlays on top of camera feed.

use std::sync::Arc;
use streamlib::core::{LinkInput, LinkOutput, Result, RuntimeContext, StreamError, VideoFrame};
use vello::{
    kurbo::{Affine, Rect},
    peniko::Color,
    util::RenderContext,
    AaSupport, RenderParams, Renderer, RendererOptions, Scene,
};

/// Configuration for lower third processor
#[derive(Clone, Default, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LowerThirdConfig {
    pub headline: String,
    pub subtitle: String,
}

/// Lower third processor with GPU effects
#[streamlib::processor(
    execution = Manual,
    description = "GPU-native lower third overlay using Vello 2D graphics for news-style video effects"
)]
pub struct LowerThirdProcessor {
    #[streamlib::input(description = "Input video frames to overlay lower third graphics on")]
    input: LinkInput<VideoFrame>,

    #[streamlib::output(description = "Output video frames with lower third overlay composited")]
    output: LinkOutput<VideoFrame>,

    #[streamlib::config]
    config: LowerThirdConfig,

    // Runtime state fields
    gpu_context: Option<streamlib::GpuContext>,
    title: String,
    subtitle: String,
    animation_time: f32,
    last_frame_timestamp_ns: Option<i64>,
    vello_renderer: Option<Renderer>,
    render_context: Option<RenderContext>,
    frame_width: u32,
    frame_height: u32,
    composite_pipeline: Option<wgpu::RenderPipeline>,
    composite_bind_group_layout: Option<wgpu::BindGroupLayout>,
}

impl LowerThirdProcessor::Processor {
    fn setup(&mut self, ctx: &RuntimeContext) -> Result<()> {
        self.gpu_context = Some(ctx.gpu.clone());
        self.title = self.config.headline.clone();
        self.subtitle = self.config.subtitle.clone();

        tracing::info!(
            "LowerThird: Received GPU context - device: {:p}, queue: {:p}",
            ctx.gpu.device().as_ref(),
            ctx.gpu.queue().as_ref()
        );

        tracing::info!(
            "LowerThird: Starting (title='{}', subtitle='{}')",
            self.title,
            self.subtitle
        );
        Ok(())
    }

    fn teardown(&mut self) -> Result<()> {
        tracing::info!("LowerThird: Stopping");
        Ok(())
    }

    fn process(&mut self) -> Result<()> {
        let input = match self.input.read() {
            Some(frame) => {
                // Convert nanoseconds to seconds for delta time calculation
                let delta_time = if let Some(last_ts_ns) = self.last_frame_timestamp_ns {
                    ((frame.timestamp_ns - last_ts_ns) as f64 / 1_000_000_000.0) as f32
                } else {
                    0.016 // Default ~60fps frame time
                };
                self.last_frame_timestamp_ns = Some(frame.timestamp_ns);
                self.animation_time += delta_time;

                if frame.frame_number % 60 == 0 {
                    tracing::info!(
                        "LowerThird: Received frame {} ({}x{}, timestamp={:.3}s) from input port",
                        frame.frame_number,
                        frame.width,
                        frame.height,
                        frame.timestamp_ns as f64 / 1_000_000_000.0
                    );
                }
                frame
            }
            None => return Ok(()),
        };

        let output = match self.render_overlay(&input) {
            Ok(frame) => {
                if frame.frame_number % 60 == 0 {
                    tracing::info!(
                        "LowerThird: Rendered overlay on frame {}, writing to output port",
                        frame.frame_number
                    );
                }
                frame
            }
            Err(e) => {
                tracing::warn!(
                    "LowerThird: Vello rendering failed ({}), passing through input frame {}",
                    e,
                    input.frame_number
                );
                input
            }
        };

        self.output.write(output);
        Ok(())
    }
}

// Helper methods for GPU rendering
impl LowerThirdProcessor::Processor {
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
            "LowerThird: Initialized Vello renderer ({}x{})",
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
            label: Some("Alpha Composite Shader"),
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
            label: Some("Composite Bind Group Layout"),
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
            label: Some("Composite Pipeline Layout"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Composite Pipeline"),
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

        tracing::info!("LowerThird: Initialized alpha compositing pipeline");
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
            label: Some("Vello Overlay"),
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
            label: Some("Composite Output"),
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
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });

        let vello_view = vello_texture.create_view(&wgpu::TextureViewDescriptor::default());

        let mut scene = Scene::new();

        let box_height = (input.height as f64) / 3.0;
        let box_y = (input.height as f64) - box_height;
        let box_rect = Rect::new(0.0, box_y, input.width as f64, input.height as f64);

        scene.fill(
            vello::peniko::Fill::NonZero,
            Affine::IDENTITY,
            Color::from_rgba8(30, 60, 120, 204),
            None,
            &box_rect,
        );

        let accent_rect = Rect::new(0.0, box_y, input.width as f64, box_y + 4.0);
        scene.fill(
            vello::peniko::Fill::NonZero,
            Affine::IDENTITY,
            Color::from_rgba8(255, 215, 0, 255),
            None,
            &accent_rect,
        );

        let renderer = self.vello_renderer.as_mut().unwrap();
        renderer
            .render_to_texture(
                device,
                queue,
                &scene,
                &vello_view,
                &RenderParams {
                    base_color: Color::TRANSPARENT,
                    width: input.width,
                    height: input.height,
                    antialiasing_method: vello::AaConfig::Area,
                },
            )
            .map_err(|e| StreamError::GpuError(format!("Vello render failed: {}", e)))?;

        let pipeline = self.composite_pipeline.as_ref().unwrap();
        let bind_group_layout = self.composite_bind_group_layout.as_ref().unwrap();

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("Composite Sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        let camera_view = input.texture.create_view(&wgpu::TextureViewDescriptor::default());
        let output_view = output_texture.create_view(&wgpu::TextureViewDescriptor::default());

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Composite Bind Group"),
            layout: bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&camera_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&vello_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("Composite Encoder"),
        });

        {
            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Composite Render Pass"),
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

        queue.submit(std::iter::once(encoder.finish()));

        Ok(VideoFrame {
            texture: Arc::new(output_texture),
            format: wgpu::TextureFormat::Rgba8Unorm,
            width: input.width,
            height: input.height,
            frame_number: input.frame_number,
            timestamp_ns: input.timestamp_ns,
        })
    }
}
