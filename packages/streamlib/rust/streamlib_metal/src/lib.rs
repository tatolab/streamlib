use pyo3::prelude::*;
use core_foundation::base::CFTypeRef;
use objc::{msg_send, sel, sel_impl};
use objc::runtime::{Class, Object};
use std::ptr;
use foreign_types::{ForeignType, ForeignTypeRef};

// Unified Metal HAL bridge for streamlib
// Provides both camera capture (IOSurface → Metal → wgpu) and display (wgpu → Metal)

// ============================================================================
// CAMERA CAPTURE: IOSurface → Metal → wgpu HAL
// ============================================================================

/// Zero-copy Metal texture wrapper for Python
#[pyclass]
pub struct MetalTextureWrapper {
    raw_texture: *mut Object,  // Metal texture object
    width: u32,
    height: u32,
}

unsafe impl Send for MetalTextureWrapper {}

#[pymethods]
impl MetalTextureWrapper {
    #[new]
    fn new() -> Self {
        Self {
            raw_texture: ptr::null_mut(),
            width: 0,
            height: 0,
        }
    }

    /// Create a wgpu HAL texture from this Metal texture
    /// Returns a pointer to the HAL texture for zero-copy usage
    fn create_hal_texture(&self) -> PyResult<u64> {
        if self.raw_texture.is_null() {
            return Err(PyErr::new::<pyo3::exceptions::PyValueError, _>(
                "Metal texture is null"
            ));
        }

        // Wrap the raw Metal texture pointer in metal::Texture
        let metal_texture = unsafe {
            metal::Texture::from_ptr(self.raw_texture as *mut _)
        };

        // Get texture properties
        let width = metal_texture.width();
        let height = metal_texture.height();
        let depth = metal_texture.depth();
        let array_layers = metal_texture.array_length();
        let mip_levels = metal_texture.mipmap_level_count();
        let raw_type = metal_texture.texture_type();

        // Determine format - assuming BGRA8Unorm for IOSurface textures
        let format = wgpu_types::TextureFormat::Bgra8Unorm;

        // Create the HAL texture using our public API from forked wgpu
        let hal_texture = unsafe {
            wgpu_hal::metal::Texture::from_raw(
                metal_texture,
                format,
                raw_type,
                array_layers as u32,
                mip_levels as u32,
                wgpu_hal::CopyExtent {
                    width: width as u32,
                    height: height as u32,
                    depth: depth as u32,
                }
            )
        };

        // Box and leak to get a stable pointer to pass to Python
        let boxed = Box::new(hal_texture);
        let ptr = Box::into_raw(boxed) as u64;

        Ok(ptr)
    }

    /// Get dimensions
    fn get_dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Get raw Metal texture pointer
    fn get_raw_texture_ptr(&self) -> u64 {
        self.raw_texture as u64
    }
}

/// Create a Metal texture from an IOSurface (zero-copy)
#[pyfunction]
fn create_metal_texture_from_iosurface(
    iosurface_ptr: u64,
    device_ptr: u64,
    width: u32,
    height: u32,
) -> PyResult<MetalTextureWrapper> {
    unsafe {
        if iosurface_ptr == 0 || device_ptr == 0 {
            return Err(PyErr::new::<pyo3::exceptions::PyValueError, _>(
                "Invalid pointers"
            ));
        }

        let iosurface = iosurface_ptr as *mut Object;
        let device = device_ptr as *mut Object;

        // Create a Metal texture descriptor
        let texture_descriptor_class = Class::get("MTLTextureDescriptor").unwrap();
        let texture_descriptor: *mut Object = msg_send![texture_descriptor_class, texture2DDescriptorWithPixelFormat: 80usize  // MTLPixelFormatBGRA8Unorm = 80
                                                                      width: width as usize
                                                                      height: height as usize
                                                                      mipmapped: false];

        // Set usage flags (shader read/write)
        let _: () = msg_send![texture_descriptor, setUsage: 0x0001 | 0x0002]; // MTLTextureUsageShaderRead | MTLTextureUsageShaderWrite

        // Create texture from IOSurface
        let metal_texture: *mut Object = msg_send![device, newTextureWithDescriptor: texture_descriptor
                                                          iosurface: iosurface
                                                          plane: 0usize];

        if metal_texture.is_null() {
            return Err(PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(
                "Failed to create Metal texture from IOSurface"
            ));
        }

        // Retain the texture
        let _: () = msg_send![metal_texture, retain];

        Ok(MetalTextureWrapper {
            raw_texture: metal_texture,
            width,
            height,
        })
    }
}

