// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Private IOSurface allocation, and the host read of one.
//!
//! Never `kIOSurfaceIsGlobal`: a global surface's id resolves from any
//! process on the machine. A private one reaches another process only
//! through a Mach port this process chose to send.

use objc2_core_foundation::{CFDictionary, CFNumber, CFRetained, CFString, CFType};
use objc2_io_surface::{
    IOSurfaceLockOptions, IOSurfaceRef, kIOSurfaceBytesPerElement, kIOSurfaceBytesPerRow,
    kIOSurfaceHeight, kIOSurfacePixelFormat, kIOSurfaceWidth,
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

/// Hand `read_rows` the first `row_count` rows of `iosurface`, each
/// `row_byte_len` bytes and back to back, while the surface is locked for
/// reading.
///
/// A surface whose stride equals `row_byte_len` is handed over in place; one
/// whose stride pads its rows is copied row by row into packed storage first.
pub fn with_iosurface_rows_tightly_packed_for_reading<R>(
    iosurface: &IOSurfaceRef,
    row_byte_len: usize,
    row_count: usize,
    read_rows: impl FnOnce(&[u8]) -> R,
) -> Result<R> {
    let bytes_per_row = iosurface.bytes_per_row();
    let byte_span_read = row_count
        .checked_sub(1)
        .and_then(|rows_before_the_last| rows_before_the_last.checked_mul(bytes_per_row))
        .and_then(|bytes_before_the_last_row| bytes_before_the_last_row.checked_add(row_byte_len));
    let readable = row_byte_len > 0
        && bytes_per_row >= row_byte_len
        && iosurface.height() >= row_count
        && byte_span_read.is_some_and(|byte_span| byte_span <= iosurface.alloc_size());
    if !readable {
        return Err(Error::GpuError(format!(
            "IOSurface {} is {} rows at {bytes_per_row} bytes per row in {} bytes, which cannot \
             hold {row_count} rows of {row_byte_len} bytes",
            iosurface.id(),
            iosurface.height(),
            iosurface.alloc_size()
        )));
    }
    let locked_for_reading = IOSurfaceLockedForReading::lock(iosurface)?;
    let base_address = locked_for_reading.base_address();
    if bytes_per_row == row_byte_len {
        // SAFETY: the surface is locked, and `row_byte_len * row_count` is the
        // byte span checked above to lie inside its allocation.
        let rows = unsafe { std::slice::from_raw_parts(base_address, row_byte_len * row_count) };
        return Ok(read_rows(rows));
    }
    let mut packed_rows = Vec::with_capacity(row_byte_len * row_count);
    for row in 0..row_count {
        // SAFETY: as above; the last row's `row_byte_len` bytes end at the
        // checked byte span, and every earlier row ends before it.
        packed_rows.extend_from_slice(unsafe {
            std::slice::from_raw_parts(base_address.add(row * bytes_per_row), row_byte_len)
        });
    }
    Ok(read_rows(&packed_rows))
}

/// An IOSurface locked read-only, unlocked when this drops.
struct IOSurfaceLockedForReading<'a> {
    iosurface: &'a IOSurfaceRef,
}

impl<'a> IOSurfaceLockedForReading<'a> {
    fn lock(iosurface: &'a IOSurfaceRef) -> Result<Self> {
        // SAFETY: a null seed pointer is the documented "not wanted".
        let locked =
            unsafe { iosurface.lock(IOSurfaceLockOptions::ReadOnly, std::ptr::null_mut()) };
        if locked != 0 {
            return Err(Error::GpuError(format!(
                "IOSurfaceLock refused IOSurface {} for reading ({locked})",
                iosurface.id()
            )));
        }
        Ok(Self { iosurface })
    }

    /// The surface's mapped pages, valid for as long as this lock is held.
    fn base_address(&self) -> *const u8 {
        self.iosurface
            .base_address()
            .as_ptr()
            .cast::<u8>()
            .cast_const()
    }
}

impl Drop for IOSurfaceLockedForReading<'_> {
    fn drop(&mut self) {
        // SAFETY: paired with the read-only lock this value was made by.
        let unlocked = unsafe {
            self.iosurface
                .unlock(IOSurfaceLockOptions::ReadOnly, std::ptr::null_mut())
        };
        if unlocked != 0 {
            tracing::warn!(
                "IOSurfaceUnlock refused IOSurface {} after a read ({unlocked})",
                self.iosurface.id()
            );
        }
    }
}

