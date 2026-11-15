// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use streamlib_adapter_cuda::dlpack::DataType;

use super::*;

#[test]
fn bgra_layout_is_height_width_four_bytes() {
    let layout =
        PixelExchangeTensorLayout::for_pixel_format(PixelFormat::Bgra32, 640, 480, 640 * 4)
            .expect("bgra has a single-buffer shape");
    assert_eq!(layout.shape, vec![480, 640, 4]);
    // Element size is one byte, so element strides equal byte strides
    // — the same (bytes_per_row, 4, 1) the pre-wheel surface returned.
    assert_eq!(layout.strides, vec![2560, 4, 1]);
    assert_eq!(layout.dtype, DataType::U8);
}

#[test]
fn a_padded_row_pitch_survives_into_the_stride() {
    // 640 pixels of BGRA is 2560 bytes; a driver that pads to 2688
    // must not be flattened into a tightly-packed view.
    let layout = PixelExchangeTensorLayout::for_pixel_format(PixelFormat::Bgra32, 640, 480, 2688)
        .expect("bgra has a single-buffer shape");
    assert_eq!(layout.shape, vec![480, 640, 4]);
    assert_eq!(layout.strides[0], 2688);
}

#[test]
fn sixteen_bit_strides_are_counted_in_elements_not_bytes() {
    // DLPack strides are element counts. RGBA64 is 8 bytes per pixel
    // over 2-byte elements, so a 640-pixel row of 5120 bytes is 2560
    // elements and the channel step is 1 element, not 2 bytes.
    let layout =
        PixelExchangeTensorLayout::for_pixel_format(PixelFormat::Rgba64, 640, 480, 640 * 8)
            .expect("rgba64 has a single-buffer shape");
    assert_eq!(layout.shape, vec![480, 640, 4]);
    assert_eq!(layout.strides, vec![2560, 4, 1]);
    assert_eq!(layout.dtype.bits, 16);
}

#[test]
fn gray8_has_no_channel_axis() {
    let layout = PixelExchangeTensorLayout::for_pixel_format(PixelFormat::Gray8, 320, 200, 320)
        .expect("gray8 has a single-buffer shape");
    assert_eq!(layout.shape, vec![200, 320]);
    assert_eq!(layout.strides, vec![320, 1]);
}

/// NV12 is two planes; a one-buffer export would be luma only.
#[test]
fn multi_plane_formats_are_refused_rather_than_truncated() {
    Python::initialize();
    for format in [PixelFormat::Nv12VideoRange, PixelFormat::Nv12FullRange] {
        let refusal = PixelExchangeTensorLayout::for_pixel_format(format, 640, 480, 640)
            .expect_err("nv12 must not export as one buffer");
        assert!(
            refusal.to_string().contains("one strided linear buffer"),
            "the refusal should say why, got: {refusal}"
        );
    }
}

#[test]
fn the_access_gate_refuses_an_export_taken_without_a_lock() {
    Python::initialize();
    let gate = CpuAccessGate::new_unlocked();
    let refusal = gate.require_locked().expect_err("unlocked must refuse");
    assert!(refusal.to_string().contains("not locked"));

    gate.lock_for(true);
    assert!(gate.require_locked().is_ok());

    gate.unlock();
    assert!(
        gate.require_locked().is_err(),
        "unlock closes the gate again"
    );
}

// -------------------------------------------------------------------
// Capsule ownership. The DLPack protocol splits responsibility for the
// deleter between producer and consumer at the rename, and getting that
// split wrong is either a leak or a double free — neither of which a
// pixel-shaped test would surface.
// -------------------------------------------------------------------

use std::sync::atomic::{AtomicUsize, Ordering};

/// The name a consumer renames the capsule to once it has adopted the
/// tensor — the other half of the protocol pair.
const DLPACK_CONSUMED_CAPSULE_NAME: &CStr = c"used_dltensor";

