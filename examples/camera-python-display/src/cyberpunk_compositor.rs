// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Cyberpunk Compositor - Person segmentation with cyberpunk background.
//!
//! Uses Vision framework for person segmentation and Metal for compositing.

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::AnyThread;
use objc2_core_video::CVPixelBuffer;
use objc2_foundation::{NSArray, NSDictionary};
use objc2_io_surface::IOSurface;
use objc2_metal::MTLTexture;
use objc2_vision::{
    VNGeneratePersonSegmentationRequest, VNGeneratePersonSegmentationRequestQualityLevel,
    VNImageOption, VNImageRequestHandler, VNPixelBufferObservation, VNRequest,
};
use serde::{Deserialize, Serialize};
use std::ffi::c_void;
use std::path::PathBuf;
use std::ptr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use streamlib::core::context::texture_pool::TexturePoolDescriptor;
use streamlib::core::rhi::TextureFormat;
use streamlib::core::{
    GpuContext, LinkInput, LinkOutput, Result, RuntimeContext, StreamError, VideoFrame,
};

mod ffi {
    use objc2::msg_send;
    use objc2::rc::Retained;
    use objc2::runtime::ProtocolObject;
    use objc2_core_video::CVPixelBuffer;
    use objc2_io_surface::{IOSurface, IOSurfaceRef};
    use objc2_metal::{
        MTLDevice, MTLPixelFormat, MTLTexture, MTLTextureDescriptor, MTLTextureUsage,
    };
    use std::ffi::c_void;
    use std::path::Path;