/// Read pixel data from IOSurface to CPU buffer (for testing/verification)
///
/// This function reads the pixel data from an IOSurface and returns it as bytes.
/// Use this to verify camera capture is working by saving frames to disk.
///
/// # Arguments
/// * `iosurface_ptr` - Pointer to IOSurface
///
/// # Returns
/// * `(bytes, width, height, bytes_per_row)` - Pixel data and metadata
///
/// # Safety
/// This function locks the IOSurface for reading.
#[pyfunction]
fn read_iosurface_pixels(py: Python, iosurface_ptr: u64) -> PyResult<(Py<pyo3::types::PyBytes>, u32, u32, usize)> {
    unsafe {
        let iosurface = iosurface_ptr as CFTypeRef;
        let iosurface_obj = iosurface as *mut objc::runtime::Object;

        // Get dimensions
        let width: usize = msg_send![iosurface_obj, width];
        let height: usize = msg_send![iosurface_obj, height];
        let bytes_per_row: usize = msg_send![iosurface_obj, bytesPerRow];

        // Lock IOSurface for reading (read-only, no options)
        let _lock_result: i32 = msg_send![iosurface_obj, lockWithOptions: 1u32 seed: std::ptr::null_mut::<u32>()];

        // Get base address of pixel data
        let base_address: *const u8 = msg_send![iosurface_obj, baseAddress];

        if base_address.is_null() {
            // Unlock before returning error
            let _: i32 = msg_send![iosurface_obj, unlockWithOptions: 1u32 seed: std::ptr::null_mut::<u32>()];
            return Err(pyo3::exceptions::PyRuntimeError::new_err(
                "Failed to get IOSurface base address"
            ));
        }

        // Copy pixel data (BGRA format, 4 bytes per pixel)
        let data_size = bytes_per_row * height;
        let pixel_data = std::slice::from_raw_parts(base_address, data_size);

        // Convert to PyBytes (copies the data)
        let py_bytes = pyo3::types::PyBytes::new_bound(py, pixel_data).unbind();

        // Unlock IOSurface
        let _: i32 = msg_send![iosurface_obj, unlockWithOptions: 1u32 seed: std::ptr::null_mut::<u32>()];

        Ok((py_bytes, width as u32, height as u32, bytes_per_row))
    }
}

// ============================================================================
// DISPLAY OUTPUT: wgpu → Metal extraction
// ============================================================================

