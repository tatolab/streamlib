use pyo3::prelude::*;
use core_foundation::base::CFTypeRef;
use metal::foreign_types::ForeignType;
use std::ffi::c_void;

// ============================================================================
// C API Type Definitions (from webgpu.h)
// ============================================================================

/// Opaque pointer to WGPUDevice (from wgpu-native C API)
#[repr(C)]
pub struct WGPUDeviceImpl {
    _private: [u8; 0],
}
pub type WGPUDevice = *mut WGPUDeviceImpl;

/// Opaque pointer to WGPUTexture (from wgpu-native C API)
#[repr(C)]
pub struct WGPUTextureImpl {
    _private: [u8; 0],
}
pub type WGPUTexture = *mut WGPUTextureImpl;

/// Extent3D structure (matches webgpu.h)
#[repr(C)]
#[derive(Copy, Clone)]
pub struct WGPUExtent3D {
    pub width: u32,
    pub height: u32,
    pub depth_or_array_layers: u32,
}

/// String view for labels (matches webgpu.h)
#[repr(C)]
#[derive(Copy, Clone)]
pub struct WGPUStringView {
    pub data: *const u8,
    pub length: usize,
}

/// Chained struct for extensions
#[repr(C)]
pub struct WGPUChainedStruct {
    pub next: *const WGPUChainedStruct,
    pub s_type: u32,
}

/// Texture descriptor (matches webgpu.h)
#[repr(C)]
pub struct WGPUTextureDescriptor {
    pub next_in_chain: *const WGPUChainedStruct,
    pub label: WGPUStringView,
    pub usage: u32,              // WGPUTextureUsage bitflags
    pub dimension: u32,           // WGPUTextureDimension
    pub size: WGPUExtent3D,
    pub format: u32,              // WGPUTextureFormat
    pub mip_level_count: u32,
    pub sample_count: u32,
    pub view_format_count: usize,
    pub view_formats: *const u32,
}

// Texture usage flags (from webgpu.h)
pub const WGPU_TEXTURE_USAGE_TEXTURE_BINDING: u32 = 0x00000004;
pub const WGPU_TEXTURE_USAGE_RENDER_ATTACHMENT: u32 = 0x00000010;

// Texture dimensions (from webgpu.h)
pub const WGPU_TEXTURE_DIMENSION_2D: u32 = 0x00000001;

// Texture formats (from webgpu.h)
pub const WGPU_TEXTURE_FORMAT_BGRA8_UNORM: u32 = 0x00000017;

// ============================================================================
// C API Function Declarations (from wgpu-native)
// ============================================================================

extern "C" {
    /// Create a texture using wgpu-native C API
    /// Signature: WGPUTexture wgpuDeviceCreateTexture(WGPUDevice device, WGPUTextureDescriptor const * descriptor)
    fn wgpuDeviceCreateTexture(
        device: WGPUDevice,
        descriptor: *const WGPUTextureDescriptor,
    ) -> WGPUTexture;

    /// Release a texture reference
    /// Signature: void wgpuTextureRelease(WGPUTexture texture)
    fn wgpuTextureRelease(texture: WGPUTexture);

    /// Get the underlying Metal texture from a wgpu texture
    /// This is a wgpu-native extension function
    fn wgpuTextureGetMetalTexture(texture: WGPUTexture) -> *mut c_void;
}

// ============================================================================
// Main Function: Create Texture from IOSurface using C API
// ============================================================================