    // Core Graphics types for image loading
    pub type CGImageRef = *const c_void;
    pub type CGDataProviderRef = *const c_void;
    pub type CGColorSpaceRef = *const c_void;
    pub type CFURLRef = *const c_void;
    pub type CFStringRef = *const c_void;

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        pub fn CGImageGetWidth(image: CGImageRef) -> usize;
        pub fn CGImageGetHeight(image: CGImageRef) -> usize;
        pub fn CGColorSpaceCreateDeviceRGB() -> CGColorSpaceRef;
        pub fn CGColorSpaceRelease(space: CGColorSpaceRef);
        pub fn CGContextDrawImage(context: *const c_void, rect: CGRect, image: CGImageRef);
        pub fn CGBitmapContextCreate(
            data: *mut c_void,
            width: usize,
            height: usize,
            bits_per_component: usize,
            bytes_per_row: usize,
            space: CGColorSpaceRef,
            bitmap_info: u32,
        ) -> *const c_void;
        pub fn CGContextRelease(context: *const c_void);
        pub fn CGDataProviderCreateWithURL(url: CFURLRef) -> CGDataProviderRef;
        pub fn CGDataProviderRelease(provider: CGDataProviderRef);
        pub fn CGImageCreateWithPNGDataProvider(
            source: CGDataProviderRef,
            decode: *const c_void,
            should_interpolate: bool,
            intent: i32,
        ) -> CGImageRef;
        pub fn CGImageCreateWithJPEGDataProvider(
            source: CGDataProviderRef,
            decode: *const c_void,
            should_interpolate: bool,
            intent: i32,
        ) -> CGImageRef;
        pub fn CGImageRelease(image: CGImageRef);
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        pub fn CFURLCreateWithFileSystemPath(
            allocator: *const c_void,
            file_path: CFStringRef,
            path_style: i32,
            is_directory: bool,
        ) -> CFURLRef;
        pub fn CFStringCreateWithCString(
            allocator: *const c_void,
            c_str: *const i8,
            encoding: u32,
        ) -> CFStringRef;
    }

    #[repr(C)]
    #[derive(Copy, Clone)]
    pub struct CGRect {
        pub origin: CGPoint,
        pub size: CGSize,
    }

    #[repr(C)]
    #[derive(Copy, Clone)]
    pub struct CGPoint {
        pub x: f64,
        pub y: f64,
    }

    #[repr(C)]
    #[derive(Copy, Clone)]
    pub struct CGSize {
        pub width: f64,
        pub height: f64,
    }

    const K_CF_STRING_ENCODING_UTF8: u32 = 0x08000100;
    const K_CG_BITMAP_BYTE_ORDER32_LITTLE: u32 = 2 << 12;
    const K_CG_IMAGE_ALPHA_PREMULTIPLIED_FIRST: u32 = 2;
    const K_CF_URL_POSIX_PATH_STYLE: i32 = 0;

    /// Load an image from a file path and create a Metal texture.
    pub fn load_image_as_metal_texture(
        device: &ProtocolObject<dyn MTLDevice>,
        path: &Path,
    ) -> Result<metal::Texture, String> {
        let path_str = path.to_str().ok_or("Invalid path")?;
        let c_path = std::ffi::CString::new(path_str).map_err(|_| "Invalid path string")?;

        unsafe {
            // Create CFURL from path
            let cf_path = CFStringCreateWithCString(
                std::ptr::null(),
                c_path.as_ptr(),
                K_CF_STRING_ENCODING_UTF8,
            );
            if cf_path.is_null() {
                return Err("Failed to create CFString".to_string());
            }

            let url = CFURLCreateWithFileSystemPath(
                std::ptr::null(),
                cf_path,
                K_CF_URL_POSIX_PATH_STYLE,
                false,
            );
            super::ffi::CFRelease(cf_path);
            if url.is_null() {
                return Err("Failed to create CFURL".to_string());
            }

            // Create data provider
            let provider = CGDataProviderCreateWithURL(url);
            super::ffi::CFRelease(url);
            if provider.is_null() {
                return Err("Failed to create data provider".to_string());
            }

            // Try PNG first, then JPEG
            let is_png = path_str.to_lowercase().ends_with(".png");
            let image = if is_png {
                CGImageCreateWithPNGDataProvider(provider, std::ptr::null(), true, 0)
            } else {
                CGImageCreateWithJPEGDataProvider(provider, std::ptr::null(), true, 0)
            };
            CGDataProviderRelease(provider);

            if image.is_null() {
                return Err("Failed to load image".to_string());
            }

            let width = CGImageGetWidth(image);
            let height = CGImageGetHeight(image);

            // Create BGRA buffer
            let bytes_per_row = width * 4;
            let mut pixel_data: Vec<u8> = vec![0; height * bytes_per_row];

            let color_space = CGColorSpaceCreateDeviceRGB();
            let bitmap_info =
                K_CG_BITMAP_BYTE_ORDER32_LITTLE | K_CG_IMAGE_ALPHA_PREMULTIPLIED_FIRST;

            let context = CGBitmapContextCreate(
                pixel_data.as_mut_ptr() as *mut c_void,
                width,
                height,
                8,
                bytes_per_row,
                color_space,
                bitmap_info,
            );
            CGColorSpaceRelease(color_space);

            if context.is_null() {
                CGImageRelease(image);
                return Err("Failed to create bitmap context".to_string());
            }

            // Draw image into context (flipped for Metal coordinate system)
            let rect = CGRect {
                origin: CGPoint { x: 0.0, y: 0.0 },
                size: CGSize {
                    width: width as f64,
                    height: height as f64,
                },
            };
            CGContextDrawImage(context, rect, image);
            CGContextRelease(context);
            CGImageRelease(image);

            // Create Metal texture
            let device_ref = {
                use metal::foreign_types::ForeignTypeRef;
                let ptr = device as *const _ as *mut c_void;
                metal::DeviceRef::from_ptr(ptr as *mut _)
            };

            let descriptor = metal::TextureDescriptor::new();
            descriptor.set_width(width as u64);
            descriptor.set_height(height as u64);
            descriptor.set_pixel_format(metal::MTLPixelFormat::BGRA8Unorm);
            descriptor.set_usage(metal::MTLTextureUsage::ShaderRead);

            let texture = device_ref.new_texture(&descriptor);

            let region = metal::MTLRegion {
                origin: metal::MTLOrigin { x: 0, y: 0, z: 0 },
                size: metal::MTLSize {
                    width: width as u64,
                    height: height as u64,
                    depth: 1,
                },
            };

            texture.replace_region(
                region,
                0,
                pixel_data.as_ptr() as *const c_void,
                bytes_per_row as u64,
            );

            Ok(texture)
        }
    }

    pub type CVReturn = i32;
    pub const K_CV_RETURN_SUCCESS: CVReturn = 0;

    #[link(name = "CoreVideo", kind = "framework")]
    extern "C" {
        pub fn CVPixelBufferCreateWithIOSurface(
            allocator: *const c_void,
            surface: *const IOSurface,
            pixelBufferAttributes: *const c_void,
            pixelBufferOut: *mut *mut CVPixelBuffer,
        ) -> CVReturn;

        pub fn CVPixelBufferGetIOSurface(pixelBuffer: *const CVPixelBuffer) -> *mut IOSurface;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        pub fn CFRelease(cf: *const c_void);
    }

    /// Create a Metal texture from an IOSurface.
    pub fn create_metal_texture_from_iosurface(
        device: &ProtocolObject<dyn MTLDevice>,
        iosurface: &IOSurface,
        plane: usize,
    ) -> Result<Retained<ProtocolObject<dyn MTLTexture>>, String> {
        let width = iosurface.width();
        let height = iosurface.height();

        // Get pixel format from IOSurface
        let pixel_format = iosurface.pixelFormat();
        let metal_format = match pixel_format {
            0x42475241 => MTLPixelFormat::BGRA8Unorm, // 'BGRA'
            0x4C303038 => MTLPixelFormat::R8Unorm,    // 'L008' - grayscale (Vision mask)
            _ => MTLPixelFormat::BGRA8Unorm,          // Default
        };

        let descriptor = MTLTextureDescriptor::new();
        unsafe {
            descriptor.setWidth(width as usize);
            descriptor.setHeight(height as usize);
            descriptor.setPixelFormat(metal_format);
            descriptor.setUsage(MTLTextureUsage::ShaderRead);
        }

        // Cast IOSurface to IOSurfaceRef pointer for msg_send
        let iosurface_ptr: *const IOSurfaceRef =
            iosurface as *const IOSurface as *const IOSurfaceRef;

        let texture: Option<Retained<ProtocolObject<dyn MTLTexture>>> = unsafe {
            msg_send![
                device,
                newTextureWithDescriptor: &*descriptor,
                iosurface: iosurface_ptr,
                plane: plane
            ]
        };

        texture.ok_or_else(|| {
            format!(
                "Failed to create texture from IOSurface ({}x{}, format=0x{:08X})",
                width, height, pixel_format
            )
        })
    }
}

#[repr(C)]
struct CompositorUniforms {
    time: f32,
    mask_threshold: f32,
    edge_feather: f32,
    _padding: f32, // Align to 16 bytes
}

#[repr(C)]
struct TemporalBlendParams {
    blend_factor: f32,
    _padding: [f32; 3], // Align to 16 bytes
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, streamlib::ConfigDescriptor)]
pub struct CyberpunkCompositorConfig {
    /// Quality level: 0=Fast, 1=Balanced, 2=Accurate.
    /// Accurate (2) enables Vision's built-in temporal smoothing for stable masks.
    pub quality_level: u8,
    pub mask_threshold: f32,
    /// Path to background image (PNG or JPEG). If None, uses procedural background.
    pub background_image_path: Option<PathBuf>,
    /// Temporal blend factor for mask smoothing (0.0-1.0).
    /// Higher values = more smoothing but more latency. Default 0.3.
    pub temporal_blend_factor: f32,
    /// Edge feather amount for smooth mask edges (0.0-0.5). Default 0.15.
    pub edge_feather: f32,
    /// Gaussian blur sigma for mask edges. Default 3.0.
    pub mask_blur_sigma: f32,
}