/// Extract the underlying Metal texture from a wgpu texture (zero-copy)
///
/// This enables zero-copy display by getting direct access to the Metal texture
/// that backs a wgpu texture, which can then be rendered to CAMetalLayer.
///
/// # Arguments
/// * `device_ptr` - Pointer to wgpu device internal
/// * `texture_ptr` - Pointer to wgpu texture internal
///
/// # Returns
/// * `(metal_texture_ptr, width, height, format)` - Metal texture info
///
/// # Note
/// This requires the forked wgpu with public HAL access
#[pyfunction]
fn extract_metal_texture_from_wgpu(
    _py: Python<'_>,
    device_ptr: usize,
    texture_ptr: usize,
) -> PyResult<(usize, u32, u32, String)> {
    // Validate pointers
    if device_ptr == 0 || texture_ptr == 0 {
        return Err(PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(
            "Invalid device or texture pointer"
        ));
    }

    // SAFETY: We're casting pointers from Python to wgpu types
    // The Python wgpu library gives us id(texture._internal) which is a pointer
    // to the wgpu Texture object
    unsafe {
        // Cast to wgpu pointers
        let device = device_ptr as *const wgpu::Device;
        let texture = texture_ptr as *const wgpu::Texture;

        if device.is_null() || texture.is_null() {
            return Err(PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(
                "Device or texture pointer is null"
            ));
        }

        // Get references
        let device_ref = &*device;
        let texture_ref = &*texture;

        // Get texture dimensions from wgpu texture
        let size = texture_ref.size();
        let width = size.width;
        let height = size.height;
        let format = texture_ref.format();

        // Use as_hal to access the HAL texture
        // Note: This requires the forked wgpu with public HAL access
        let hal_texture_deref = texture_ref.as_hal::<wgpu_hal::metal::Api>()
            .ok_or_else(|| {
                PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(
                    "Texture is not Metal-backed or HAL access failed"
                )
            })?;

        // Get the raw Metal texture using the public raw_handle() method
        let metal_texture = hal_texture_deref.raw_handle();
        let texture_ptr = metal_texture.as_ptr() as usize;

        // Retain the Metal texture to prevent deallocation
        let metal_texture_ptr = texture_ptr as *mut Object;
        let _: () = msg_send![metal_texture_ptr, retain];

        // Format as string
        let format_str = format!("{:?}", format);

        Ok((texture_ptr, width, height, format_str))
    }
}

/// Get Metal device from wgpu device (zero-copy)
///
/// Extracts the underlying Metal device from a wgpu device.
/// This uses wgpu's HAL API to get direct access to the Metal backend.
///
/// # Arguments
/// * `device_ptr` - Pointer to wgpu device internal (_internal attribute from Python)
///
/// # Returns
/// * Metal device pointer (can be wrapped with PyObjC as MTLDevice)
///
/// # Safety
/// This function assumes the device_ptr is valid and points to a wgpu Device
/// backed by Metal. It will fail on non-Metal backends.
#[pyfunction]
fn extract_metal_device_from_wgpu(
    _py: Python<'_>,
    device_ptr: usize,
) -> PyResult<usize> {
    if device_ptr == 0 {
        return Err(PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(
            "Invalid device pointer"
        ));
    }

    // SAFETY: We're casting the pointer from Python to wgpu Device
    // The Python wgpu library gives us id(device._internal) which is a pointer
    // to the wgpu Device object
    unsafe {
        // Cast to wgpu Device pointer
        let device = device_ptr as *const wgpu::Device;

        if device.is_null() {
            return Err(PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(
                "Device pointer is null"
            ));
        }

        // Get reference to the device
        let device_ref = &*device;

        // Use as_hal to access the HAL device
        // Note: This requires the forked wgpu with public HAL access
        let hal_device = device_ref.as_hal::<wgpu_hal::metal::Api>()
            .ok_or_else(|| {
                PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(
                    "Device is not Metal-backed or HAL access failed"
                )
            })?;

        // Get the raw Metal device pointer
        let metal_device = hal_device.raw_device();
        let device_ptr = metal_device.lock().as_ptr() as usize;

        // Retain the Metal device to prevent deallocation
        let metal_device_ptr = device_ptr as *mut Object;
        let _: () = msg_send![metal_device_ptr, retain];

        Ok(device_ptr)
    }
}