/// Create a wgpu::Texture from an IOSurface using WGPUDevice C API directly.
///
/// This is Option B: Use wgpu-native's C API instead of Rust wgpu::Device API.
///
/// Approach:
/// 1. IOSurface → Metal Texture (zero-copy via newTextureWithDescriptor:iosurface:plane:)
/// 2. Metal Texture → Register with WGPUDevice via C API
/// 3. Return WGPUTexture pointer to Python
///
/// # Arguments
/// * `device_ptr` - WGPUDevice C pointer (from wgpu-py's device._internal)
/// * `iosurface_ptr` - Pointer to IOSurface (from AVFoundation CVPixelBuffer)
///
/// # Returns
/// * `(texture_ptr, width, height)` - Pointer to WGPUTexture and dimensions
///
/// # Safety
/// This function is unsafe because it:
/// - Works with raw C pointers from wgpu-native
/// - Calls extern C functions
/// - Interacts with Metal and IOSurface via Objective-C
#[pyfunction]
fn create_texture_from_iosurface_c_api(
    device_ptr: u64,
    iosurface_ptr: u64,
) -> PyResult<(u64, u32, u32)> {
    unsafe {
        // Cast pointers
        let wgpu_device = device_ptr as WGPUDevice;
        let iosurface = iosurface_ptr as CFTypeRef;
        let iosurface_obj = iosurface as *mut objc::runtime::Object;

        if wgpu_device.is_null() {
            return Err(pyo3::exceptions::PyRuntimeError::new_err(
                "WGPUDevice pointer is null"
            ));
        }

        // Get IOSurface dimensions using Objective-C
        use objc::{msg_send, sel, sel_impl};
        let width: usize = msg_send![iosurface_obj, width];
        let height: usize = msg_send![iosurface_obj, height];

        // STEP 1: Create Metal texture from IOSurface (ZERO-COPY)
        // We need to get the Metal device from the WGPUDevice
        // For now, create our own Metal device (same approach as current lib.rs)
        let mtl_device = metal::Device::system_default()
            .ok_or_else(|| pyo3::exceptions::PyRuntimeError::new_err(
                "Failed to create Metal device"
            ))?;

        // Create Metal texture descriptor
        let mtl_desc = metal::TextureDescriptor::new();
        mtl_desc.set_texture_type(metal::MTLTextureType::D2);
        mtl_desc.set_pixel_format(metal::MTLPixelFormat::BGRA8Unorm);
        mtl_desc.set_width(width as u64);
        mtl_desc.set_height(height as u64);
        mtl_desc.set_usage(
            metal::MTLTextureUsage::ShaderRead
            | metal::MTLTextureUsage::RenderTarget
        );

        // Create Metal texture from IOSurface (ZERO COPY)
        let mtl_texture_raw: *mut objc::runtime::Object = msg_send![
            mtl_device.as_ptr(),
            newTextureWithDescriptor: mtl_desc.as_ptr()
            iosurface: iosurface
            plane: 0u64
        ];

        if mtl_texture_raw.is_null() {
            return Err(pyo3::exceptions::PyRuntimeError::new_err(
                "Failed to create Metal texture from IOSurface"
            ));
        }

        let mtl_texture_ptr = mtl_texture_raw as *mut metal::MTLTexture;
        let mtl_texture = metal::Texture::from_ptr(mtl_texture_ptr);

        // STEP 2: Register Metal texture with WGPUDevice using C API
        // We need to use wgpu_hal to convert Metal texture to HAL texture,
        // then somehow register it with the C API device

        // Create HAL texture from Metal texture
        use wgpu_hal::metal::Device as MetalDevice;
        let hal_texture = MetalDevice::texture_from_raw(
            mtl_texture,
            wgpu_types::TextureFormat::Bgra8Unorm,
            metal::MTLTextureType::D2,
            1,  // array_layers
            1,  // mip_levels
            wgpu_hal::CopyExtent {
                width: width as u32,
                height: height as u32,
                depth: 1,
            },
        );

        // STEP 3: The challenge - we need to register hal_texture with WGPUDevice
        // Option 3a: Use wgpuDeviceCreateTexture with descriptor pointing to Metal texture
        // Option 3b: Use wgpu-native extension to import external texture

        // For now, let's try creating a WGPUTexture descriptor and calling C API
        let label = b"IOSurface Camera\0";
        let label_view = WGPUStringView {
            data: label.as_ptr(),
            length: label.len() - 1,
        };

        let wgpu_desc = WGPUTextureDescriptor {
            next_in_chain: std::ptr::null(),
            label: label_view,
            usage: WGPU_TEXTURE_USAGE_TEXTURE_BINDING | WGPU_TEXTURE_USAGE_RENDER_ATTACHMENT,
            dimension: WGPU_TEXTURE_DIMENSION_2D,
            size: WGPUExtent3D {
                width: width as u32,
                height: height as u32,
                depth_or_array_layers: 1,
            },
            format: WGPU_TEXTURE_FORMAT_BGRA8_UNORM,
            mip_level_count: 1,
            sample_count: 1,
            view_format_count: 0,
            view_formats: std::ptr::null(),
        };

        // Call wgpu-native C API to create texture
        // NOTE: This creates a NEW texture, not wrapping our Metal texture!
        // We need a different approach - see comments below
        let wgpu_texture = wgpuDeviceCreateTexture(wgpu_device, &wgpu_desc);

        if wgpu_texture.is_null() {
            return Err(pyo3::exceptions::PyRuntimeError::new_err(
                "wgpuDeviceCreateTexture failed"
            ));
        }

        // LIMITATION: Standard wgpuDeviceCreateTexture creates a NEW texture,
        // not wrapping our existing Metal texture from IOSurface.
        //
        // To actually wrap the IOSurface Metal texture, we need one of:
        // 1. wgpu-native extension function (if it exists)
        // 2. Direct manipulation of wgpu-core internals
        // 3. Return hal_texture to Python and let Python wrap it
        //
        // For now, this demonstrates the C API approach but doesn't achieve zero-copy.

        Ok((wgpu_texture as u64, width as u32, height as u32))
    }
}