impl Default for CyberpunkCompositorConfig {
    fn default() -> Self {
        Self {
            // Use Balanced for good performance - our temporal smoothing handles stability
            quality_level: 1,
            mask_threshold: 0.5,
            background_image_path: None,
            // EMA blending: 70% current + 30% previous
            temporal_blend_factor: 0.3,
            // Wider edge feathering for smoother edges
            edge_feather: 0.15,
            // Gaussian blur sigma for mask smoothing
            mask_blur_sigma: 3.0,
        }
    }
}

#[streamlib::processor(
    name = "CyberpunkCompositor",
    execution = Reactive,
    description = "Person segmentation with cyberpunk background compositing",
    unsafe_send
)]
pub struct CyberpunkCompositorProcessor {
    #[streamlib::input(description = "Video frames to process")]
    video_in: LinkInput<VideoFrame>,

    #[streamlib::output(description = "Composited video frames")]
    video_out: LinkOutput<VideoFrame>,

    #[streamlib::config]
    config: CyberpunkCompositorConfig,

    gpu_context: Option<GpuContext>,
    metal_command_queue: Option<metal::CommandQueue>,
    render_pipeline: Option<metal::RenderPipelineState>,
    procedural_pipeline: Option<metal::RenderPipelineState>,
    sampler: Option<metal::SamplerState>,
    uniforms_buffer: Option<metal::Buffer>,
    background_texture: Option<metal::Texture>,
    /// Default white mask (all 1.0) used when Vision has never succeeded - shows full person.
    default_mask_texture: Option<metal::Texture>,
    /// Our own copy of the mask - Vision's IOSurface gets recycled, so we must copy the data.
    /// This texture is owned by us and persists between frames.
    owned_mask_texture: Option<metal::Texture>,
    /// Previous frame's mask for temporal EMA blending.
    previous_mask_texture: Option<metal::Texture>,
    /// Blurred mask texture (output of Gaussian blur pass).
    blurred_mask_texture: Option<metal::Texture>,
    /// Final smoothed mask (after temporal blend + blur).
    smoothed_mask_texture: Option<metal::Texture>,
    /// Dimensions of the owned mask texture (to detect when we need to resize).
    owned_mask_dimensions: Option<(u64, u64)>,
    /// Compute pipeline for temporal blending (EMA).
    temporal_blend_pipeline: Option<metal::ComputePipelineState>,
    /// Compute pipeline for Gaussian blur (horizontal pass).
    blur_h_pipeline: Option<metal::ComputePipelineState>,
    /// Compute pipeline for Gaussian blur (vertical pass).
    blur_v_pipeline: Option<metal::ComputePipelineState>,
    start_time: Option<Instant>,
    frame_count: AtomicU64,
    segmentation_request: Option<Retained<VNGeneratePersonSegmentationRequest>>,
}

