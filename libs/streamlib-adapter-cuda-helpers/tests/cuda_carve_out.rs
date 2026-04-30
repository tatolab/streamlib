// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! CUDA carve-out semantic test for `streamlib-adapter-cuda` (#587).
//!
//! Validates the foundation primitive every dependent piece of CUDA
//! interop work rides on:
//!
//! 1. The host allocates an OPAQUE_FD-exportable HOST_VISIBLE
//!    `VkBuffer` via `HostVulkanPixelBuffer::new_opaque_fd_export`
//!    (new in #587) and a Vulkan timeline semaphore via
//!    `HostVulkanTimelineSemaphore::new_exportable`.
//! 2. The host registers the surface with `CudaSurfaceAdapter`, writes
//!    a known BGRA pattern through the mapped pointer, and signals the
//!    timeline on guard drop (timeline value 1).
//! 3. The host exports an OPAQUE_FD for the buffer's
//!    `VkDeviceMemory` and an OPAQUE_FD for the timeline.
//! 4. CUDA imports both via `cudaImportExternalMemory` /
//!    `cudaImportExternalSemaphore`, waits on the timeline at
//!    value 1, runs a `cudaMemcpyAsync` device→host into a CPU
//!    buffer, and asserts byte-equal against the source pattern.
//!
//! This is the simplest test that proves the OPAQUE_FD primitive
//! works end-to-end. VkImage interop (with `cudaExternalMemoryGetMappedMipmappedArray`)
//! is deferred to #588 — it requires the wire-format extension to
//! carry full `VkImageCreateInfo` round-trip and a CUDA texture
//! object to handle tile-aware reads. The buffer-flavored primitive
//! tested here is sufficient for the scaffold.
//!
//! Test gating:
//! - `#[cfg(feature = "cuda")]` — the test is only compiled when the
//!   `cuda` feature on `streamlib-adapter-cuda-helpers` is enabled.
//!   Default-OFF keeps `cargo test` working for contributors without
//!   `libcuda.so`.
//! - `cudarc::runtime::sys::is_culib_present()` — runtime probe for
//!   the CUDA driver. The test prints a skip message and returns
//!   when libcudart can't be dlopen'd. Combined with the build-time
//!   feature gate, this lets the test compile on a CUDA-less builder
//!   and skip cleanly on a CUDA-less runner.
//! - `#[serial]` — same `VkInstance` / `VkDevice` discipline as
//!   the cpu-readback helper's carve-out (NVIDIA dual-device crash).

#![cfg(all(target_os = "linux", feature = "cuda"))]

use std::mem::MaybeUninit;
use std::sync::Arc;

use serial_test::serial;
use streamlib::core::context::GpuContext;
use streamlib::core::rhi::PixelFormat;
use streamlib::host_rhi::{
    HostVulkanDevice, HostVulkanPixelBuffer, HostVulkanTimelineSemaphore,
};
use streamlib_adapter_abi::{
    StreamlibSurface, SurfaceFormat, SurfaceSyncState, SurfaceTransportHandle, SurfaceUsage,
};
use streamlib_adapter_cuda::{CudaSurfaceAdapter, HostSurfaceRegistration, VulkanLayout};

const W: u32 = 32;
const H: u32 = 32;
const BPP: u32 = 4;
const SURFACE_ID: u64 = 0xCDA0_0001;

fn try_init_gpu() -> Option<Arc<GpuContext>> {
    let _ = tracing_subscriber::fmt()
        .with_test_writer()
        .with_env_filter("streamlib_adapter_cuda=debug,streamlib=warn")
        .try_init();
    GpuContext::init_for_platform_sync().ok().map(Arc::new)
}

