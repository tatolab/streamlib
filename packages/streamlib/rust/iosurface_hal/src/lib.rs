use pyo3::prelude::*;
use core_foundation::base::CFTypeRef;
use objc::{msg_send, sel, sel_impl};
use objc::runtime::{Class, Object};
use std::ptr;
use foreign_types::ForeignType;

// NOW WITH TRUE ZERO-COPY: Using our forked wgpu with public HAL texture constructor!

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

        // Create the HAL texture using our new public API from forked wgpu
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

/// Get the default Metal device pointer
#[pyfunction]
fn get_default_metal_device() -> u64 {
    unsafe {
        extern "C" {
            fn MTLCreateSystemDefaultDevice() -> *mut Object;
        }
        let device = MTLCreateSystemDefaultDevice();
        device as u64
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


#[pymodule]
fn iosurface_hal(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<MetalTextureWrapper>()?;
    m.add_function(wrap_pyfunction!(create_metal_texture_from_iosurface, m)?)?;
    m.add_function(wrap_pyfunction!(get_default_metal_device, m)?)?;
    m.add_function(wrap_pyfunction!(read_iosurface_pixels, m)?)?;
    Ok(())
}