/// Release a WGPUTexture created via C API
///
/// # Arguments
/// * `texture_ptr` - WGPUTexture pointer to release
///
/// # Safety
/// Caller must ensure texture_ptr is valid
#[pyfunction]
fn release_texture_c_api(texture_ptr: u64) -> PyResult<()> {
    unsafe {
        let texture = texture_ptr as WGPUTexture;
        if !texture.is_null() {
            wgpuTextureRelease(texture);
        }
        Ok(())
    }
}

#[pymodule]
fn iosurface_hal(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(create_texture_from_iosurface_c_api, m)?)?;
    m.add_function(wrap_pyfunction!(release_texture_c_api, m)?)?;
    Ok(())
}

// ============================================================================
// Notes on Implementation Challenges
// ============================================================================
//
// CHALLENGE: The standard wgpuDeviceCreateTexture() C API creates a NEW texture
// with GPU-allocated memory. It doesn't allow importing an existing Metal texture.
//
// SOLUTIONS EXPLORED:
//
// 1. **wgpu-native Extension** (PREFERRED if available)
//    - Look for wgpuDeviceImportTexture() or similar
//    - Check wgpu.h for Metal-specific extensions
//    - May require wgpu-native version with import support
//
// 2. **wgpu-core Direct Access** (RISKY)
//    - Cast WGPUDevice → wgpu_core::id::DeviceId
//    - Access global registry: wgpu_core::hub::global::Global
//    - Call register_texture() on HAL texture
//    - Bypasses C API safety guarantees
//
// 3. **Return HAL Texture to Python** (CURRENT WORKAROUND)
//    - Create Metal texture in Rust
//    - Return raw Metal texture pointer
//    - Let Python use wgpu-py to wrap via create_texture_from_hal()
//    - This is what lib.rs currently does
//
// 4. **Use wgpu::Device API** (ORIGINAL APPROACH)
//    - Cast device_ptr to *mut wgpu::Device
//    - Call device.create_texture_from_hal()
//    - Problem: Causes hangs when dereferencing device pointer
//
// CONCLUSION: Option B (C API approach) faces fundamental limitation:
// - C API doesn't expose texture import functionality
// - Would need wgpu-native extension or internal registry access
// - Current lib.rs approach (return Metal texture) may be most practical
//