/// Write `rows` into `iosurface`, one per stride — a producer's side, for
/// tests.
#[cfg(test)]
pub(crate) fn write_rows_at_the_iosurfaces_stride(iosurface: &IOSurfaceRef, rows: &[&[u8]]) {
    let locked = unsafe { iosurface.lock(IOSurfaceLockOptions::empty(), std::ptr::null_mut()) };
    assert_eq!(locked, 0, "IOSurfaceLock");
    let base_address = iosurface.base_address().as_ptr().cast::<u8>();
    for (row_index, row) in rows.iter().enumerate() {
        assert!(
            row.len() <= iosurface.bytes_per_row(),
            "a row fits its stride"
        );
        // SAFETY: the surface is locked for writing and the row fits inside
        // its own stride.
        unsafe {
            std::ptr::copy_nonoverlapping(
                row.as_ptr(),
                base_address.add(row_index * iosurface.bytes_per_row()),
                row.len(),
            )
        };
    }
    let unlocked = unsafe { iosurface.unlock(IOSurfaceLockOptions::empty(), std::ptr::null_mut()) };
    assert_eq!(unlocked, 0, "IOSurfaceUnlock");
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
    let packed_bytes_per_row =
        byte_size_of_one_packed_row(OPERATION, width, height, bytes_per_element)?;
    let iosurface = create_private_iosurface(
        OPERATION,
        width,
        height,
        bytes_per_element,
        Some(packed_bytes_per_row),
        pixel_format,
    )?;
    if iosurface.bytes_per_row() != packed_bytes_per_row as usize {
        return Err(Error::TextureError(format!(
            "{OPERATION}: IOSurface padded {width}x{height}'s rows to {} bytes; the packed \
             layout needs {packed_bytes_per_row}",
            iosurface.bytes_per_row()
        )));
    }
    Ok(iosurface)
}

/// A private IOSurface of `height` rows of `width` elements, each
/// `bytes_per_element` wide, with the row pitch IOSurface chooses — the
/// alignment a Metal texture over the surface needs. Readers take the
/// stride from the surface, never from `width`.
pub fn create_private_iosurface_for_a_gpu_image(
    width: u32,
    height: u32,
    bytes_per_element: u32,
) -> Result<CFRetained<IOSurfaceRef>> {
    const OPERATION: &str = "create_private_iosurface_for_a_gpu_image";
    byte_size_of_one_packed_row(OPERATION, width, height, bytes_per_element)?;
    create_private_iosurface(
        OPERATION,
        width,
        height,
        bytes_per_element,
        None,
        PixelFormat::Unknown,
    )
}

fn byte_size_of_one_packed_row(
    operation: &str,
    width: u32,
    height: u32,
    bytes_per_element: u32,
) -> Result<u32> {
    width
        .checked_mul(bytes_per_element)
        .filter(|bytes| *bytes > 0 && height > 0)
        .ok_or_else(|| {
            Error::Configuration(format!(
                "{operation}: {width}x{height} at {bytes_per_element} byte(s) per element \
                 describes no memory"
            ))
        })
}