/// Create a wgpu texture from a Metal texture (zero-copy)
///
/// Wraps an external Metal texture (e.g., from CAMetalLayer drawable)
/// as a wgpu texture for rendering. This enables zero-copy display by allowing
/// wgpu to render directly to CAMetalLayer drawables.
///
/// # Arguments
/// * `device_ptr` - Pointer to wgpu device internal
/// * `metal_texture_ptr` - Pointer to Metal texture (from drawable.texture())
/// * `width` - Texture width in pixels
/// * `height` - Texture height in pixels
/// * `format` - Texture format string ("bgra8unorm", "rgba8unorm", etc.)
///
/// # Returns
/// * Pointer to wgpu texture that wraps the Metal texture
///
/// # Usage
/// ```python
/// drawable = layer.nextDrawable()
/// metal_tex_ptr = get_metal_texture_ptr(drawable.texture())
/// wgpu_tex_ptr = create_wgpu_texture_from_metal(
///     device_ptr, metal_tex_ptr, 1920, 1080, "bgra8unorm"
/// )
/// # Now render to wgpu_tex_ptr
/// drawable.present()
/// ```
///
/// # Safety
/// The Metal texture must remain valid for the lifetime of the wgpu texture.
/// Typically this means keeping the drawable alive until rendering is complete.
#[pyfunction]
fn create_wgpu_texture_from_metal(
    _py: Python<'_>,
    device_ptr: usize,
    metal_texture_ptr: usize,
    width: u32,
    height: u32,
    format: &str,
) -> PyResult<usize> {
    // Validate inputs
    if device_ptr == 0 || metal_texture_ptr == 0 {
        return Err(PyErr::new::<pyo3::exceptions::PyValueError, _>(
            "Invalid device or texture pointer"
        ));
    }

    if width == 0 || height == 0 {
        return Err(PyErr::new::<pyo3::exceptions::PyValueError, _>(
            "Invalid texture dimensions"
        ));
    }

    unsafe {
        // Cast pointers
        let device = device_ptr as *const wgpu::Device;
        let metal_texture_obj = metal_texture_ptr as *mut Object;

        if device.is_null() || metal_texture_obj.is_null() {
            return Err(PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(
                "Device or texture pointer is null"
            ));
        }

        let device_ref = &*device;

        // Wrap the Metal texture pointer
        let metal_texture = metal::Texture::from_ptr(metal_texture_obj as *mut _);

        // Parse texture format
        let wgpu_format = match format {
            "bgra8unorm" | "Bgra8Unorm" => wgpu_types::TextureFormat::Bgra8Unorm,
            "rgba8unorm" | "Rgba8Unorm" => wgpu_types::TextureFormat::Rgba8Unorm,
            "rgba16float" | "Rgba16Float" => wgpu_types::TextureFormat::Rgba16Float,
            "rgba32float" | "Rgba32Float" => wgpu_types::TextureFormat::Rgba32Float,
            _ => return Err(PyErr::new::<pyo3::exceptions::PyValueError, _>(
                format!("Unsupported texture format: {}. Supported: bgra8unorm, rgba8unorm, rgba16float, rgba32float", format)
            )),
        };

        // Get Metal texture properties
        let raw_type = metal_texture.texture_type();
        let array_layers = metal_texture.array_length();
        let mip_levels = metal_texture.mipmap_level_count();

        // Verify dimensions match
        if metal_texture.width() != width as u64 || metal_texture.height() != height as u64 {
            return Err(PyErr::new::<pyo3::exceptions::PyValueError, _>(
                format!(
                    "Dimension mismatch: Metal texture is {}x{}, but {}x{} was specified",
                    metal_texture.width(), metal_texture.height(), width, height
                )
            ));
        }

        // Create HAL texture from the Metal texture
        let hal_texture = wgpu_hal::metal::Texture::from_raw(
            metal_texture,
            wgpu_format,
            raw_type,
            array_layers as u32,
            mip_levels as u32,
            wgpu_hal::CopyExtent {
                width,
                height,
                depth: 1,
            }
        );

        // Create texture descriptor
        let descriptor = wgpu::TextureDescriptor {
            label: Some("External Metal Texture"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: mip_levels as u32,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu_format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        };

        // Create wgpu texture from HAL texture
        // This uses the forked wgpu's create_texture_from_hal API
        let wgpu_texture = device_ref.create_texture_from_hal::<wgpu_hal::metal::Api>(
            hal_texture,
            &descriptor,
        );

        // Box the texture and return pointer
        let boxed = Box::new(wgpu_texture);
        let ptr = Box::into_raw(boxed) as usize;

        Ok(ptr)
    }
}

// ============================================================================
// SHARED UTILITIES
// ============================================================================

/// Create a standalone Metal texture for display
///
/// Creates a Metal texture suitable for use as a display render target.
/// This texture can be wrapped as a wgpu texture and used for rendering.
///
/// # Arguments
/// * `device_ptr` - Pointer to Metal device
/// * `width` - Texture width in pixels
/// * `height` - Texture height in pixels
///
/// # Returns
/// * MetalTextureWrapper that can be converted to HAL and wrapped as wgpu texture
#[pyfunction]
fn create_metal_display_texture(
    device_ptr: u64,
    width: u32,
    height: u32,
) -> PyResult<MetalTextureWrapper> {
    unsafe {
        if device_ptr == 0 {
            return Err(PyErr::new::<pyo3::exceptions::PyValueError, _>(
                "Invalid device pointer"
            ));
        }

        let device = device_ptr as *mut Object;

        // Create a Metal texture descriptor
        let texture_descriptor_class = Class::get("MTLTextureDescriptor").unwrap();
        let texture_descriptor: *mut Object = msg_send![texture_descriptor_class, texture2DDescriptorWithPixelFormat: 80usize  // MTLPixelFormatBGRA8Unorm = 80
                                                                      width: width as usize
                                                                      height: height as usize
                                                                      mipmapped: false];

        // Set usage flags (render target + shader read)
        let _: () = msg_send![texture_descriptor, setUsage: 0x0001 | 0x0002 | 0x0004]; // ShaderRead | ShaderWrite | RenderTarget

        // Use Shared storage mode so both CPU and GPU can access
        let _: () = msg_send![texture_descriptor, setStorageMode: 0]; // MTLStorageModeShared = 0

        // Create the texture
        let metal_texture: *mut Object = msg_send![device, newTextureWithDescriptor: texture_descriptor];

        if metal_texture.is_null() {
            return Err(PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(
                "Failed to create Metal display texture"
            ));
        }

        // Retain the texture
        let _: () = msg_send![metal_texture, retain];

        Ok(MetalTextureWrapper {
            raw_texture: metal_texture,
            width,
            height,
        })
    }
}

/// Get the default Metal device pointer
///
/// Returns a pointer to the system's default Metal device.
/// This pointer can be used to create Metal textures and other resources.
///
/// # Returns
/// * `u64` - Pointer to MTLDevice object
#[pyfunction]
fn get_default_metal_device() -> PyResult<u64> {
    // Use the metal crate to get the system default device
    let device = metal::Device::system_default()
        .ok_or_else(|| PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(
            "Failed to get default Metal device - Metal may not be available on this system"
        ))?;

    // Get the raw pointer from the device
    // The device object must be retained to keep the pointer valid
    let device_ptr = device.as_ptr() as u64;

    // Retain the device to prevent deallocation
    unsafe {
        let _: () = msg_send![device.as_ptr(), retain];
    }

    Ok(device_ptr)
}

// ============================================================================
// DISPLAY SINK: Zero-copy wgpu → Metal → CAMetalLayer
// ============================================================================

/// Display sink for zero-copy rendering to macOS window
///
/// This struct owns the NSWindow, CAMetalLayer, and Metal resources.
/// It accepts wgpu-native C pointers and handles all the display logic in Rust.
#[pyclass]
pub struct DisplaySink {
    window: *mut Object,
    metal_layer: *mut Object,
    metal_device: metal::Device,
    command_queue: metal::CommandQueue,
    width: u32,
    height: u32,
}

unsafe impl Send for DisplaySink {}

impl Drop for DisplaySink {
    fn drop(&mut self) {
        unsafe {
            if !self.window.is_null() {
                let _: () = msg_send![self.window, close];
                let _: () = msg_send![self.window, release];
            }
            if !self.metal_layer.is_null() {
                let _: () = msg_send![self.metal_layer, release];
            }
        }
    }
}

/// Create a display sink for zero-copy rendering
#[pyfunction]
fn create_display_sink(width: u32, height: u32, title: &str) -> PyResult<DisplaySink> {
    unsafe {
        // Get Metal device
        let metal_device = metal::Device::system_default()
            .ok_or_else(|| PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(
                "Failed to get default Metal device"
            ))?;

        // Create command queue
        let command_queue = metal_device.new_command_queue();

        // Create window
        let ns_window_class = Class::get("NSWindow").unwrap();
        let window: *mut Object = msg_send![ns_window_class, alloc];

        // Window style mask: titled, closable, miniaturizable, resizable
        let style_mask: usize = 1 | 2 | 4 | 8;  // NSTitledWindowMask | NSClosableWindowMask | NSMiniaturizableWindowMask | NSResizableWindowMask

        // Create window with frame
        let window: *mut Object = msg_send![window,
            initWithContentRect: ((100.0f64, 100.0f64), (width as f64, height as f64))
            styleMask: style_mask
            backing: 2usize  // NSBackingStoreBuffered
            defer: false
        ];

        // Set window title
        let title_nsstring: *mut Object = msg_send![Class::get("NSString").unwrap(),
            stringWithUTF8String: title.as_ptr()
        ];
        let _: () = msg_send![window, setTitle: title_nsstring];

        // Create CAMetalLayer
        let ca_metal_layer_class = Class::get("CAMetalLayer").unwrap();
        let metal_layer: *mut Object = msg_send![ca_metal_layer_class, layer];
        let _: () = msg_send![metal_layer, retain];

        // Set layer properties
        let _: () = msg_send![metal_layer, setDevice: metal_device.as_ptr()];
        let _: () = msg_send![metal_layer, setPixelFormat: 80usize];  // MTLPixelFormatBGRA8Unorm
        let _: () = msg_send![metal_layer, setFramebufferOnly: false];

        // Set drawable size
        let _: () = msg_send![metal_layer, setDrawableSize: (width as f64, height as f64)];

        // Create content view and set layer
        let ns_view_class = Class::get("NSView").unwrap();
        let content_view: *mut Object = msg_send![ns_view_class, alloc];
        let content_view: *mut Object = msg_send![content_view,
            initWithFrame: ((100.0f64, 100.0f64), (width as f64, height as f64))
        ];
        let _: () = msg_send![content_view, setWantsLayer: true];
        let _: () = msg_send![content_view, setLayer: metal_layer];

        // Set content view and show window
        let _: () = msg_send![window, setContentView: content_view];
        let _: () = msg_send![window, makeKeyAndOrderFront: ptr::null::<Object>()];

        Ok(DisplaySink {
            window,
            metal_layer,
            metal_device,
            command_queue,
            width,
            height,
        })
    }
}

/// Present pixel data to the display
///
/// Takes pixel data from Python and uploads to Metal texture, then blits to display.
/// This uses a CPU copy as an intermediate step.
///
/// # Arguments
/// * `sink` - The DisplaySink that owns the window and layer
/// * `pixel_data` - Raw BGRA8 pixel data (width * height * 4 bytes)
///
/// # Note
/// This is not zero-copy, but provides a working display pipeline.
/// TODO: Implement true zero-copy using Metal texture extraction from wgpu
#[pyfunction]
fn display_sink_present(py: Python, sink: &DisplaySink, pixel_data: &[u8]) -> PyResult<()> {
    unsafe {
        // Validate pixel data size
        let expected_size = (sink.width * sink.height * 4) as usize;
        if pixel_data.len() != expected_size {
            return Err(PyErr::new::<pyo3::exceptions::PyValueError, _>(
                format!("Invalid pixel data size. Expected {} bytes, got {}", expected_size, pixel_data.len())
            ));
        }

        // Create a staging Metal texture to hold the pixel data
        let texture_descriptor_class = Class::get("MTLTextureDescriptor").unwrap();
        let texture_descriptor: *mut Object = msg_send![texture_descriptor_class,
            texture2DDescriptorWithPixelFormat: 80usize  // MTLPixelFormatBGRA8Unorm
            width: sink.width as usize
            height: sink.height as usize
            mipmapped: false
        ];

        // Set usage and storage mode
        let _: () = msg_send![texture_descriptor, setUsage: 0x0001]; // ShaderRead
        let _: () = msg_send![texture_descriptor, setStorageMode: 0]; // Shared (CPU and GPU accessible)

        // Create texture
        let staging_texture_obj: *mut Object = msg_send![sink.metal_device.as_ptr(),
            newTextureWithDescriptor: texture_descriptor
        ];

        if staging_texture_obj.is_null() {
            return Err(PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(
                "Failed to create staging Metal texture"
            ));
        }

        let staging_texture = metal::Texture::from_ptr(staging_texture_obj as *mut _);

        // Upload pixel data to staging texture
        let region = metal::MTLRegion {
            origin: metal::MTLOrigin { x: 0, y: 0, z: 0 },
            size: metal::MTLSize {
                width: sink.width as u64,
                height: sink.height as u64,
                depth: 1
            },
        };

        let bytes_per_row = sink.width * 4;
        staging_texture.replace_region(
            region,
            0,  // mipmap level
            pixel_data.as_ptr() as *const std::ffi::c_void,
            bytes_per_row as u64,
        );

        // Get next drawable from CAMetalLayer
        let drawable: *mut Object = msg_send![sink.metal_layer, nextDrawable];
        if drawable.is_null() {
            return Err(PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(
                "Failed to acquire drawable from CAMetalLayer"
            ));
        }

        let dest_texture_ptr: *mut Object = msg_send![drawable, texture];
        let dest_texture_metal = dest_texture_ptr as *mut metal::MTLTexture;
        let dest_texture = metal::Texture::from_ptr(dest_texture_metal);

        // Create Metal command buffer and blit from staging to drawable
        let command_buffer = sink.command_queue.new_command_buffer();
        let blit_encoder = command_buffer.new_blit_command_encoder();

        blit_encoder.copy_from_texture(
            &staging_texture,
            0,  // source_slice
            0,  // source_level
            metal::MTLOrigin { x: 0, y: 0, z: 0 },
            metal::MTLSize { width: sink.width as u64, height: sink.height as u64, depth: 1 },
            &dest_texture,
            0,  // destination_slice
            0,  // destination_level
            metal::MTLOrigin { x: 0, y: 0, z: 0 },
        );

        blit_encoder.end_encoding();

        // Present drawable
        let drawable_metal = drawable as *mut metal::MTLDrawable;
        let drawable_ref = metal::DrawableRef::from_ptr(drawable_metal);
        command_buffer.present_drawable(drawable_ref);
        command_buffer.commit();

        // Release staging texture
        let _: () = msg_send![staging_texture_obj, release];

        Ok(())
    }
}

// ============================================================================
// PYTHON MODULE
// ============================================================================

#[pymodule]
fn streamlib_metal(m: &Bound<'_, PyModule>) -> PyResult<()> {
    // Camera capture (IOSurface → Metal → wgpu)
    m.add_class::<MetalTextureWrapper>()?;
    m.add_function(wrap_pyfunction!(create_metal_texture_from_iosurface, m)?)?;
    m.add_function(wrap_pyfunction!(read_iosurface_pixels, m)?)?;

    // Display output
    m.add_function(wrap_pyfunction!(extract_metal_texture_from_wgpu, m)?)?;  // Extract Metal from wgpu (for display)
    // extract_metal_device_from_wgpu is disabled - not needed for current implementation
    // m.add_function(wrap_pyfunction!(extract_metal_device_from_wgpu, m)?)?;
    m.add_function(wrap_pyfunction!(create_wgpu_texture_from_metal, m)?)?;  // Create wgpu from Metal (for camera)

    // Shared utilities
    m.add_function(wrap_pyfunction!(get_default_metal_device, m)?)?;
    m.add_function(wrap_pyfunction!(create_metal_display_texture, m)?)?;

    // Display sink (zero-copy wgpu → Metal → CAMetalLayer)
    m.add_class::<DisplaySink>()?;
    m.add_function(wrap_pyfunction!(create_display_sink, m)?)?;
    m.add_function(wrap_pyfunction!(display_sink_present, m)?)?;

    Ok(())
}
