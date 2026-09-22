// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Private IOSurface allocation.
//!
//! Never `kIOSurfaceIsGlobal`: a global surface's id resolves from any
//! process on the machine. A private one reaches another process only
//! through a Mach port this process chose to send.

use objc2_core_foundation::{CFDictionary, CFNumber, CFRetained, CFString, CFType};
use objc2_io_surface::{
    IOSurfaceRef, kIOSurfaceBytesPerElement, kIOSurfaceBytesPerRow, kIOSurfaceHeight,
    kIOSurfacePixelFormat, kIOSurfaceWidth,
};

use crate::core::rhi::PixelFormat;
use crate::core::{Error, Result};

/// A retained IOSurface that may be held and read from any thread.
///
/// Everything done through it — retain, release, geometry and in-use
/// queries, `IOSurfaceCreateMachPort` — is thread-safe per IOSurface's own
/// contract; the binding simply does not say so.
#[derive(Clone)]
pub struct RetainedIOSurfaceSharedAcrossThreads(CFRetained<IOSurfaceRef>);

// SAFETY: see the type's doc — IOSurface's retain count, property reads and
// port minting are thread-safe, and nothing here mutates the surface.
unsafe impl Send for RetainedIOSurfaceSharedAcrossThreads {}
// SAFETY: as above.
unsafe impl Sync for RetainedIOSurfaceSharedAcrossThreads {}

impl RetainedIOSurfaceSharedAcrossThreads {
    /// Hold `iosurface`.
    pub fn new(iosurface: CFRetained<IOSurfaceRef>) -> Self {
        Self(iosurface)
    }
}

impl std::ops::Deref for RetainedIOSurfaceSharedAcrossThreads {
    type Target = IOSurfaceRef;
    fn deref(&self) -> &IOSurfaceRef {
        &self.0
    }
}

/// A fresh send right naming `iosurface`, owned — the one way a surface
/// leaves this process. Refused when IOSurface mints no port.
pub fn create_iosurface_mach_send_right(
    iosurface: &IOSurfaceRef,
) -> Result<streamlib_surface_client::OwnedMachSendRight> {
    match iosurface.create_mach_port() {
        mach2::port::MACH_PORT_NULL => Err(Error::TextureError(format!(
            "IOSurfaceCreateMachPort failed for the {}x{} IOSurface {}",
            iosurface.width(),
            iosurface.height(),
            iosurface.id()
        ))),
        // SAFETY: `IOSurfaceCreateMachPort` hands this task a fresh send
        // right that nothing else holds.
        port => Ok(unsafe { streamlib_surface_client::OwnedMachSendRight::from_raw_name(port) }),
    }
}

/// A private IOSurface of `height` rows of `width` elements, each
/// `bytes_per_element` wide, with rows packed back to back — no padding — so
/// its pages read as one tightly laid-out pixel buffer.
///
/// Tagged with `pixel_format`'s CoreVideo code when it is a packed,
/// single-plane format; otherwise the surface is described as elements
/// alone. Refused when IOSurface would not honour the packed row pitch.
pub fn create_private_iosurface_with_packed_rows(
    width: u32,
    height: u32,
    bytes_per_element: u32,
    pixel_format: PixelFormat,
) -> Result<CFRetained<IOSurfaceRef>> {
    const OPERATION: &str = "create_private_iosurface_with_packed_rows";
    let Some(packed_bytes_per_row) = width
        .checked_mul(bytes_per_element)
        .filter(|bytes| *bytes > 0 && height > 0)
    else {
        return Err(Error::Configuration(format!(
            "{OPERATION}: {width}x{height} at {bytes_per_element} byte(s) per element describes \
             no memory"
        )));
    };
    let as_cf_number = |value: u32| CFNumber::new_i64(i64::from(value));
    let width_number = as_cf_number(width);
    let height_number = as_cf_number(height);
    let bytes_per_element_number = as_cf_number(bytes_per_element);
    let bytes_per_row_number = as_cf_number(packed_bytes_per_row);
    let pixel_format_number = (!pixel_format.is_yuv() && pixel_format != PixelFormat::Unknown)
        .then(|| as_cf_number(pixel_format.as_cv_pixel_format_type()));

    // SAFETY: the IOSurface property keys are immutable framework statics.
    let mut keys: Vec<&CFString> = unsafe {
        vec![
            kIOSurfaceWidth,
            kIOSurfaceHeight,
            kIOSurfaceBytesPerElement,
            kIOSurfaceBytesPerRow,
        ]
    };
    let mut values: Vec<&CFType> = vec![
        &width_number,
        &height_number,
        &bytes_per_element_number,
        &bytes_per_row_number,
    ];
    if let Some(pixel_format_number) = pixel_format_number.as_deref() {
        // SAFETY: as above.
        keys.push(unsafe { kIOSurfacePixelFormat });
        values.push(pixel_format_number);
    }
    let properties = CFDictionary::<CFString, CFType>::from_slices(&keys, &values);

    // SAFETY: the dictionary holds only the documented property keys, with
    // CFNumber values.
    let iosurface = unsafe { IOSurfaceRef::new(properties.as_opaque()) }.ok_or_else(|| {
        Error::TextureError(format!(
            "{OPERATION}: IOSurfaceCreate refused {width}x{height} at {bytes_per_element} \
             byte(s) per element"
        ))
    })?;
    if iosurface.bytes_per_row() != packed_bytes_per_row as usize {
        return Err(Error::TextureError(format!(
            "{OPERATION}: IOSurface padded {width}x{height}'s rows to {} bytes; the packed \
             layout needs {packed_bytes_per_row}",
            iosurface.bytes_per_row()
        )));
    }
    Ok(iosurface)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_odd_width_keeps_its_rows_packed_and_its_base_page_aligned() {
        let iosurface = create_private_iosurface_with_packed_rows(641, 3, 4, PixelFormat::Bgra32)
            .expect("a private IOSurface");
        assert_eq!(iosurface.width(), 641);
        assert_eq!(iosurface.height(), 3);
        assert_eq!(iosurface.bytes_per_row(), 641 * 4);
        assert_eq!(
            iosurface.pixel_format(),
            PixelFormat::Bgra32.as_cv_pixel_format_type()
        );
        assert!(iosurface.alloc_size() >= 641 * 4 * 3);
        let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
        assert_eq!(iosurface.base_address().as_ptr() as usize % page_size, 0);
    }

    #[test]
    fn a_fresh_surface_is_not_in_use() {
        let iosurface = create_private_iosurface_with_packed_rows(64, 64, 4, PixelFormat::Rgba32)
            .expect("a private IOSurface");
        assert!(!iosurface.is_in_use());
    }

    #[test]
    fn a_yuv_format_is_described_by_elements_alone() {
        let iosurface =
            create_private_iosurface_with_packed_rows(64, 64, 1, PixelFormat::Nv12VideoRange)
                .expect("a private IOSurface");
        assert_eq!(iosurface.pixel_format(), 0);
        assert_eq!(iosurface.bytes_per_row(), 64);
    }

    #[test]
    fn an_empty_extent_is_refused_by_name() {
        let refused =
            create_private_iosurface_with_packed_rows(0, 64, 4, PixelFormat::Bgra32).unwrap_err();
        assert!(refused.to_string().contains("describes no memory"));
    }
}
