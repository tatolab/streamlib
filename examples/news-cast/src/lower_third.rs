//! Lower Third Processor
//!
//! GPU-native video effects processor using WebGPU compute shaders.
//! Implements a grayscale filter using the same pattern as Python streamlib.

use streamlib::{
    StreamProcessor, StreamInput, StreamOutput, VideoFrame,
    TimedTick, Result, StreamError,
};
use std::sync::Arc;
use wgpu;

/// Input ports for lower third processor
pub struct LowerThirdInputPorts {
    pub video: StreamInput<VideoFrame>,
}

/// Output ports for lower third processor
pub struct LowerThirdOutputPorts {
    pub video: StreamOutput<VideoFrame>,
}

/// Lower third processor with GPU effects
pub struct LowerThirdProcessor {
    // GPU context (shared with all processors via runtime)
    gpu_context: Option<streamlib::GpuContext>,

    // Content
    title: String,
    subtitle: String,

    // Animation state
    animation_time: f32,

    // Stream I/O (using ports pattern like camera/display)
    input_ports: LowerThirdInputPorts,
    output_ports: LowerThirdOutputPorts,

    // Grayscale compute pipeline (using Python's pattern)
    grayscale_pipeline: Option<wgpu::ComputePipeline>,
    grayscale_bind_group_layout: Option<wgpu::BindGroupLayout>,
}

impl LowerThirdProcessor {
    pub fn new(
        title: String,
        subtitle: String,
    ) -> Result<Self> {
        tracing::info!("LowerThird: Initializing (GPU context will be provided by runtime)");

        Ok(Self {
            gpu_context: None,  // Will be set by runtime in on_start()
            title,
            subtitle,
            animation_time: 0.0,
            input_ports: LowerThirdInputPorts {
                video: StreamInput::new("video"),
            },
            output_ports: LowerThirdOutputPorts {
                video: StreamOutput::new("video"),
            },
            grayscale_pipeline: None,
            grayscale_bind_group_layout: None,
        })
    }

