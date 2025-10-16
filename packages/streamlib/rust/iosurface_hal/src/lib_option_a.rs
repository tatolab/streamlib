use pyo3::prelude::*;
use core_foundation::base::CFTypeRef;
use metal::foreign_types::ForeignType;

/// Create a Metal texture from an IOSurface - NO wgpu::Device required!
///
/// This approach:
/// 1. Creates its own Metal device using MTLCreateSystemDefaultDevice
/// 2. Creates Metal texture from IOSurface (zero-copy)
/// 3. Returns the Metal texture pointer to Python
/// 4. Python side wraps it using wgpu-py's create_texture_from_hal()
///
/// This is simpler because:
/// - No need to pass device_ptr from Python (which was hanging)
/// - No unsafe dereferencing of wgpu::Device pointer
/// - Let Python do the wgpu wrapping (where it's easier)
///
/// # Arguments
/// * `iosurface_ptr` - Pointer to IOSurface (from AVFoundation CVPixelBuffer)
///
/// # Returns
/// * `(metal_texture_ptr, width, height)` - Pointer to Metal texture and dimensions
///
/// # Safety
/// This function is unsafe because it dereferences raw pointers.
#[pyfunction]
fn create_metal_texture_from_iosurface(
    iosurface_ptr: u64,
) -> PyResult<(u64, u32, u32)> {
    unsafe {
        // Cast IOSurface pointer
        let iosurface = iosurface_ptr as CFTypeRef;
        let iosurface_obj = iosurface as *mut objc::runtime::Object;

        // Get IOSurface dimensions
        use objc::{msg_send, sel, sel_impl};
        let width: usize = msg_send![iosurface_obj, width];
        let height: usize = msg_send![iosurface_obj, height];

        // STEP 1: Create our own Metal device
        let mtl_device = metal::Device::system_default()
            .ok_or_else(|| pyo3::exceptions::PyRuntimeError::new_err(
                "Failed to create Metal device"
            ))?;

        // STEP 2: Create Metal texture descriptor
        let desc = metal::TextureDescriptor::new();
        desc.set_texture_type(metal::MTLTextureType::D2);
        desc.set_pixel_format(metal::MTLPixelFormat::BGRA8Unorm);
        desc.set_width(width as u64);
        desc.set_height(height as u64);
        desc.set_usage(
            metal::MTLTextureUsage::ShaderRead
            | metal::MTLTextureUsage::RenderTarget
        );

        // STEP 3: Create Metal texture from IOSurface (ZERO COPY)
        let mtl_texture_raw: *mut objc::runtime::Object = msg_send![
            mtl_device.as_ptr(),
            newTextureWithDescriptor: desc.as_ptr()
            iosurface: iosurface
            plane: 0u64
        ];

        if mtl_texture_raw.is_null() {
            return Err(pyo3::exceptions::PyRuntimeError::new_err(
                "Failed to create Metal texture from IOSurface"
            ));
        }

        // STEP 4: Wrap as Metal texture and leak it (Python will own it)
        let mtl_texture_ptr = mtl_texture_raw as *mut metal::MTLTexture;
        let mtl_texture = metal::Texture::from_ptr(mtl_texture_ptr);

        // Don't let Rust drop the texture - Python will manage it
        let leaked_ptr = Box::into_raw(Box::new(mtl_texture));

        // Return Metal texture pointer to Python
        // Python will use wgpu-py to wrap this in a wgpu::Texture
        Ok((leaked_ptr as u64, width as u32, height as u32))
    }
}

/// Clean up a Metal texture created by create_metal_texture_from_iosurface
///
/// Call this when you're done with the texture to avoid memory leaks.
///
/// # Arguments
/// * `metal_texture_ptr` - Pointer to Metal texture returned by create_metal_texture_from_iosurface
///
/// # Safety
/// This function is unsafe because it takes ownership of a raw pointer.
#[pyfunction]
fn release_metal_texture(metal_texture_ptr: u64) -> PyResult<()> {
    unsafe {
        if metal_texture_ptr != 0 {
            let texture = Box::from_raw(metal_texture_ptr as *mut metal::Texture);
            drop(texture);
        }
        Ok(())
    }
}

#[pymodule]
fn iosurface_hal(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(create_metal_texture_from_iosurface, m)?)?;
    m.add_function(wrap_pyfunction!(release_metal_texture, m)?)?;
    Ok(())
}