impl streamlib::core::ReactiveProcessor for CyberpunkCompositorProcessor::Processor {
    fn setup(
        &mut self,
        ctx: RuntimeContext,
    ) -> impl std::future::Future<Output = Result<()>> + Send {
        let result = (|| {
            tracing::info!("CyberpunkCompositor: Setting up...");

            self.gpu_context = Some(ctx.gpu.clone());
            self.start_time = Some(Instant::now());

            let metal_device = ctx.gpu.metal_device();
            let metal_device_ref = {
                use metal::foreign_types::ForeignTypeRef;
                let device_ptr = metal_device.device() as *const _ as *mut c_void;
                unsafe { metal::DeviceRef::from_ptr(device_ptr as *mut _) }
            };

            let metal_command_queue = metal_device_ref.new_command_queue();

            // Compile shader
            let shader_source = include_str!("shaders/cyberpunk_compositor.metal");
            let library = metal_device_ref
                .new_library_with_source(shader_source, &metal::CompileOptions::new())
                .map_err(|e| StreamError::Configuration(format!("Shader compile failed: {}", e)))?;

            let vertex_function = library
                .get_function("compositor_vertex", None)
                .map_err(|e| {
                    StreamError::Configuration(format!("Vertex function not found: {}", e))
                })?;
            let fragment_function =
                library
                    .get_function("compositor_fragment", None)
                    .map_err(|e| {
                        StreamError::Configuration(format!("Fragment function not found: {}", e))
                    })?;
            let procedural_fragment = library
                .get_function("compositor_procedural_fragment", None)
                .map_err(|e| {
                    StreamError::Configuration(format!("Procedural function not found: {}", e))
                })?;

            // Create compositor pipeline (with background image)
            let pipeline_descriptor = metal::RenderPipelineDescriptor::new();
            pipeline_descriptor.set_vertex_function(Some(&vertex_function));
            pipeline_descriptor.set_fragment_function(Some(&fragment_function));
            // Output to RGBA to match output texture and downstream Python processors (Skia uses RGBA)
            pipeline_descriptor
                .color_attachments()
                .object_at(0)
                .unwrap()
                .set_pixel_format(metal::MTLPixelFormat::RGBA8Unorm);

            let render_pipeline = metal_device_ref
                .new_render_pipeline_state(&pipeline_descriptor)
                .map_err(|e| {
                    StreamError::Configuration(format!("Pipeline create failed: {}", e))
                })?;

            // Create procedural background pipeline (no image)
            pipeline_descriptor.set_fragment_function(Some(&procedural_fragment));
            let procedural_pipeline = metal_device_ref
                .new_render_pipeline_state(&pipeline_descriptor)
                .map_err(|e| {
                    StreamError::Configuration(format!("Procedural pipeline failed: {}", e))
                })?;

            // Create compute pipelines for mask smoothing
            let temporal_blend_fn =
                library
                    .get_function("temporal_blend_mask", None)
                    .map_err(|e| {
                        StreamError::Configuration(format!("temporal_blend_mask not found: {}", e))
                    })?;
            let temporal_blend_pipeline = metal_device_ref
                .new_compute_pipeline_state_with_function(&temporal_blend_fn)
                .map_err(|e| {
                    StreamError::Configuration(format!("Temporal blend pipeline failed: {}", e))
                })?;

            let blur_h_fn = library
                .get_function("gaussian_blur_horizontal", None)
                .map_err(|e| {
                    StreamError::Configuration(format!("gaussian_blur_horizontal not found: {}", e))
                })?;
            let blur_h_pipeline = metal_device_ref
                .new_compute_pipeline_state_with_function(&blur_h_fn)
                .map_err(|e| {
                    StreamError::Configuration(format!("Blur H pipeline failed: {}", e))
                })?;

            let blur_v_fn = library
                .get_function("gaussian_blur_vertical", None)
                .map_err(|e| {
                    StreamError::Configuration(format!("gaussian_blur_vertical not found: {}", e))
                })?;
            let blur_v_pipeline = metal_device_ref
                .new_compute_pipeline_state_with_function(&blur_v_fn)
                .map_err(|e| {
                    StreamError::Configuration(format!("Blur V pipeline failed: {}", e))
                })?;

            // Create default white mask texture (1x1 white pixel = show full person when Vision fails)
            let mask_desc = metal::TextureDescriptor::new();
            mask_desc.set_texture_type(metal::MTLTextureType::D2);
            mask_desc.set_pixel_format(metal::MTLPixelFormat::R8Unorm);
            mask_desc.set_width(1);
            mask_desc.set_height(1);
            mask_desc.set_usage(metal::MTLTextureUsage::ShaderRead);
            let default_mask = metal_device_ref.new_texture(&mask_desc);
            // Fill with white (1.0 = full person visibility)
            let white_pixel: [u8; 1] = [255];
            default_mask.replace_region(
                metal::MTLRegion {
                    origin: metal::MTLOrigin { x: 0, y: 0, z: 0 },
                    size: metal::MTLSize {
                        width: 1,
                        height: 1,
                        depth: 1,
                    },
                },
                0,
                white_pixel.as_ptr() as *const _,
                1,
            );

            // Sampler
            let sampler_descriptor = metal::SamplerDescriptor::new();
            sampler_descriptor.set_min_filter(metal::MTLSamplerMinMagFilter::Linear);
            sampler_descriptor.set_mag_filter(metal::MTLSamplerMinMagFilter::Linear);
            sampler_descriptor.set_address_mode_s(metal::MTLSamplerAddressMode::ClampToEdge);
            sampler_descriptor.set_address_mode_t(metal::MTLSamplerAddressMode::ClampToEdge);
            let sampler_state = metal_device_ref.new_sampler(&sampler_descriptor);

            // Uniforms buffer
            let uniforms_buffer = metal_device_ref.new_buffer(
                std::mem::size_of::<CompositorUniforms>() as u64,
                metal::MTLResourceOptions::CPUCacheModeDefaultCache,
            );

            // Vision segmentation request
            let segmentation_request = unsafe { VNGeneratePersonSegmentationRequest::new() };
            let quality_level = match self.config.quality_level {
                0 => VNGeneratePersonSegmentationRequestQualityLevel::Fast,
                1 => VNGeneratePersonSegmentationRequestQualityLevel::Balanced,
                _ => VNGeneratePersonSegmentationRequestQualityLevel::Accurate,
            };
            unsafe { segmentation_request.setQualityLevel(quality_level) };

            // Load background image if configured
            let background_texture = if let Some(ref path) = self.config.background_image_path {
                match ffi::load_image_as_metal_texture(metal_device.device(), path) {
                    Ok(texture) => {
                        tracing::info!(
                            "CyberpunkCompositor: Loaded background image {}x{} from {:?}",
                            texture.width(),
                            texture.height(),
                            path
                        );
                        Some(texture)
                    }
                    Err(e) => {
                        tracing::warn!(
                            "CyberpunkCompositor: Failed to load background image: {}",
                            e
                        );
                        None
                    }
                }
            } else {
                None
            };

            self.metal_command_queue = Some(metal_command_queue);
            self.render_pipeline = Some(render_pipeline);
            self.procedural_pipeline = Some(procedural_pipeline);
            self.temporal_blend_pipeline = Some(temporal_blend_pipeline);
            self.blur_h_pipeline = Some(blur_h_pipeline);
            self.blur_v_pipeline = Some(blur_v_pipeline);
            self.sampler = Some(sampler_state);
            self.uniforms_buffer = Some(uniforms_buffer);
            self.background_texture = background_texture;
            self.default_mask_texture = Some(default_mask);
            self.segmentation_request = Some(segmentation_request);

            tracing::info!("CyberpunkCompositor: Initialized");
            Ok(())
        })();
        std::future::ready(result)
    }

    fn teardown(&mut self) -> impl std::future::Future<Output = Result<()>> + Send {
        let count = self.frame_count.load(Ordering::Relaxed);
        tracing::info!("CyberpunkCompositor: Shutdown ({} frames)", count);
        std::future::ready(Ok(()))
    }