    /// Create or get grayscale compute pipeline (lazy initialization)
    /// Uses the same pattern as Python streamlib
    fn get_or_create_grayscale_pipeline(&mut self) -> Result<()> {
        if self.grayscale_pipeline.is_some() {
            return Ok(());
        }

        let gpu_context = self.gpu_context.as_ref()
            .ok_or_else(|| StreamError::Configuration("GPU context not initialized".into()))?;
        let device = gpu_context.device();

        // Create bind group layout for compute shader
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Grayscale Bind Group Layout"),
            entries: &[
                // Input texture (read-only)
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                // Output texture (write-only storage)
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: wgpu::TextureFormat::Rgba8Unorm, // Storage textures must be Rgba, not Bgra
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
            ],
        });

        // Create compute shader (same as Python's GRAYSCALE_SHADER)
        let shader_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Grayscale Compute Shader"),
            source: wgpu::ShaderSource::Wgsl(std::borrow::Cow::Borrowed(
                r#"
@group(0) @binding(0) var input_texture: texture_2d<f32>;
@group(0) @binding(1) var output_texture: texture_storage_2d<rgba8unorm, write>;

@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(input_texture);
    let coord = vec2<i32>(gid.xy);

    if (coord.x >= i32(dims.x) || coord.y >= i32(dims.y)) {
        return;
    }

    let color = textureLoad(input_texture, coord, 0);

    // Standard luminance weights (ITU-R BT.709)
    let gray = dot(color.rgb, vec3<f32>(0.2126, 0.7152, 0.0722));
    let result = vec4<f32>(gray, gray, gray, color.a);

    textureStore(output_texture, coord, result);
}
                "#
            )),
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Grayscale Pipeline Layout"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("Grayscale Compute Pipeline"),
            layout: Some(&pipeline_layout),
            module: &shader_module,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });

        self.grayscale_pipeline = Some(pipeline);
        self.grayscale_bind_group_layout = Some(bind_group_layout);

        tracing::info!("LowerThird: Created grayscale compute pipeline");
        Ok(())
    }

    /// Apply grayscale effect to input frame using compute shader
    /// Pattern matches Python's gpu_context.run_compute()
    fn apply_grayscale(&mut self, input: &VideoFrame) -> Result<VideoFrame> {
        // Ensure pipeline exists
        self.get_or_create_grayscale_pipeline()?;

        // Camera already provides WebGPU-owned textures, no need to copy!
        // Just use the input texture directly.

        let gpu_context = self.gpu_context.as_ref()
            .ok_or_else(|| StreamError::Configuration("GPU context not initialized".into()))?;
        let device = gpu_context.device();
        let queue = gpu_context.queue();

        // Create WebGPU-owned output texture
        let output_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Grayscale Output"),
            size: wgpu::Extent3d {
                width: input.width,
                height: input.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm, // Storage textures must be Rgba, not Bgra
            // STORAGE_BINDING: compute shader writes to it (exclusive usage!)
            // COPY_SRC: display processor uses unwrap_to_metal_texture() to read via blit
            // NOTE: Cannot use TEXTURE_BINDING with STORAGE_BINDING - they conflict in same scope
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });

        // Create texture views
        let input_view = input.texture.create_view(&wgpu::TextureViewDescriptor::default());
        let output_view = output_texture.create_view(&wgpu::TextureViewDescriptor::default());

        // Create bind group
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Grayscale Bind Group"),
            layout: self.grayscale_bind_group_layout.as_ref().unwrap(),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&input_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&output_view),
                },
            ],
        });

        // Encode & submit compute pass (all in one operation)
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("Grayscale Encoder"),
        });

        {
            let mut compute_pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("Grayscale Pass"),
                timestamp_writes: None,
            });
            compute_pass.set_pipeline(self.grayscale_pipeline.as_ref().unwrap());
            compute_pass.set_bind_group(0, &bind_group, &[]);

            // Dispatch workgroups (8x8 workgroup size, match Python)
            let workgroups_x = (input.width + 7) / 8;
            let workgroups_y = (input.height + 7) / 8;
            compute_pass.dispatch_workgroups(workgroups_x, workgroups_y, 1);
        }

        // Submit immediately
        queue.submit(std::iter::once(encoder.finish()));

        // Return new frame with WebGPU-owned texture
        Ok(VideoFrame {
            texture: Arc::new(output_texture),
            width: input.width,
            height: input.height,
            frame_number: input.frame_number,
            timestamp: input.timestamp,
            metadata: input.metadata.clone(),
        })
    }

    /// Get mutable reference to input ports
    pub fn input_ports(&mut self) -> &mut LowerThirdInputPorts {
        &mut self.input_ports
    }

    /// Get mutable reference to output ports
    pub fn output_ports(&mut self) -> &mut LowerThirdOutputPorts {
        &mut self.output_ports
    }
}

impl StreamProcessor for LowerThirdProcessor {
    fn process(&mut self, tick: TimedTick) -> Result<()> {
        // Update animation
        self.animation_time += tick.delta_time as f32;

        // Read input frame
        let input = match self.input_ports.video.read_latest() {
            Some(frame) => {
                // Debug: Log every 60 frames (once per second at 60fps)
                if frame.frame_number % 60 == 0 {
                    tracing::info!(
                        "LowerThird: Received frame {} ({}x{}, timestamp={:.3}s) from input port",
                        frame.frame_number,
                        frame.width,
                        frame.height,
                        frame.timestamp
                    );
                }
                frame
            },
            None => {
                // No input available - don't output anything
                return Ok(());
            },
        };

        // Apply grayscale effect
        let output = self.apply_grayscale(&input)?;

        // Debug: Log every 60 frames (once per second at 60fps)
        if output.frame_number % 60 == 0 {
            tracing::info!(
                "LowerThird: Applied grayscale to frame {}, writing to output port",
                output.frame_number
            );
        }

        self.output_ports.video.write(output);

        Ok(())
    }

    fn on_start(&mut self, gpu_context: &streamlib::GpuContext) -> Result<()> {
        // Store the shared GPU context from runtime
        self.gpu_context = Some(gpu_context.clone());

        // Log device/queue addresses to verify all processors share same context
        tracing::info!(
            "LowerThird: Received GPU context - device: {:p}, queue: {:p}",
            gpu_context.device().as_ref(),
            gpu_context.queue().as_ref()
        );

        tracing::info!(
            "LowerThird: Starting (title='{}', subtitle='{}')",
            self.title,
            self.subtitle
        );
        Ok(())
    }

    fn on_stop(&mut self) -> Result<()> {
        tracing::info!("LowerThird: Stopping");
        Ok(())
    }
}
