// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! DLPack capsule construction for handing CUDA device pointers to
//! external frameworks (PyTorch / JAX / NumPy / CuPy) via
//! `from_dlpack` zero-copy.
//!
//! The crate re-exports `dlpark::ffi`'s `#[repr(C)]` ABI mirrors as the
//! canonical types — every consumer that speaks DLPack v0.8 or later
//! agrees on this struct layout, so layout-stability across the FFI
//! boundary that `streamlib-python-native` / `streamlib-deno-native`
//! cdylibs cross is guaranteed by the spec, pinned at our end by the
//! `=0.6.0` workspace lockfile and the layout regression test below.
//!
//! The construction helpers wrap a CUDA device pointer + caller-owned
//! state into a heap-allocated [`ManagedTensor`] whose `deleter` drops
//! the caller-owned state when the consumer (e.g. PyTorch) finishes
//! with the capsule. The helpers do NOT touch CUDA APIs; the cdylib
//! pulls the `CUdeviceptr` from `cudaExternalMemoryGetMappedBuffer`
//! and threads it through these helpers.

use std::any::Any;
use std::ffi::c_void;

pub use dlpark::ffi::{
    DataType, DataTypeCode, Device, DeviceType, ManagedTensor, ManagedTensorVersioned, Tensor,
};

/// Owner of any heap-allocated state the producer must keep alive
/// until the consumer calls the capsule deleter. `Box<dyn Any + Send>`
/// so the cdylib can pass an `Arc<...>` clone of the surface
/// registration / device handle / allocation tracker without this
/// crate naming those types.
pub type CapsuleOwner = Box<dyn Any + Send + 'static>;

/// Heap-allocated state the deleter reclaims. Stored behind
/// `ManagedTensor::manager_ctx`; the [`Tensor::shape`] and
/// [`Tensor::strides`] pointers reference the boxed slices here, so
/// this struct must outlive the [`ManagedTensor`] it owns.
struct ManagerCtx {
    _owner: CapsuleOwner,
    _shape: Box<[i64]>,
    _strides: Option<Box<[i64]>>,
}

unsafe extern "C" fn deleter(t: *mut ManagedTensor) {
    if t.is_null() {
        return;
    }
    let mt = unsafe { Box::from_raw(t) };
    if !mt.manager_ctx.is_null() {
        let ctx = unsafe { Box::from_raw(mt.manager_ctx as *mut ManagerCtx) };
        drop(ctx);
    }
    drop(mt);
}

/// Build a [`ManagedTensor`] over an existing device pointer.
///
/// `device_ptr` is a raw integer (typically a `CUdeviceptr` cast to
/// `u64`); the helper does NOT validate it. `owner` is heap-allocated
/// state the deleter will drop when the consumer releases the capsule
/// — typically an `Arc` clone of whatever ref-count keeps the backing
/// allocation alive in the cdylib.
///
/// The returned pointer is a `Box::into_raw` — the consumer takes
/// ownership and MUST eventually call the `deleter` to reclaim it.
/// Forgetting to call the deleter leaks the owner + shape + strides +
/// the `ManagedTensor` itself.
pub fn build_managed_tensor(
    device_ptr: u64,
    shape: Vec<i64>,
    strides: Option<Vec<i64>>,
    dtype: DataType,
    device: Device,
    owner: CapsuleOwner,
) -> *mut ManagedTensor {
    let shape: Box<[i64]> = shape.into_boxed_slice();
    let shape_ptr = shape.as_ptr() as *mut i64;
    let ndim = shape.len() as i32;

    let strides: Option<Box<[i64]>> = strides.map(Vec::into_boxed_slice);
    let strides_ptr: *mut i64 = match &strides {
        Some(s) => s.as_ptr() as *mut i64,
        None => std::ptr::null_mut(),
    };

    let ctx = Box::new(ManagerCtx {
        _owner: owner,
        _shape: shape,
        _strides: strides,
    });
    let ctx_ptr = Box::into_raw(ctx);

    let dl_tensor = Tensor {
        data: device_ptr as *mut c_void,
        device,
        ndim,
        dtype,
        shape: shape_ptr,
        strides: strides_ptr,
        byte_offset: 0,
    };
    let mt = Box::new(ManagedTensor {
        dl_tensor,
        manager_ctx: ctx_ptr as *mut c_void,
        deleter: Some(deleter),
    });
    Box::into_raw(mt)
}