    fn process(&mut self) -> Result<()> {
        tracing::trace!("CyberpunkCompositor: process() called");
        let Some(frame) = self.video_in.read() else {
            tracing::trace!("CyberpunkCompositor: no frame available");
            return Ok(());
        };
        // Log input IOSurface ID for texture flow debugging
        let input_iosurface_id = frame.texture.iosurface_id();
        tracing::debug!(
            "CyberpunkCompositor: INPUT frame {}x{}, IOSurface ID={:?}",
            frame.width,
            frame.height,
            input_iosurface_id
        );

        let width = frame.width;
        let height = frame.height;
        let elapsed = self
            .start_time
            .map(|t| t.elapsed().as_secs_f32())
            .unwrap_or(0.0);
        let input_metal = frame.metal_texture();

        let gpu_ctx = self
            .gpu_context
            .as_ref()
            .ok_or_else(|| StreamError::Configuration("GPU context not initialized".into()))?;

        let command_queue = self.metal_command_queue.as_ref().unwrap();

        // Vision/CVPixelBuffer requires BGRA format (0x42475241), but the camera's pool
        // texture is labeled as RGBA (0x52474241). The actual bytes are in BGRA order
        // (from AVFoundation), but the IOSurface format code is wrong.
        //
        // Solution: Acquire a BGRA IOSurface from the pool, blit to it, then use for Vision.
        // The blit copies bytes unchanged, so BGRA bytes → BGRA-labeled IOSurface = correct.
        let bgra_pool_desc = TexturePoolDescriptor::new(width, height, TextureFormat::Bgra8Unorm);
        let bgra_handle = gpu_ctx.acquire_texture(&bgra_pool_desc)?;
        let bgra_metal = bgra_handle.metal_texture();

        // Blit input → BGRA texture (copies bytes, which are already BGRA-ordered)
        let blit_buffer = command_queue.new_command_buffer();
        let blit_encoder = blit_buffer.new_blit_command_encoder();
        let origin = metal::MTLOrigin { x: 0, y: 0, z: 0 };
        let size = metal::MTLSize {
            width: width as u64,
            height: height as u64,
            depth: 1,
        };
        blit_encoder.copy_from_texture(input_metal, 0, 0, origin, size, bgra_metal, 0, 0, origin);
        blit_encoder.end_encoding();
        blit_buffer.commit();
        blit_buffer.wait_until_completed();

        // Now get the IOSurface from the BGRA texture for Vision
        if let Some(bgra_iosurface) = bgra_handle.texture().as_iosurface() {
            // Log the actual pixel format of the IOSurface for debugging
            let pixel_format = bgra_iosurface.pixelFormat();
            let fourcc_bytes = pixel_format.to_be_bytes();
            let fourcc_str: String = fourcc_bytes
                .iter()
                .map(|&b| if b.is_ascii_graphic() { b as char } else { '?' })
                .collect();
            tracing::trace!(
                "BGRA IOSurface for Vision: {}x{}, format=0x{:08X} ('{}')",
                bgra_iosurface.width(),
                bgra_iosurface.height(),
                pixel_format,
                fourcc_str
            );

            // Run Vision segmentation - copy mask data to our owned texture
            // (Vision recycles its IOSurface buffers, so we can't just cache a reference)
            if let Some(vision_mask) = self.run_segmentation(bgra_iosurface, gpu_ctx)? {
                // Get Vision mask dimensions
                let mask_width: u64 = unsafe {
                    use objc2::msg_send;
                    msg_send![&*vision_mask, width]
                };
                let mask_height: u64 = unsafe {
                    use objc2::msg_send;
                    msg_send![&*vision_mask, height]
                };

                tracing::debug!(
                    "Vision: mask received {}x{}, copying to owned texture",
                    mask_width,
                    mask_height
                );

                // Create or resize our owned mask texture if needed
                let needs_create = match self.owned_mask_dimensions {
                    Some((w, h)) => w != mask_width || h != mask_height,
                    None => true,
                };

                if needs_create {
                    let metal_device = gpu_ctx.metal_device();
                    let metal_device_ref = {
                        use metal::foreign_types::ForeignTypeRef;
                        let device_ptr = metal_device.device() as *const _ as *mut c_void;
                        unsafe { metal::DeviceRef::from_ptr(device_ptr as *mut _) }
                    };

                    let mask_desc = metal::TextureDescriptor::new();
                    mask_desc.set_texture_type(metal::MTLTextureType::D2);
                    mask_desc.set_pixel_format(metal::MTLPixelFormat::R8Unorm);
                    mask_desc.set_width(mask_width);
                    mask_desc.set_height(mask_height);
                    mask_desc.set_usage(
                        metal::MTLTextureUsage::ShaderRead | metal::MTLTextureUsage::ShaderWrite,
                    );

                    self.owned_mask_texture = Some(metal_device_ref.new_texture(&mask_desc));
                    self.owned_mask_dimensions = Some((mask_width, mask_height));
                    tracing::info!("Created owned mask texture: {}x{}", mask_width, mask_height);
                }

                // Blit Vision's mask to our owned texture
                if let Some(ref owned_mask) = self.owned_mask_texture {
                    let blit_buffer = command_queue.new_command_buffer();
                    let blit_encoder = blit_buffer.new_blit_command_encoder();

                    // Convert vision_mask (ProtocolObject<dyn MTLTexture>) to metal::TextureRef
                    let vision_mask_ref = {
                        use metal::foreign_types::ForeignTypeRef;
                        let ptr =
                            &*vision_mask as *const ProtocolObject<dyn MTLTexture> as *mut c_void;
                        unsafe { metal::TextureRef::from_ptr(ptr as *mut _) }
                    };

                    let origin = metal::MTLOrigin { x: 0, y: 0, z: 0 };
                    let size = metal::MTLSize {
                        width: mask_width,
                        height: mask_height,
                        depth: 1,
                    };
                    blit_encoder.copy_from_texture(
                        vision_mask_ref,
                        0,
                        0,
                        origin,
                        size,
                        owned_mask,
                        0,
                        0,
                        origin,
                    );
                    blit_encoder.end_encoding();
                    blit_buffer.commit();
                    blit_buffer.wait_until_completed();

                    tracing::trace!("Copied Vision mask to owned texture");
                }
            } else {
                tracing::trace!(
                    "Vision: no mask this frame, using previous owned mask (have={})",
                    self.owned_mask_texture.is_some()
                );
            }
        } else {
            tracing::warn!("BGRA texture has no IOSurface backing");
        }

        // ==========================================================================
        // Apply temporal smoothing and Gaussian blur to the mask
        // ==========================================================================
        if let Some(ref owned_mask) = self.owned_mask_texture {
            let (mask_width, mask_height) = self.owned_mask_dimensions.unwrap_or((1, 1));
            let metal_device = gpu_ctx.metal_device();
            let metal_device_ref = {
                use metal::foreign_types::ForeignTypeRef;
                let device_ptr = metal_device.device() as *const _ as *mut c_void;
                unsafe { metal::DeviceRef::from_ptr(device_ptr as *mut _) }
            };

            // Create additional mask textures if needed
            let needs_create_smoothing_textures = self.smoothed_mask_texture.is_none()
                || self.previous_mask_texture.is_none()
                || self.blurred_mask_texture.is_none();

            if needs_create_smoothing_textures {
                let mask_desc = metal::TextureDescriptor::new();
                mask_desc.set_texture_type(metal::MTLTextureType::D2);
                mask_desc.set_pixel_format(metal::MTLPixelFormat::R8Unorm);
                mask_desc.set_width(mask_width);
                mask_desc.set_height(mask_height);
                mask_desc.set_usage(
                    metal::MTLTextureUsage::ShaderRead | metal::MTLTextureUsage::ShaderWrite,
                );

                if self.previous_mask_texture.is_none() {
                    self.previous_mask_texture = Some(metal_device_ref.new_texture(&mask_desc));
                    // Initialize previous mask by copying current
                    let init_buffer = command_queue.new_command_buffer();
                    let init_encoder = init_buffer.new_blit_command_encoder();
                    let origin = metal::MTLOrigin { x: 0, y: 0, z: 0 };
                    let size = metal::MTLSize {
                        width: mask_width,
                        height: mask_height,
                        depth: 1,
                    };
                    init_encoder.copy_from_texture(
                        owned_mask,
                        0,
                        0,
                        origin,
                        size,
                        self.previous_mask_texture.as_ref().unwrap(),
                        0,
                        0,
                        origin,
                    );
                    init_encoder.end_encoding();
                    init_buffer.commit();
                    init_buffer.wait_until_completed();
                }
                if self.blurred_mask_texture.is_none() {
                    self.blurred_mask_texture = Some(metal_device_ref.new_texture(&mask_desc));
                }
                if self.smoothed_mask_texture.is_none() {
                    self.smoothed_mask_texture = Some(metal_device_ref.new_texture(&mask_desc));
                }
                tracing::info!("Created smoothing textures: {}x{}", mask_width, mask_height);
            }

            let previous_mask = self.previous_mask_texture.as_ref().unwrap();
            let blurred_mask = self.blurred_mask_texture.as_ref().unwrap();
            let smoothed_mask = self.smoothed_mask_texture.as_ref().unwrap();

            // Step 1: Temporal blend (EMA) - blend current with previous
            if let Some(ref temporal_pipeline) = self.temporal_blend_pipeline {
                let blend_buffer = command_queue.new_command_buffer();
                let blend_encoder = blend_buffer.new_compute_command_encoder();

                // Create params buffer
                let params = TemporalBlendParams {
                    blend_factor: self.config.temporal_blend_factor,
                    _padding: [0.0, 0.0, 0.0],
                };
                let params_buffer = metal_device_ref.new_buffer_with_data(
                    &params as *const _ as *const c_void,
                    std::mem::size_of::<TemporalBlendParams>() as u64,
                    metal::MTLResourceOptions::CPUCacheModeDefaultCache,
                );

                blend_encoder.set_compute_pipeline_state(temporal_pipeline);
                blend_encoder.set_texture(0, Some(owned_mask));
                blend_encoder.set_texture(1, Some(previous_mask));
                blend_encoder.set_texture(2, Some(smoothed_mask));
                blend_encoder.set_buffer(0, Some(&params_buffer), 0);

                let thread_group_size = metal::MTLSize {
                    width: 16,
                    height: 16,
                    depth: 1,
                };
                let thread_groups = metal::MTLSize {
                    width: mask_width.div_ceil(16),
                    height: mask_height.div_ceil(16),
                    depth: 1,
                };
                blend_encoder.dispatch_thread_groups(thread_groups, thread_group_size);
                blend_encoder.end_encoding();
                blend_buffer.commit();
                blend_buffer.wait_until_completed();
            }

            // Step 2: Gaussian blur horizontal pass (smoothed → blurred)
            if let Some(ref blur_h_pipeline) = self.blur_h_pipeline {
                let blur_buffer = command_queue.new_command_buffer();
                let blur_encoder = blur_buffer.new_compute_command_encoder();

                blur_encoder.set_compute_pipeline_state(blur_h_pipeline);
                blur_encoder.set_texture(0, Some(smoothed_mask));
                blur_encoder.set_texture(1, Some(blurred_mask));

                let thread_group_size = metal::MTLSize {
                    width: 16,
                    height: 16,
                    depth: 1,
                };
                let thread_groups = metal::MTLSize {
                    width: mask_width.div_ceil(16),
                    height: mask_height.div_ceil(16),
                    depth: 1,
                };
                blur_encoder.dispatch_thread_groups(thread_groups, thread_group_size);
                blur_encoder.end_encoding();
                blur_buffer.commit();
                blur_buffer.wait_until_completed();
            }

            // Step 3: Gaussian blur vertical pass (blurred → smoothed)
            if let Some(ref blur_v_pipeline) = self.blur_v_pipeline {
                let blur_buffer = command_queue.new_command_buffer();
                let blur_encoder = blur_buffer.new_compute_command_encoder();

                blur_encoder.set_compute_pipeline_state(blur_v_pipeline);
                blur_encoder.set_texture(0, Some(blurred_mask));
                blur_encoder.set_texture(1, Some(smoothed_mask));

                let thread_group_size = metal::MTLSize {
                    width: 16,
                    height: 16,
                    depth: 1,
                };
                let thread_groups = metal::MTLSize {
                    width: mask_width.div_ceil(16),
                    height: mask_height.div_ceil(16),
                    depth: 1,
                };
                blur_encoder.dispatch_thread_groups(thread_groups, thread_group_size);
                blur_encoder.end_encoding();
                blur_buffer.commit();
                blur_buffer.wait_until_completed();
            }

            // Step 4: Copy current smoothed mask to previous for next frame
            let copy_buffer = command_queue.new_command_buffer();
            let copy_encoder = copy_buffer.new_blit_command_encoder();
            let origin = metal::MTLOrigin { x: 0, y: 0, z: 0 };
            let size = metal::MTLSize {
                width: mask_width,
                height: mask_height,
                depth: 1,
            };
            copy_encoder.copy_from_texture(
                smoothed_mask,
                0,
                0,
                origin,
                size,
                previous_mask,
                0,
                0,
                origin,
            );
            copy_encoder.end_encoding();
            copy_buffer.commit();
            copy_buffer.wait_until_completed();

            tracing::trace!("Applied temporal blend and Gaussian blur to mask");
        }

        // Acquire output texture (RGBA to match downstream Python processors using Skia RGBA)
        let pool_desc = TexturePoolDescriptor::new(width, height, TextureFormat::Rgba8Unorm);
        let output_handle = gpu_ctx.acquire_texture(&pool_desc)?;
        let output_metal = output_handle.metal_texture();

        // Render
        let sampler = self.sampler.as_ref().unwrap();
        let uniforms_buffer = self.uniforms_buffer.as_ref().unwrap();

        // Update uniforms
        unsafe {
            let ptr = uniforms_buffer.contents() as *mut CompositorUniforms;
            (*ptr).time = elapsed;
            (*ptr).mask_threshold = self.config.mask_threshold;
            (*ptr).edge_feather = self.config.edge_feather;
            (*ptr)._padding = 0.0;
        }

        let command_buffer = command_queue.new_command_buffer();
        let render_pass_descriptor = metal::RenderPassDescriptor::new();
        let color_attachment = render_pass_descriptor
            .color_attachments()
            .object_at(0)
            .unwrap();
        color_attachment.set_texture(Some(output_metal));
        color_attachment.set_load_action(metal::MTLLoadAction::Clear);
        color_attachment.set_clear_color(metal::MTLClearColor::new(0.0, 0.0, 0.0, 1.0));
        color_attachment.set_store_action(metal::MTLStoreAction::Store);

        let render_encoder = command_buffer.new_render_command_encoder(render_pass_descriptor);

        // Get mask texture - use smoothed mask (after temporal blend + blur) if available,
        // otherwise fall back to owned_mask, otherwise default white mask.
        // (white = 1.0 = show full person, no background replacement)
        let (mask_metal_ref, mask_source): (&metal::TextureRef, &str) =
            if let Some(ref smoothed_mask) = self.smoothed_mask_texture {
                // Use smoothed mask (temporal blend + Gaussian blur applied)
                (smoothed_mask.as_ref(), "smoothed")
            } else if let Some(ref owned_mask) = self.owned_mask_texture {
                // Use raw owned mask (no smoothing applied yet)
                (owned_mask.as_ref(), "owned_copy")
            } else {
                // No Vision mask ever succeeded - use default white mask (shows full video)
                (self.default_mask_texture.as_ref().unwrap(), "default_white")
            };
        tracing::trace!("Render: using mask source={}", mask_source);

        // Always use compositor shader (consistent color handling)
        // Choose pipeline based on whether we have a background image
        if let Some(ref bg_texture) = self.background_texture {
            // Use image-based compositor (video + mask + background image)
            render_encoder.set_render_pipeline_state(self.render_pipeline.as_ref().unwrap());
            render_encoder.set_fragment_texture(0, Some(input_metal));
            render_encoder.set_fragment_texture(1, Some(mask_metal_ref));
            render_encoder.set_fragment_texture(2, Some(bg_texture));
        } else {
            // Use procedural background compositor (video + mask + procedural)
            render_encoder.set_render_pipeline_state(self.procedural_pipeline.as_ref().unwrap());
            render_encoder.set_fragment_texture(0, Some(input_metal));
            render_encoder.set_fragment_texture(1, Some(mask_metal_ref));
        }

        render_encoder.set_fragment_sampler_state(0, Some(sampler));
        render_encoder.set_fragment_buffer(0, Some(uniforms_buffer), 0);
        render_encoder.draw_primitives(metal::MTLPrimitiveType::Triangle, 0, 3);
        render_encoder.end_encoding();

        command_buffer.commit();
        command_buffer.wait_until_completed();

        // Output - log IOSurface ID to verify texture flow
        let output_iosurface_id = output_handle.iosurface_id();
        tracing::debug!(
            "CyberpunkCompositor: OUTPUT IOSurface ID={:?} (input was {:?})",
            output_iosurface_id,
            input_iosurface_id
        );

        let output_frame = frame.with_pooled_texture(output_handle);
        self.video_out.write(output_frame);

        let count = self.frame_count.fetch_add(1, Ordering::Relaxed) + 1;
        if count == 1 || count.is_multiple_of(60) {
            let has_mask = self.owned_mask_texture.is_some();
            tracing::debug!(
                "CyberpunkCompositor: {} frames (owned_mask={})",
                count,
                has_mask
            );
        }

        Ok(())
    }
}