/// A managed tensor over a fake pointer whose owner counts its drops.
fn counted_managed_tensor(drops: &Arc<AtomicUsize>) -> *mut ManagedTensor {
    struct CountedOwner(Arc<AtomicUsize>);
    impl Drop for CountedOwner {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    dlpack::build_managed_tensor(
        0xCAFE_F00D,
        vec![4, 4, 4],
        Some(vec![16, 4, 1]),
        DataType::U8,
        HOST_VISIBLE_DLPACK_DEVICE,
        Box::new(CountedOwner(Arc::clone(drops))),
    )
}

/// A capsule nobody consumed still owns its tensor, so collection must
/// free it — exactly once.
#[test]
fn an_unconsumed_capsule_frees_its_tensor_on_collection() {
    Python::initialize();
    let drops = Arc::new(AtomicUsize::new(0));
    Python::attach(|python| {
        let capsule = dlpack_capsule_from_managed_tensor(python, counted_managed_tensor(&drops))
            .expect("capsule");
        assert_eq!(
            drops.load(Ordering::SeqCst),
            0,
            "still owned by the capsule"
        );
        drop(capsule);
    });
    assert_eq!(
        drops.load(Ordering::SeqCst),
        1,
        "collection must run the deleter exactly once"
    );
}

/// Renaming to `used_dltensor` is how a consumer signals it has adopted
/// the tensor. Freeing it here as well would be a double free of memory
/// torch is still using.
#[test]
fn a_consumed_capsule_is_left_alone_by_the_destructor() {
    Python::initialize();
    let drops = Arc::new(AtomicUsize::new(0));
    let adopted = Python::attach(|python| {
        let capsule = dlpack_capsule_from_managed_tensor(python, counted_managed_tensor(&drops))
            .expect("capsule");
        // What every `from_dlpack` implementation does on adoption.
        let adopted = unsafe {
            let raw =
                pyo3::ffi::PyCapsule_GetPointer(capsule.as_ptr(), DLPACK_CAPSULE_NAME.as_ptr())
                    as *mut ManagedTensor;
            assert_eq!(
                pyo3::ffi::PyCapsule_SetName(
                    capsule.as_ptr(),
                    DLPACK_CONSUMED_CAPSULE_NAME.as_ptr(),
                ),
                0,
            );
            raw
        };
        drop(capsule);
        adopted
    });
    assert_eq!(
        drops.load(Ordering::SeqCst),
        0,
        "a consumed capsule's tensor belongs to its consumer"
    );
    // The consumer's own release.
    unsafe {
        let deleter = (*adopted).deleter.expect("deleter");
        deleter(adopted);
    }
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

/// An owner that, like every helper release debt, attaches and then
/// detaches for its blocking round trip.
struct OwnerThatDetachesOnDrop(Arc<AtomicUsize>);
impl Drop for OwnerThatDetachesOnDrop {
    fn drop(&mut self) {
        Python::attach(|python| python.detach(|| self.0.fetch_add(1, Ordering::SeqCst)));
    }
}

/// A consumed capsule from `dlpack_capsule_over`, handed back as the raw
/// managed tensor its consumer now owns.
fn adopted_managed_tensor_over_an_owner_that_detaches_on_drop(
    python: Python<'_>,
    drops: &Arc<AtomicUsize>,
) -> *mut ManagedTensor {
    let layout = PixelExchangeTensorLayout::for_pixel_format(PixelFormat::Rgba32, 4, 4, 16)
        .expect("an rgba layout");
    let capsule = dlpack_capsule_over(
        python,
        0xCAFE_F00D,
        layout,
        HOST_VISIBLE_DLPACK_DEVICE,
        DlpackExchangeShape::Unversioned,
        false,
        Box::new(OwnerThatDetachesOnDrop(Arc::clone(drops))),
    )
    .expect("capsule");
    unsafe {
        let adopted =
            pyo3::ffi::PyCapsule_GetPointer(capsule.as_ptr(), DLPACK_CAPSULE_NAME.as_ptr())
                as *mut ManagedTensor;
        assert_eq!(
            pyo3::ffi::PyCapsule_SetName(capsule.as_ptr(), DLPACK_CONSUMED_CAPSULE_NAME.as_ptr()),
            0,
        );
        adopted
    }
}

/// torch frees a tensor inside its own `gil_scoped_release`, on the very
/// thread pyo3 counts as attached for `process()`. The owner's drop must
/// not run Python, or detach, with that thread state released.
#[test]
fn a_consumer_releasing_the_tensor_with_the_thread_state_released_does_not_abort() {
    Python::initialize();
    let drops = Arc::new(AtomicUsize::new(0));
    Python::attach(|python| {
        let adopted = adopted_managed_tensor_over_an_owner_that_detaches_on_drop(python, &drops);
        unsafe {
            let released_thread_state = pyo3::ffi::PyEval_SaveThread();
            let deleter = (*adopted).deleter.expect("deleter");
            deleter(adopted);
            pyo3::ffi::PyEval_RestoreThread(released_thread_state);
        }
    });
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

/// A consumer may free the tensor on a thread of its own that has never
/// touched the interpreter.
#[test]
fn a_consumer_releasing_the_tensor_from_a_foreign_thread_does_not_abort() {
    Python::initialize();
    let drops = Arc::new(AtomicUsize::new(0));
    let adopted_address = Python::attach(|python| {
        adopted_managed_tensor_over_an_owner_that_detaches_on_drop(python, &drops) as usize
    });
    std::thread::spawn(move || unsafe {
        let adopted = adopted_address as *mut ManagedTensor;
        let deleter = (*adopted).deleter.expect("deleter");
        deleter(adopted);
    })
    .join()
    .expect("the foreign thread's release");
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}