/// Convenience: wrap a flat byte buffer at `device_ptr` as a 1-D
/// `u8` tensor on `device`. The default shape produced by
/// [`crate::CudaReadView::dlpack_managed_tensor`] /
/// [`crate::CudaWriteView::dlpack_managed_tensor`].
pub fn build_byte_buffer_managed_tensor(
    device_ptr: u64,
    size_bytes: u64,
    device: Device,
    owner: CapsuleOwner,
) -> *mut ManagedTensor {
    build_managed_tensor(
        device_ptr,
        vec![size_bytes as i64],
        None,
        DataType::U8,
        device,
        owner,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::{offset_of, size_of};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // -------------------------------------------------------------------
    // Layout regression — pins the DLPack v0.8 C ABI we ship across the
    // cdylib FFI boundary in #589/#590. All offsets/sizes are computed
    // for 64-bit Linux + macOS (the platforms our consumers run on);
    // an upstream `dlpark` change that adds a field or repacks a struct
    // would land here as a CI failure rather than a silent ABI drift.
    // -------------------------------------------------------------------

    #[test]
    fn dlpack_device_layout_matches_spec() {
        // typedef struct { DLDeviceType device_type; int32_t device_id; } DLDevice;
        assert_eq!(size_of::<Device>(), 8, "DLDevice is 2 × int32 = 8 bytes");
        assert_eq!(offset_of!(Device, device_type), 0);
        assert_eq!(offset_of!(Device, device_id), 4);
    }

    #[test]
    fn dlpack_device_type_discriminants_match_spec() {
        // DLPack v0.8 — values are part of the wire ABI.
        assert_eq!(DeviceType::Cpu as i32, 1);
        assert_eq!(DeviceType::Cuda as i32, 2, "kDLCUDA");
        assert_eq!(DeviceType::CudaHost as i32, 3, "kDLCUDAHost");
        assert_eq!(DeviceType::OpenCl as i32, 4);
        assert_eq!(DeviceType::Vulkan as i32, 7);
        assert_eq!(DeviceType::Metal as i32, 8);
        assert_eq!(DeviceType::Rocm as i32, 10);
        assert_eq!(DeviceType::CudaManaged as i32, 13);
    }

    #[test]
    fn dlpack_data_type_layout_matches_spec() {
        // typedef struct { uint8_t code; uint8_t bits; uint16_t lanes; } DLDataType;
        assert_eq!(size_of::<DataType>(), 4);
        assert_eq!(offset_of!(DataType, code), 0);
        assert_eq!(offset_of!(DataType, bits), 1);
        assert_eq!(offset_of!(DataType, lanes), 2);
    }

    #[test]
    fn dlpack_data_type_code_discriminants_match_spec() {
        assert_eq!(DataTypeCode::Int as u8, 0);
        assert_eq!(DataTypeCode::UInt as u8, 1);
        assert_eq!(DataTypeCode::Float as u8, 2);
        assert_eq!(DataTypeCode::OpaqueHandle as u8, 3);
        assert_eq!(DataTypeCode::Bfloat as u8, 4);
        assert_eq!(DataTypeCode::Complex as u8, 5);
        assert_eq!(DataTypeCode::Bool as u8, 6);
    }

    #[test]
    fn dlpack_tensor_layout_matches_spec_64bit() {
        // typedef struct {
        //   void*       data;          // offset 0,  size 8
        //   DLDevice    device;        // offset 8,  size 8
        //   int32_t     ndim;          // offset 16, size 4
        //   DLDataType  dtype;         // offset 20, size 4
        //   int64_t*    shape;         // offset 24, size 8
        //   int64_t*    strides;       // offset 32, size 8
        //   uint64_t    byte_offset;   // offset 40, size 8
        // } DLTensor;
        assert_eq!(size_of::<*mut c_void>(), 8, "64-bit pointer required");
        assert_eq!(offset_of!(Tensor, data), 0);
        assert_eq!(offset_of!(Tensor, device), 8);
        assert_eq!(offset_of!(Tensor, ndim), 16);
        assert_eq!(offset_of!(Tensor, dtype), 20);
        assert_eq!(offset_of!(Tensor, shape), 24);
        assert_eq!(offset_of!(Tensor, strides), 32);
        assert_eq!(offset_of!(Tensor, byte_offset), 40);
        assert_eq!(size_of::<Tensor>(), 48);
    }

    #[test]
    fn dlpack_managed_tensor_layout_matches_spec_64bit() {
        // typedef struct {
        //   DLTensor  dl_tensor;     // offset 0,  size 48
        //   void*     manager_ctx;   // offset 48, size 8
        //   void (*deleter)(...);    // offset 56, size 8
        // } DLManagedTensor;
        assert_eq!(offset_of!(ManagedTensor, dl_tensor), 0);
        assert_eq!(offset_of!(ManagedTensor, manager_ctx), 48);
        assert_eq!(offset_of!(ManagedTensor, deleter), 56);
        assert_eq!(size_of::<ManagedTensor>(), 64);
    }

    // -------------------------------------------------------------------
    // Behavioral tests for the helpers themselves.
    // -------------------------------------------------------------------

    #[test]
    fn build_byte_buffer_managed_tensor_round_trips_metadata() {
        let device_ptr = 0xDEAD_BEEF_CAFE_F00D_u64;
        let size = 1920 * 1080 * 4;
        let device = Device::cuda(0);
        let owner: CapsuleOwner = Box::new(());

        let mt_ptr = build_byte_buffer_managed_tensor(device_ptr, size, device, owner);
        assert!(!mt_ptr.is_null());

        unsafe {
            let mt = &*mt_ptr;
            assert_eq!(mt.dl_tensor.data as u64, device_ptr);
            assert_eq!(mt.dl_tensor.device, device);
            assert_eq!(mt.dl_tensor.ndim, 1);
            assert_eq!(mt.dl_tensor.dtype, DataType::U8);
            assert_eq!(*mt.dl_tensor.shape, size as i64);
            assert!(mt.dl_tensor.strides.is_null());
            assert_eq!(mt.dl_tensor.byte_offset, 0);
            assert!(!mt.manager_ctx.is_null());
            let del = mt.deleter.expect("deleter must be Some");
            del(mt_ptr);
        }
    }

    #[test]
    fn deleter_drops_owner_exactly_once() {
        // Drop counter on a custom owner type. The deleter must drop
        // it exactly once — not zero times (leak), not twice (UAF).
        struct Counted(Arc<AtomicUsize>);
        impl Drop for Counted {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }

        let drops = Arc::new(AtomicUsize::new(0));
        let owner: CapsuleOwner = Box::new(Counted(Arc::clone(&drops)));

        let mt = build_byte_buffer_managed_tensor(0xCAFE_F00D, 64, Device::cuda(0), owner);
        assert_eq!(drops.load(Ordering::SeqCst), 0, "owner not yet dropped");

        unsafe {
            let del = (*mt).deleter.expect("deleter must be Some");
            del(mt);
        }
        assert_eq!(drops.load(Ordering::SeqCst), 1, "owner dropped exactly once");
    }

    #[test]
    fn build_managed_tensor_with_explicit_strides_and_shape() {
        // 4-D BCHW tensor (1, 3, 480, 640) with row-major strides:
        // [3*480*640, 480*640, 640, 1]. Verifies multi-dim shape and
        // explicit strides round-trip through the helper.
        let shape = vec![1_i64, 3, 480, 640];
        let strides_vec = vec![3 * 480 * 640_i64, 480 * 640, 640, 1];
        let owner: CapsuleOwner = Box::new(());

        let mt = build_managed_tensor(
            0x1000_u64,
            shape.clone(),
            Some(strides_vec.clone()),
            DataType::F32,
            Device::cuda(0),
            owner,
        );
        unsafe {
            let t = &(*mt).dl_tensor;
            assert_eq!(t.ndim, 4);
            assert_eq!(t.dtype, DataType::F32);
            let observed_shape =
                std::slice::from_raw_parts(t.shape, t.ndim as usize).to_vec();
            assert_eq!(observed_shape, shape);
            assert!(!t.strides.is_null());
            let observed_strides =
                std::slice::from_raw_parts(t.strides, t.ndim as usize).to_vec();
            assert_eq!(observed_strides, strides_vec);

            let del = (*mt).deleter.expect("deleter must be Some");
            del(mt);
        }
    }

    #[test]
    fn deleter_tolerates_null_pointer() {
        // DLPack consumers occasionally guard against accidentally
        // double-freeing by zeroing the pointer; the deleter must
        // no-op on null rather than UB.
        unsafe { deleter(std::ptr::null_mut()) };
    }
}