impl CyberpunkCompositorProcessor::Processor {
    fn run_segmentation(
        &self,
        iosurface: &IOSurface,
        gpu_ctx: &GpuContext,
    ) -> Result<Option<Retained<ProtocolObject<dyn MTLTexture>>>> {
        // Log IOSurface info for debugging
        let surface_width = iosurface.width();
        let surface_height = iosurface.height();
        let pixel_format = iosurface.pixelFormat();

        // Format FourCC as string for debugging
        let fourcc_bytes = pixel_format.to_be_bytes();
        let fourcc_str: String = fourcc_bytes
            .iter()
            .map(|&b| if b.is_ascii_graphic() { b as char } else { '?' })
            .collect();
        tracing::trace!(
            "IOSurface: {}x{}, pixel_format=0x{:08X} ('{}')",
            surface_width,
            surface_height,
            pixel_format,
            fourcc_str
        );

        // Create CVPixelBuffer from IOSurface
        // Note: CVPixelBuffer requires specific pixel formats. Common ones:
        // - 'BGRA' (0x42475241) - 32-bit BGRA
        // - '420v' (0x34323076) - Bi-Planar YCbCr 4:2:0 video range
        // - '420f' (0x34323066) - Bi-Planar YCbCr 4:2:0 full range
        let mut pixel_buffer: *mut CVPixelBuffer = ptr::null_mut();
        let cv_result = unsafe {
            ffi::CVPixelBufferCreateWithIOSurface(
                ptr::null(),
                iosurface as *const IOSurface,
                ptr::null(),
                &mut pixel_buffer,
            )
        };

        if cv_result != ffi::K_CV_RETURN_SUCCESS || pixel_buffer.is_null() {
            // -6661 = kCVReturnInvalidPixelFormat - IOSurface format not recognized by CoreVideo
            tracing::warn!(
                "CVPixelBuffer creation failed: cv_result={} (IOSurface: {}x{}, format='{}'/0x{:08X}). \
                 Need BGRA/420v/420f format for Vision.",
                cv_result, surface_width, surface_height, fourcc_str, pixel_format
            );
            return Ok(None);
        }
        tracing::trace!("CVPixelBuffer created successfully");

        let segmentation_request = self.segmentation_request.as_ref().unwrap();

        // Create image handler
        let empty_dict: Retained<NSDictionary<VNImageOption, objc2::runtime::AnyObject>> =
            NSDictionary::new();

        let image_handler = unsafe {
            let pixel_buffer_ref = &*pixel_buffer;
            VNImageRequestHandler::initWithCVPixelBuffer_options(
                VNImageRequestHandler::alloc(),
                pixel_buffer_ref,
                &empty_dict,
            )
        };

        // Perform request
        let requests: Retained<NSArray<VNRequest>> = {
            let request_ref: &VNRequest = segmentation_request;
            NSArray::from_slice(&[request_ref])
        };

        let perform_result = image_handler.performRequests_error(&requests);

        // Release pixel buffer
        unsafe { ffi::CFRelease(pixel_buffer as *const c_void) };

        if let Err(ref e) = perform_result {
            tracing::warn!("Vision performRequests failed: {:?}", e);
            return Ok(None);
        }
        tracing::trace!("Vision request performed successfully");

        // Get mask texture from results
        let mask_texture = unsafe {
            let results = segmentation_request.results();
            if let Some(observations) = results {
                tracing::trace!("Vision returned {} observations", observations.count());
                if observations.count() > 0 {
                    let observation: Retained<VNPixelBufferObservation> =
                        observations.objectAtIndex(0);
                    let mask_pixel_buffer = observation.pixelBuffer();

                    let mask_iosurface_ptr = ffi::CVPixelBufferGetIOSurface(&*mask_pixel_buffer);
                    if !mask_iosurface_ptr.is_null() {
                        let mask_iosurface = &*mask_iosurface_ptr;
                        tracing::trace!(
                            "Mask IOSurface: {}x{}",
                            mask_iosurface.width(),
                            mask_iosurface.height()
                        );
                        match ffi::create_metal_texture_from_iosurface(
                            gpu_ctx.metal_device().device(),
                            mask_iosurface,
                            0,
                        ) {
                            Ok(tex) => {
                                tracing::trace!("Created mask Metal texture");
                                Some(tex)
                            }
                            Err(e) => {
                                tracing::warn!("Failed to create mask texture: {}", e);
                                None
                            }
                        }
                    } else {
                        tracing::warn!("Mask pixel buffer has no IOSurface");
                        None
                    }
                } else {
                    tracing::trace!("No observations in results");
                    None
                }
            } else {
                tracing::trace!("Vision results() returned None");
                None
            }
        };

        Ok(mask_texture)
    }
}