#[test]
#[serial]
fn host_buffer_to_cuda_byte_equal_round_trip() {
    use cudarc::runtime::result::external_memory;
    use cudarc::runtime::sys;

    // ── Phase 0: skip if Vulkan or CUDA not available ──────────────
    let Some(gpu) = try_init_gpu() else {
        println!("cuda carve-out: no Vulkan device — skipping");
        return;
    };
    let host_device: Arc<HostVulkanDevice> = Arc::clone(gpu.device().vulkan_device());
    if host_device.opaque_fd_buffer_pool().is_none() {
        println!(
            "cuda carve-out: OPAQUE_FD buffer pool unavailable — driver doesn't support \
             external memory; skipping"
        );
        return;
    }
    if !unsafe { sys::is_culib_present() } {
        println!("cuda carve-out: libcudart not present — skipping (CUDA toolkit absent)");
        return;
    }

    // Pick GPU 0 — the test rig assumption (UUID matching across
    // multi-GPU rigs is #588's concern).
    let device_set = unsafe { sys::cudaSetDevice(0) }.result();
    if let Err(e) = device_set {
        println!("cuda carve-out: cudaSetDevice(0) failed: {e:?} — skipping");
        return;
    }

    // ── Phase 1: host allocates OPAQUE_FD-exportable pixel buffer ──
    let pixel_buffer = match HostVulkanPixelBuffer::new_opaque_fd_export(
        &host_device,
        W,
        H,
        BPP,
        PixelFormat::Bgra32,
    ) {
        Ok(b) => Arc::new(b),
        Err(e) => {
            println!("cuda carve-out: new_opaque_fd_export failed: {e} — skipping");
            return;
        }
    };
    let timeline = match HostVulkanTimelineSemaphore::new_exportable(host_device.device(), 0) {
        Ok(s) => Arc::new(s),
        Err(e) => {
            println!("cuda carve-out: timeline new_exportable failed: {e} — skipping");
            return;
        }
    };
    let buffer_size = pixel_buffer.size() as usize;
    assert_eq!(
        buffer_size,
        (W * H * BPP) as usize,
        "buffer size matches W*H*BPP"
    );

    // ── Phase 2: register with the adapter, write a pattern through
    //    the host adapter's acquire_write, signal timeline (= 1) on
    //    guard drop ─────────────────────────────────────────────────
    let adapter = Arc::new(CudaSurfaceAdapter::new(Arc::clone(&host_device)));
    adapter
        .register_host_surface(
            SURFACE_ID,
            HostSurfaceRegistration {
                pixel_buffer: Arc::clone(&pixel_buffer),
                timeline: Arc::clone(&timeline),
                initial_layout: VulkanLayout::UNDEFINED,
            },
        )
        .expect("register_host_surface");
    let surface = StreamlibSurface::new(
        SURFACE_ID,
        W,
        H,
        SurfaceFormat::Bgra8,
        SurfaceUsage::SAMPLED,
        SurfaceTransportHandle::empty(),
        SurfaceSyncState::default(),
    );

    let pattern: Vec<u8> = (0..buffer_size)
        .map(|i| ((i * 31) & 0xFF) as u8)
        .collect();

    // Take the write acquire to exercise the trait, then write through
    // the underlying mapped pointer (the view doesn't expose mapped
    // bytes yet — that's the cuda-typed view work in #589/#590).
    {
        use streamlib_adapter_abi::SurfaceAdapter as _;
        let _wguard = adapter
            .acquire_write(&surface)
            .expect("host acquire_write");
        // SAFETY: `pixel_buffer` is HOST_VISIBLE | HOST_COHERENT and the
        // mapped pointer stays valid for the buffer's lifetime; we hold
        // an Arc through the entire test. Single-writer discipline is
        // enforced by the SurfaceAdapter trait — we hold the write
        // guard for the duration of this block.
        unsafe {
            let dst = pixel_buffer.mapped_ptr();
            std::ptr::copy_nonoverlapping(pattern.as_ptr(), dst, buffer_size);
        }
        // Drop on scope exit advances the timeline to 1 via signal_host.
    }

    // Sanity: re-acquire read to confirm the host adapter sees the
    // pattern. The acquire waits on timeline=1 (already signaled)
    // and returns immediately.
    {
        use streamlib_adapter_abi::SurfaceAdapter as _;
        let _rguard = adapter
            .acquire_read(&surface)
            .expect("host acquire_read sanity");
        let host_view = unsafe {
            std::slice::from_raw_parts(pixel_buffer.mapped_ptr(), buffer_size)
        };
        assert_eq!(
            host_view, pattern,
            "host adapter's mapped pointer must observe the pattern post-acquire-write"
        );
    }
    // After the read acquire+release the timeline has advanced to 2.
    // CUDA waits on value 1 below, which is unconditionally past.

    // ── Phase 3: export OPAQUE_FDs from the registered surface ─────
    // Round-trip through the adapter's registry accessors (rather than
    // the local Arcs) so this also exercises `surface_pixel_buffer` /
    // `surface_timeline` — the production cdylib path will read FDs
    // out of the registered surface, not from a local handle. Returning
    // `None` here means the registry forgot the surface, which would be
    // a regression in `register_host_surface`.
    let registered_pixel_buffer = adapter
        .surface_pixel_buffer(SURFACE_ID)
        .expect("CudaSurfaceAdapter::surface_pixel_buffer must return registered buffer");
    let registered_timeline = adapter
        .surface_timeline(SURFACE_ID)
        .expect("CudaSurfaceAdapter::surface_timeline must return registered timeline");
    assert!(
        Arc::ptr_eq(&registered_pixel_buffer, &pixel_buffer),
        "registry-returned pixel_buffer Arc must point at the originally-registered buffer"
    );
    assert!(
        Arc::ptr_eq(&registered_timeline, &timeline),
        "registry-returned timeline Arc must point at the originally-registered timeline"
    );
    let memory_fd = registered_pixel_buffer
        .export_opaque_fd_memory()
        .expect("HostVulkanPixelBuffer::export_opaque_fd_memory");
    let timeline_fd = registered_timeline
        .export_opaque_fd()
        .expect("HostVulkanTimelineSemaphore::export_opaque_fd");

    // ── Phase 4: import into CUDA, wait on timeline, memcpy d→h ────
    // SAFETY: CUDA Driver API contract per
    // `cudarc::runtime::result::external_memory::import_external_memory_opaque_fd`
    // and the cudaImportExternalMemory docs:
    //   - On UNIX, ownership of `memory_fd` transfers to the CUDA
    //     driver on successful import. We MUST NOT close the fd
    //     ourselves after this point (and the `pixel_buffer`'s
    //     in-process VkDeviceMemory remains owned by Vulkan; the FD
    //     is a separate kernel reference).
    //   - Same applies to `timeline_fd` after cudaImportExternalSemaphore.
    let ext_mem = unsafe {
        match external_memory::import_external_memory_opaque_fd(memory_fd, buffer_size as u64) {
            Ok(m) => m,
            Err(e) => {
                // FD ownership did NOT transfer on error — close it
                // to avoid leaking. Same for the timeline fd we
                // haven't tried to import yet.
                libc::close(memory_fd);
                libc::close(timeline_fd);
                panic!("cudaImportExternalMemory failed: {e:?}");
            }
        }
    };

    let dev_ptr = unsafe {
        match external_memory::get_mapped_buffer(ext_mem, 0, buffer_size as u64) {
            Ok(p) => p,
            Err(e) => {
                let _ = external_memory::destroy_external_memory(ext_mem);
                libc::close(timeline_fd);
                panic!("cudaExternalMemoryGetMappedBuffer failed: {e:?}");
            }
        }
    };

    // ── Phase 4a (Stage 8 of #588): probe the imported pointer's
    //    memory class. The cdylib that constructs DLPack capsules
    //    (#589/#590) needs to know whether to advertise the pointer
    //    as `kDLCUDA = 2` (real device memory; reads at device
    //    bandwidth) or `kDLCUDAHost = 3` (pinned-host memory; reads
    //    cross PCIe per access). CUDA's `cudaPointerGetAttributes`
    //    is the authoritative answer.
    //
    //    Expected: `cudaMemoryTypeDevice` for the imported HOST_VISIBLE
    //    OPAQUE_FD `VkBuffer`. If a future driver downgrades to
    //    `cudaMemoryTypeHost`, drop `HOST_VISIBLE` from
    //    `HostVulkanPixelBuffer::new_opaque_fd_export` and re-test —
    //    the host-side mapped pointer goes away (host-side population
    //    routes through `vkCmdCopyBuffer` instead) but device-side
    //    inference performance is preserved. The flip is recorded in
    //    `context.md` under "Open empirical question (Stage 8)".
    let mut ptr_attrs = MaybeUninit::<sys::cudaPointerAttributes>::uninit();
    let ptr_attrs_call = unsafe {
        sys::cudaPointerGetAttributes(ptr_attrs.as_mut_ptr(), dev_ptr).result()
    };
    if let Err(e) = ptr_attrs_call {
        let _ = unsafe { external_memory::destroy_external_memory(ext_mem) };
        unsafe { libc::close(timeline_fd) };
        panic!(
            "cudaPointerGetAttributes failed on imported OPAQUE_FD device \
             pointer: {e:?} — the Runtime API may not classify externally-\
             imported memory on this driver; investigate before proceeding \
             (#588 Stage 8, see context.md)"
        );
    }
    let ptr_attrs = unsafe { ptr_attrs.assume_init() };
    println!(
        "cuda carve-out: cudaPointerGetAttributes(dev_ptr) → \
         type={:?}, device={}, devicePointer={:?}, hostPointer={:?}",
        ptr_attrs.type_, ptr_attrs.device, ptr_attrs.devicePointer, ptr_attrs.hostPointer,
    );
    match ptr_attrs.type_ {
        sys::cudaMemoryType::cudaMemoryTypeDevice => {
            // Expected. The imported pointer is real device memory;
            // DLPack capsules can advertise `kDLCUDA = 2` and CUDA
            // kernels read at device bandwidth.
        }
        sys::cudaMemoryType::cudaMemoryTypeHost => {
            let _ = unsafe { external_memory::destroy_external_memory(ext_mem) };
            unsafe { libc::close(timeline_fd) };
            panic!(
                "Stage 8 regression (#588): imported OPAQUE_FD device pointer \
                 presents as `cudaMemoryTypeHost` (pinned-host, PCIe per \
                 access). Action: drop `HOST_VISIBLE` from \
                 `HostVulkanPixelBuffer::new_opaque_fd_export`'s memory-property \
                 mask, document the change in `context.md`, and update \
                 `streamlib-adapter-cuda::dlpack` so the cdylib advertises \
                 `kDLCUDAHost = 3` instead of `kDLCUDA = 2`."
            );
        }
        sys::cudaMemoryType::cudaMemoryTypeUnregistered => {
            let _ = unsafe { external_memory::destroy_external_memory(ext_mem) };
            unsafe { libc::close(timeline_fd) };
            panic!(
                "Stage 8 (#588): imported OPAQUE_FD device pointer is \
                 `cudaMemoryTypeUnregistered` — the Runtime API does not \
                 recognize the pointer at all, which would block DLPack \
                 hand-off to PyTorch / JAX. Investigate driver \
                 behavior before proceeding."
            );
        }
        sys::cudaMemoryType::cudaMemoryTypeManaged => {
            let _ = unsafe { external_memory::destroy_external_memory(ext_mem) };
            unsafe { libc::close(timeline_fd) };
            panic!(
                "Stage 8 (#588): imported OPAQUE_FD device pointer is \
                 `cudaMemoryTypeManaged` — unexpected for an externally-\
                 imported VkBuffer. Investigate driver before proceeding."
            );
        }
    }

    // Build the descriptor via `MaybeUninit::zeroed()` + raw-pointer
    // writes per field so the body is robust across `cuda-11040..=12090`
    // (no `reserved` field) and `cuda-13000..` (with `reserved: [u32;
    // 16]`). The naive `mem::zeroed()` shape originally shipped here
    // panics on modern Rust because `cudaExternalSemaphoreHandleType` is
    // a `#[repr(C)]` enum whose discriminant 0 is not a valid variant —
    // Rust's validity-invariant check at `mem::zeroed()` rejects an
    // all-zero bit pattern for the enclosing struct as a whole.
    // `MaybeUninit::zeroed()` skips that check (the type *might* not be
    // valid yet); we then overwrite `type_` to a valid variant and
    // `assume_init()` becomes sound. `handle` (a union; all-zero is a
    // valid bit pattern) and `flags` (c_uint; all-zero is 0) inherit the
    // `zeroed()` pre-fill, but we still write them explicitly to make
    // the construction self-documenting. `reserved` (cuda-13xxx only)
    // is `[c_uint; 16]` and stays at the all-zero pre-fill, matching
    // the CUDA spec's "reserved must be zero" contract. (#595)
    let mut sem_desc =
        std::mem::MaybeUninit::<sys::cudaExternalSemaphoreHandleDesc>::zeroed();
    let sem_desc = unsafe {
        let p = sem_desc.as_mut_ptr();
        (&raw mut (*p).type_).write(
            sys::cudaExternalSemaphoreHandleType::cudaExternalSemaphoreHandleTypeTimelineSemaphoreFd,
        );
        (&raw mut (*p).handle).write(
            sys::cudaExternalSemaphoreHandleDesc__bindgen_ty_1 { fd: timeline_fd },
        );
        (&raw mut (*p).flags).write(0);
        sem_desc.assume_init()
    };
    let mut ext_sem = MaybeUninit::<sys::cudaExternalSemaphore_t>::uninit();
    let import_sem_result = unsafe {
        sys::cudaImportExternalSemaphore(ext_sem.as_mut_ptr(), &sem_desc).result()
    };
    if let Err(e) = import_sem_result {
        let _ = unsafe { external_memory::destroy_external_memory(ext_mem) };
        unsafe { libc::close(timeline_fd) };
        panic!("cudaImportExternalSemaphore failed: {e:?}");
    }
    let ext_sem = unsafe { ext_sem.assume_init() };

    let mut stream = MaybeUninit::<sys::cudaStream_t>::uninit();
    unsafe { sys::cudaStreamCreate(stream.as_mut_ptr()) }
        .result()
        .expect("cudaStreamCreate");
    let stream = unsafe { stream.assume_init() };

    // Wait params for a TIMELINE semaphore: only `params.fence.value` is
    // meaningful; the unused union arms (nvSciSync, keyedMutex) and the
    // `reserved` arrays must be zero per the CUDA spec. The same
    // `MaybeUninit::zeroed()` pattern as `sem_desc` above keeps this
    // sound under modern Rust validity rules and cross-cuda-version-stable
    // (cuda-13xxx adds a top-level `reserved: [c_uint; 16]`). All field
    // writes go through raw pointers so we never form a `&mut` to an
    // intermediate not-yet-fully-initialised value. (#595)
    let mut wait_params =
        std::mem::MaybeUninit::<sys::cudaExternalSemaphoreWaitParams>::zeroed();
    let wait_params = unsafe {
        let p = wait_params.as_mut_ptr();
        (&raw mut (*p).params.fence.value).write(1);
        (&raw mut (*p).flags).write(0);
        wait_params.assume_init()
    };

    // The `_v2` suffixed name is the canonical extern in cuda
    // 11.4..12.9 bindings; the unsuffixed `cudaWaitExternalSemaphoresAsync`
    // is gated on cuda-13xxx. We pin to `cuda-12090` (see helpers
    // Cargo.toml) so the `_v2` symbol is the right one. The runtime
    // libcudart on cuda 13.x continues to export `_v2` for ABI
    // stability, so the dlopen path resolves.
    let wait_result = unsafe {
        sys::cudaWaitExternalSemaphoresAsync_v2(&ext_sem, &wait_params, 1, stream).result()
    };
    if let Err(e) = wait_result {
        let _ = unsafe { sys::cudaStreamDestroy(stream).result() };
        let _ = unsafe { sys::cudaDestroyExternalSemaphore(ext_sem).result() };
        let _ = unsafe { external_memory::destroy_external_memory(ext_mem) };
        panic!("cudaWaitExternalSemaphoresAsync failed: {e:?}");
    }

    let mut host_buf = vec![0u8; buffer_size];
    unsafe {
        sys::cudaMemcpyAsync(
            host_buf.as_mut_ptr() as *mut std::ffi::c_void,
            dev_ptr,
            buffer_size,
            sys::cudaMemcpyKind::cudaMemcpyDeviceToHost,
            stream,
        )
        .result()
        .expect("cudaMemcpyAsync DeviceToHost");
        sys::cudaStreamSynchronize(stream)
            .result()
            .expect("cudaStreamSynchronize");
    }

    // ── Phase 5: byte-equal assertion ──────────────────────────────
    assert_eq!(
        host_buf, pattern,
        "CUDA's view of the OPAQUE_FD-imported buffer must observe \
         the same bytes the host wrote through Vulkan's mapped pointer"
    );

    // ── Phase 6: cleanup ───────────────────────────────────────────
    unsafe {
        let _ = sys::cudaStreamDestroy(stream).result();
        let _ = sys::cudaDestroyExternalSemaphore(ext_sem).result();
        let _ = external_memory::destroy_external_memory(ext_mem);
    }
    // memory_fd and timeline_fd ownership transferred to CUDA on
    // successful import — do NOT close.
}