fn create_private_iosurface(
    operation: &str,
    width: u32,
    height: u32,
    bytes_per_element: u32,
    bytes_per_row: Option<u32>,
    pixel_format: PixelFormat,
) -> Result<CFRetained<IOSurfaceRef>> {
    let as_cf_number = |value: u32| CFNumber::new_i64(i64::from(value));
    let width_number = as_cf_number(width);
    let height_number = as_cf_number(height);
    let bytes_per_element_number = as_cf_number(bytes_per_element);
    let bytes_per_row_number = bytes_per_row.map(as_cf_number);
    let pixel_format_number = (!pixel_format.is_yuv() && pixel_format != PixelFormat::Unknown)
        .then(|| as_cf_number(pixel_format.as_cv_pixel_format_type()));

    // SAFETY: the IOSurface property keys are immutable framework statics.
    let mut keys: Vec<&CFString> =
        unsafe { vec![kIOSurfaceWidth, kIOSurfaceHeight, kIOSurfaceBytesPerElement] };
    let mut values: Vec<&CFType> = vec![&width_number, &height_number, &bytes_per_element_number];
    if let Some(bytes_per_row_number) = bytes_per_row_number.as_deref() {
        // SAFETY: as above.
        keys.push(unsafe { kIOSurfaceBytesPerRow });
        values.push(bytes_per_row_number);
    }
    if let Some(pixel_format_number) = pixel_format_number.as_deref() {
        // SAFETY: as above.
        keys.push(unsafe { kIOSurfacePixelFormat });
        values.push(pixel_format_number);
    }
    let properties = CFDictionary::<CFString, CFType>::from_slices(&keys, &values);

    // SAFETY: the dictionary holds only the documented property keys, with
    // CFNumber values.
    unsafe { IOSurfaceRef::new(properties.as_opaque()) }.ok_or_else(|| {
        Error::TextureError(format!(
            "{operation}: IOSurfaceCreate refused {width}x{height} at {bytes_per_element} \
             byte(s) per element"
        ))
    })
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
    fn a_gpu_image_surface_takes_the_row_pitch_iosurface_aligns() {
        let iosurface =
            create_private_iosurface_for_a_gpu_image(641, 3, 4).expect("a private IOSurface");
        assert_eq!(iosurface.width(), 641);
        assert_eq!(iosurface.bytes_per_element(), 4);
        assert!(iosurface.bytes_per_row() >= 641 * 4);
        assert!(iosurface.alloc_size() >= iosurface.bytes_per_row() * 3);
    }

    #[test]
    fn an_empty_extent_is_refused_by_name() {
        let refused =
            create_private_iosurface_with_packed_rows(0, 64, 4, PixelFormat::Bgra32).unwrap_err();
        assert!(refused.to_string().contains("describes no memory"));
    }

    /// Rows no misaligned read passes for: every byte differs from its
    /// neighbours and from the same byte of the next row.
    fn distinct_rows(row_byte_len: usize, row_count: usize) -> Vec<Vec<u8>> {
        (0..row_count)
            .map(|row| {
                (0..row_byte_len)
                    .map(|at| ((row * row_byte_len + at) % 251) as u8)
                    .collect()
            })
            .collect()
    }

    #[test]
    fn a_padded_surface_reads_out_with_its_padding_stripped() {
        let iosurface =
            create_private_iosurface_for_a_gpu_image(1000, 5, 4).expect("a private IOSurface");
        assert!(
            iosurface.bytes_per_row() > 1000 * 4,
            "the fixture needs a stride that pads its rows"
        );
        let rows = distinct_rows(1000 * 4, 5);
        write_rows_at_the_iosurfaces_stride(
            &iosurface,
            &rows.iter().map(Vec::as_slice).collect::<Vec<_>>(),
        );

        let read_out =
            with_iosurface_rows_tightly_packed_for_reading(&iosurface, 1000 * 4, 5, <[u8]>::to_vec)
                .expect("the rows read out");

        assert_eq!(read_out, rows.concat());
    }

    #[test]
    fn a_packed_surface_reads_out_in_place() {
        let iosurface = create_private_iosurface_with_packed_rows(641, 3, 4, PixelFormat::Bgra32)
            .expect("a private IOSurface");
        let rows = distinct_rows(641 * 4, 3);
        write_rows_at_the_iosurfaces_stride(
            &iosurface,
            &rows.iter().map(Vec::as_slice).collect::<Vec<_>>(),
        );

        let read_in_place_from =
            with_iosurface_rows_tightly_packed_for_reading(&iosurface, 641 * 4, 3, |packed_rows| {
                (packed_rows.as_ptr(), packed_rows.to_vec())
            })
            .expect("the rows read out");

        assert_eq!(
            read_in_place_from.0,
            iosurface.base_address().as_ptr().cast::<u8>().cast_const()
        );
        assert_eq!(read_in_place_from.1, rows.concat());
    }

    #[test]
    fn rows_wider_than_the_surfaces_stride_are_refused_before_the_lock() {
        let iosurface = create_private_iosurface_with_packed_rows(64, 4, 4, PixelFormat::Rgba32)
            .expect("a private IOSurface");

        let refused =
            with_iosurface_rows_tightly_packed_for_reading(&iosurface, 64 * 4 + 1, 4, |_| ())
                .unwrap_err();

        assert!(refused.to_string().contains("cannot hold"), "{refused}");
    }

    #[test]
    fn a_read_of_no_rows_is_refused_rather_than_handed_an_empty_frame() {
        let iosurface = create_private_iosurface_with_packed_rows(64, 4, 4, PixelFormat::Rgba32)
            .expect("a private IOSurface");

        let refused = with_iosurface_rows_tightly_packed_for_reading(&iosurface, 64 * 4, 0, |_| ())
            .unwrap_err();

        assert!(refused.to_string().contains("cannot hold"), "{refused}");
    }
}
