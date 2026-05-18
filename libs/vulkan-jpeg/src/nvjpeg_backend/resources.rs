// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! All the pre-allocated state the nvJPEG backend owns:
//!
//! - `libnvjpeg` handle + per-decoder state (resolved via `libloading`).
//! - One CUDA stream the decode + cudaMemcpy2DAsync work runs on.
//! - One [`TextureRing`] of `Rgba8Unorm` output textures (the consumer-
//!   visible result; matches the Vulkan-compute backend exactly).
//! - Per ring slot, two CUDA-side resources:
//!   - A CUDA-private `cudaMalloc` buffer sized `max_width * max_height *
//!     3` — the linear RGBI region nvJPEG writes into directly.
//!   - An OPAQUE_FD-exported DEVICE_LOCAL `VkBuffer` sized
//!     `max_width * max_height * 4` — pre-filled with `0xFF` (alpha=255
//!     in every pixel) and imported into CUDA via
//!     [`external_memory::import_external_memory_opaque_fd`] +
//!     [`external_memory::get_mapped_buffer`]. The shared region acts as
//!     the cross-API staging between CUDA's `cudaMemcpy2DAsync` and the
//!     host-side `vkCmdCopyBufferToImage`.
//!
//! Per-frame decode does no allocation: nvjpegDecode → cudaMemcpy2DAsync
//! (3bpp → 4bpp with stride trick, leaving the pre-filled alpha
//! byte at `0xFF`) → cudaStreamSynchronize → vkCmdCopyBufferToImage →
//! `submit_and_wait`. The CPU-side sync after cudaStreamSynchronize is
//! sufficient cross-API ordering for same-process / same-device CUDA-
//! Vulkan interop — no timeline semaphore is needed; the Vulkan-side
//! `submit_and_wait` mirrors the synchronous shape the Vulkan-compute
//! backend already uses.

use std::ffi::c_void;
use std::mem::MaybeUninit;
use std::os::unix::io::RawFd;
use std::sync::Arc;

use cudarc::runtime::result::external_memory;
use cudarc::runtime::sys;

use streamlib::sdk::context::{GpuContextFullAccess, TextureRing};
use streamlib::sdk::engine::host_rhi::{
    HostVulkanBuffer, HostVulkanDevice, ImageCopyRegion, RhiCommandRecorder, VulkanAccess,
    VulkanStage,
};
use streamlib::sdk::engine::HostGpuDeviceExt;
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::rhi::{Texture, TextureFormat, TextureUsages, VulkanLayout};

use crate::simple_decoder::{JpegDecodeOutput, MAX_FRAMES_IN_FLIGHT};

use super::ffi::{
    self, nvjpegHandle_t, nvjpegImage_t, nvjpegJpegState_t, nvjpegOutputFormat_t, NvJpegLib,
    NVJPEG_STATUS_SUCCESS,
};

/// Per ring slot's CUDA + Vulkan staging state.
struct NvJpegSlot {
    /// Consumer-visible output texture.
    texture: Texture,
    /// Same-process texture-cache id for the slot.
    surface_id: String,

    /// OPAQUE_FD-exported DEVICE_LOCAL `VkBuffer`, sized
    /// `max_width * max_height * 4`. Pre-filled with `0xFF` at
    /// construction so cudaMemcpy2DAsync's stride trick (writing 3 bytes
    /// per pixel) leaves the alpha byte at 255.
    shared_buffer: Arc<HostVulkanBuffer>,

    /// CUDA-side import handle. Lifetime: lives until [`Self::destroy`]
    /// drops it before the Vulkan-side `shared_buffer` Arc drops. The
    /// drop order matters — CUDA must release its hold on the kernel FD
    /// before Vulkan frees the underlying VkDeviceMemory.
    cuda_ext_mem: sys::cudaExternalMemory_t,
    /// Mapped CUDA device pointer aliasing the same kernel memory as
    /// `shared_buffer`. Stable for the slot's lifetime.
    cuda_shared_dev_ptr: u64,

    /// CUDA-private RGBI staging buffer (`cudaMalloc`-allocated). nvJPEG
    /// writes interleaved-RGB pixels here at pitch `width * 3`; the
    /// subsequent `cudaMemcpy2DAsync` repitches the rows into
    /// `shared_buffer`. Sized `max_width * max_height * 3` at
    /// construction; freed via `cudaFree` in [`Self::destroy`].
    cuda_rgbi_ptr: u64,
}

/// SAFETY: `cudaExternalMemory_t` is an opaque pointer-shaped handle.
/// The CUDA Runtime API's threading contract permits creation + use +
/// teardown from any thread, with the application responsible for not
/// concurrently using and destroying the same handle. Our enclosing
/// `NvJpegResources` is `&mut self` on every per-frame call, so no
/// concurrent use is possible.
unsafe impl Send for NvJpegSlot {}

impl NvJpegSlot {
    /// Tear down the slot's CUDA-side resources in the correct order:
    /// `cudaExternalMemory` (releases the OPAQUE_FD kernel hold) and
    /// the cudaMalloc'd RGBI buffer. Vulkan-side `shared_buffer`'s
    /// `Arc<HostVulkanBuffer>` drops at the end (Rust drop order),
    /// returning the underlying VkDeviceMemory to VMA's pool.
    fn destroy(&mut self) {
        unsafe {
            if self.cuda_rgbi_ptr != 0 {
                let _ = sys::cudaFree(self.cuda_rgbi_ptr as *mut c_void).result();
                self.cuda_rgbi_ptr = 0;
            }
            if !self.cuda_ext_mem.is_null() {
                let _ = external_memory::destroy_external_memory(self.cuda_ext_mem);
                self.cuda_ext_mem = std::ptr::null_mut();
            }
        }
    }
}

impl Drop for NvJpegSlot {
    fn drop(&mut self) {
        self.destroy();
    }
}

/// Backend-internal state owning the nvJPEG handle, CUDA stream, and
/// per-slot CUDA + Vulkan staging.
pub(super) struct NvJpegResources {
    lib: NvJpegLib,
    nvjpeg_handle: nvjpegHandle_t,
    nvjpeg_state: nvjpegJpegState_t,
    stream: sys::cudaStream_t,

    /// Held to back the slots' `texture` references and to drop after
    /// every per-slot CUDA handle is torn down. Unused after construction.
    _ring: Arc<TextureRing>,

    slots: Vec<NvJpegSlot>,
    current_slot: usize,

    /// Device handle for per-frame `RhiCommandRecorder` construction.
    device: Arc<HostVulkanDevice>,
}

/// SAFETY: every field's threading contract is documented above.
/// `Arc<HostVulkanDevice>` is `Send + Sync` already. The CUDA-side
/// handles (`nvjpegHandle_t`, `nvjpegJpegState_t`, `cudaStream_t`) are
/// pointer-shaped — CUDA permits use from any thread once created.
/// We never share a `NvJpegResources` across threads concurrently; the
/// backend is owned by a single `SimpleJpegDecoder` and every per-frame
/// call goes through `&mut self`.
unsafe impl Send for NvJpegResources {}

impl NvJpegResources {
    /// Bring up the nvJPEG handle + per-slot CUDA + Vulkan resources.
    /// See module doc for the allocation shape. Construction is
    /// privileged (FullAccess); steady-state per-frame work is not.
    pub(super) fn new(
        full_access: &GpuContextFullAccess,
        max_width: u32,
        max_height: u32,
    ) -> Result<Self> {
        let lib = NvJpegLib::load()?;
        let device = Arc::clone(full_access.device().vulkan_device());

        // ── Bind CUDA to the matching physical device ──────────────────
        // Match the Vulkan-selected device's `VkPhysicalDeviceIDProperties::deviceUUID`
        // against each CUDA device's `cudaDeviceProp::uuid` and call
        // `cudaSetDevice` with the matching ordinal. On a single-GPU host
        // this resolves to ordinal 0 either way; on multi-GPU rigs without
        // the match, CUDA's default device may differ from the Vulkan-
        // selected one and the OPAQUE_FD import below would land on the
        // wrong GPU. Falls back to ordinal 0 with a warn log when no CUDA
        // device's UUID matches (defensive — keeps single-GPU behavior
        // even if the probe fails).
        let vulkan_uuid = device.physical_device_uuid();
        let cuda_ordinal = match_cuda_device_to_vulkan_uuid(&vulkan_uuid).unwrap_or_else(|| {
            tracing::warn!(
                target: "vulkan_jpeg::cuda_device_match",
                vulkan_uuid = ?vulkan_uuid,
                "no CUDA device matches Vulkan device UUID; falling back to CUDA ordinal 0 \
                 (single-GPU hosts unaffected; multi-GPU may decode on the wrong device)",
            );
            0
        });
        unsafe {
            sys::cudaSetDevice(cuda_ordinal).result().map_err(|e| {
                Error::GpuError(format!("cudaSetDevice({cuda_ordinal}): {e:?}"))
            })?;
        }

        // ── CUDA stream ────────────────────────────────────────────────
        let stream = unsafe {
            let mut stream = MaybeUninit::<sys::cudaStream_t>::uninit();
            sys::cudaStreamCreate(stream.as_mut_ptr())
                .result()
                .map_err(|e| Error::GpuError(format!("cudaStreamCreate: {e:?}")))?;
            stream.assume_init()
        };

        // ── nvJPEG handle + state ──────────────────────────────────────
        let mut nvjpeg_handle: nvjpegHandle_t = std::ptr::null_mut();
        let status = unsafe { (lib.create_simple)(&mut nvjpeg_handle) };
        if status != NVJPEG_STATUS_SUCCESS {
            unsafe {
                let _ = sys::cudaStreamDestroy(stream).result();
            }
            return Err(ffi::status_to_error("nvjpegCreateSimple", status));
        }

        let mut nvjpeg_state: nvjpegJpegState_t = std::ptr::null_mut();
        let status = unsafe { (lib.state_create)(nvjpeg_handle, &mut nvjpeg_state) };
        if status != NVJPEG_STATUS_SUCCESS {
            unsafe {
                let _ = (lib.destroy)(nvjpeg_handle);
                let _ = sys::cudaStreamDestroy(stream).result();
            }
            return Err(ffi::status_to_error("nvjpegJpegStateCreate", status));
        }

        // ── Output texture ring (consumer-visible Rgba8Unorm) ──────────
        // GENERAL layout — same shape as VulkanComputeBackend's ring,
        // because `vkCmdCopyBufferToImage` accepts GENERAL as the dst
        // layout (suboptimal vs TRANSFER_DST_OPTIMAL but allows zero
        // per-frame barriers).
        let ring = full_access.create_texture_ring(
            max_width,
            max_height,
            TextureFormat::Rgba8Unorm,
            TextureUsages::COPY_DST | TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_SRC,
            MAX_FRAMES_IN_FLIGHT,
        )?;

        for slot_index in 0..ring.len() {
            let slot = ring
                .slot(slot_index)
                .ok_or_else(|| Error::GpuError("ring slot index out of range".into()))?;
            let mut recorder =
                RhiCommandRecorder::new(&device, "nvjpeg_backend_init_layout")?;
            recorder.begin()?;
            recorder.record_image_barrier(
                &slot.texture,
                VulkanLayout::UNDEFINED,
                VulkanLayout::GENERAL,
                VulkanStage::TOP_OF_PIPE,
                VulkanStage::ALL_TRANSFER,
                VulkanAccess::NONE,
                VulkanAccess::TRANSFER_WRITE,
            )?;
            recorder.submit_and_wait()?;
            full_access
                .update_texture_registration_layout(&slot.surface_id, VulkanLayout::GENERAL);
        }

        // ── Per-slot OPAQUE_FD staging + CUDA imports ──────────────────
        let shared_size_bytes: usize = (max_width as usize)
            .checked_mul(max_height as usize)
            .and_then(|v| v.checked_mul(4))
            .ok_or_else(|| {
                Error::GpuError(format!(
                    "NvJpegBackend: shared buffer size overflow at {}x{}",
                    max_width, max_height,
                ))
            })?;
        let rgbi_size_bytes: usize = (max_width as usize)
            .checked_mul(max_height as usize)
            .and_then(|v| v.checked_mul(3))
            .ok_or_else(|| {
                Error::GpuError(format!(
                    "NvJpegBackend: RGBI buffer size overflow at {}x{}",
                    max_width, max_height,
                ))
            })?;

        let mut slots: Vec<NvJpegSlot> = Vec::with_capacity(ring.len());
        let mut construction_err: Option<Error> = None;

        for slot_index in 0..ring.len() {
            let ring_slot = match ring.slot(slot_index) {
                Some(s) => s,
                None => {
                    construction_err =
                        Some(Error::GpuError("ring slot index out of range".into()));
                    break;
                }
            };

            let result = build_slot(
                &device,
                &ring_slot.texture,
                &ring_slot.surface_id,
                shared_size_bytes,
                rgbi_size_bytes,
                stream,
            );
            match result {
                Ok(slot) => slots.push(slot),
                Err(e) => {
                    construction_err = Some(e);
                    break;
                }
            }
        }

        if let Some(e) = construction_err {
            // Tear down whatever we got far enough on. `slots`'s
            // `Drop` runs CUDA teardown; we still need to drop nvJPEG
            // + stream.
            drop(slots);
            unsafe {
                let _ = (lib.state_destroy)(nvjpeg_state);
                let _ = (lib.destroy)(nvjpeg_handle);
                let _ = sys::cudaStreamDestroy(stream).result();
            }
            return Err(e);
        }

        Ok(Self {
            lib,
            nvjpeg_handle,
            nvjpeg_state,
            stream,
            _ring: ring,
            slots,
            current_slot: 0,
            device,
        })
    }

    /// Per-frame decode. Steady-state Limited-safe (no allocation, no
    /// pipeline / fence creation; one cudaMemcpy2DAsync + one short
    /// Vulkan submit per frame).
    pub(super) fn decode(
        &mut self,
        jpeg_bytes: &[u8],
        max_width: u32,
        max_height: u32,
    ) -> Result<JpegDecodeOutput> {
        // Parse the JPEG headers on the CPU to extract dimensions + color
        // metadata; nvJPEG re-parses internally on the GPU side. The CPU
        // parser also runs Huffman entropy decode whose result is unused
        // on this path — wasted ~1-3 ms per frame on a modern CPU, worth
        // it for color-info reporting parity with the Vulkan-compute
        // backend. A future optimization can extract a parser-only path.
        let decoded = crate::decode(jpeg_bytes)
            .map_err(|e| Error::GpuError(format!("jpeg parse/huffman: {e}")))?;

        let width = u32::from(decoded.frame.width);
        let height = u32::from(decoded.frame.height);
        if width > max_width || height > max_height {
            return Err(Error::GpuError(format!(
                "NvJpegBackend::decode: frame {}x{} exceeds decoder maxima {}x{} \
                 (rebuild SimpleJpegDecoder with larger max_width/max_height)",
                width, height, max_width, max_height,
            )));
        }

        let resolved = decoded
            .color_info
            .resolve()
            .map_err(|e| Error::GpuError(format!("jpeg colorimetry: {e}")))?;

        // Rotate ring slot.
        let slot_idx = self.current_slot;
        self.current_slot = (self.current_slot + 1) % self.slots.len();
        let slot = &self.slots[slot_idx];

        // ── nvJPEG decode into the slot's CUDA-private RGBI buffer ──────
        let mut nvjpeg_image = nvjpegImage_t::default();
        nvjpeg_image.channel[0] = slot.cuda_rgbi_ptr as *mut u8;
        nvjpeg_image.pitch[0] = (width as usize) * 3;
        let status = unsafe {
            (self.lib.decode)(
                self.nvjpeg_handle,
                self.nvjpeg_state,
                jpeg_bytes.as_ptr(),
                jpeg_bytes.len(),
                nvjpegOutputFormat_t::Rgbi,
                &mut nvjpeg_image,
                self.stream as *mut c_void,
            )
        };
        if status != NVJPEG_STATUS_SUCCESS {
            return Err(ffi::status_to_error("nvjpegDecode", status));
        }

        // ── cudaMemcpy2DAsync RGBI → shared OPAQUE_FD with alpha-padding ─
        //
        // The stride trick: cudaMemcpy2D treats each pixel as a "row":
        //   - `width = 3` bytes per row (the 3 RGB bytes of one pixel)
        //   - `height = width * height` rows (one per pixel)
        //   - `spitch = 3` (advance source by 3 bytes after each pixel)
        //   - `dpitch = 4` (advance destination by 4 bytes after each
        //     pixel, leaving the 4th byte at the pre-fill 0xFF)
        // This expands 3-channel RGBI into 4-channel `Rgba8Unorm` layout
        // in-place — every output pixel reads `[R, G, B, 0xFF]`. The
        // destination is laid out tightly at `actual_width * actual_height
        // * 4` bytes; the Vulkan copy-buffer-to-image below uses
        // `buffer_row_length = width` to match.
        let total_pixels = (width as usize)
            .checked_mul(height as usize)
            .ok_or_else(|| {
                Error::GpuError(format!(
                    "NvJpegBackend::decode: pixel count overflow at {}x{}",
                    width, height
                ))
            })?;
        unsafe {
            sys::cudaMemcpy2DAsync(
                slot.cuda_shared_dev_ptr as *mut c_void,
                4,
                slot.cuda_rgbi_ptr as *const c_void,
                3,
                3,
                total_pixels,
                sys::cudaMemcpyKind::cudaMemcpyDeviceToDevice,
                self.stream,
            )
            .result()
            .map_err(|e| {
                Error::GpuError(format!("cudaMemcpy2DAsync RGBI → shared: {e:?}"))
            })?;
        }

        // ── CPU-side sync. After this returns, all CUDA writes are
        // committed and visible to the subsequent Vulkan submit on the
        // same physical device. Same-process / same-device CUDA-Vulkan
        // interop doesn't require an external semaphore; the CPU-
        // mediated ordering (`cudaStreamSynchronize` returns →
        // `vkQueueSubmit` issues) is sufficient per CUDA and Vulkan
        // specs. ─────────────────────────────────────────────────────────
        unsafe {
            sys::cudaStreamSynchronize(self.stream).result().map_err(|e| {
                Error::GpuError(format!("cudaStreamSynchronize: {e:?}"))
            })?;
        }

        // ── Vulkan vkCmdCopyBufferToImage shared buffer → ring slot ─────
        let mut recorder = RhiCommandRecorder::new(&self.device, "nvjpeg_copy_to_image")?;
        recorder.begin()?;
        recorder.record_buffer_barrier(
            slot.shared_buffer.as_ref(),
            VulkanStage::ALL_COMMANDS,
            VulkanStage::ALL_TRANSFER,
            VulkanAccess::MEMORY_WRITE,
            VulkanAccess::TRANSFER_READ,
        )?;
        recorder.record_copy_buffer_to_image(
            slot.shared_buffer.as_ref(),
            &slot.texture,
            VulkanLayout::GENERAL,
            ImageCopyRegion {
                width,
                height,
                buffer_offset: 0,
                buffer_row_length: width,
                buffer_image_height: height,
                mip_level: 0,
                array_layer: 0,
            },
        )?;
        recorder.submit_and_wait()?;

        Ok(JpegDecodeOutput {
            texture: slot.texture.clone(),
            surface_id: slot.surface_id.clone(),
            width,
            height,
            color_source: resolved.source,
            color_info: resolved.info,
        })
    }
}

impl Drop for NvJpegResources {
    fn drop(&mut self) {
        // Order matters:
        // 1. Drop slots first → CUDA imports release their OPAQUE_FD
        //    kernel holds.
        // 2. Drop nvJPEG state + handle next.
        // 3. Destroy CUDA stream.
        // 4. `_ring` drops with `self` → Vulkan teardown.
        self.slots.clear();
        unsafe {
            if !self.nvjpeg_state.is_null() {
                let _ = (self.lib.state_destroy)(self.nvjpeg_state);
                self.nvjpeg_state = std::ptr::null_mut();
            }
            if !self.nvjpeg_handle.is_null() {
                let _ = (self.lib.destroy)(self.nvjpeg_handle);
                self.nvjpeg_handle = std::ptr::null_mut();
            }
            if !self.stream.is_null() {
                let _ = sys::cudaStreamDestroy(self.stream).result();
                self.stream = std::ptr::null_mut();
            }
        }
    }
}

/// Build one ring slot's CUDA-side staging: OPAQUE_FD export from the
/// engine's pool, FD ownership transfer to CUDA via
/// `cudaImportExternalMemory`, pointer mapping, RGBI cudaMalloc, and
/// alpha pre-fill on the shared buffer.
///
/// FD ownership: `vkGetMemoryFdKHR` returns a fresh kernel FD; on
/// success in `import_external_memory_opaque_fd` ownership transfers
/// to CUDA. The error path closes the FD ourselves.
fn build_slot(
    device: &Arc<HostVulkanDevice>,
    texture: &Texture,
    surface_id: &str,
    shared_size_bytes: usize,
    rgbi_size_bytes: usize,
    stream: sys::cudaStream_t,
) -> Result<NvJpegSlot> {
    // Vulkan-side OPAQUE_FD buffer.
    let shared_buffer = Arc::new(HostVulkanBuffer::new_opaque_fd_export_device_local(
        device,
        shared_size_bytes as u64,
    )?);

    // Export FD → ownership transfers to CUDA on success.
    let fd: RawFd = shared_buffer.export_opaque_fd_memory()?;
    let cuda_ext_mem = unsafe {
        match external_memory::import_external_memory_opaque_fd(fd, shared_size_bytes as u64)
        {
            Ok(m) => m,
            Err(e) => {
                // FD ownership stays with us on error — close it.
                libc::close(fd);
                return Err(Error::GpuError(format!(
                    "cudaImportExternalMemory(OPAQUE_FD): {e:?}"
                )));
            }
        }
    };
    let cuda_shared_dev_ptr = unsafe {
        match external_memory::get_mapped_buffer(cuda_ext_mem, 0, shared_size_bytes as u64) {
            Ok(p) => p as u64,
            Err(e) => {
                let _ = external_memory::destroy_external_memory(cuda_ext_mem);
                return Err(Error::GpuError(format!(
                    "cudaExternalMemoryGetMappedBuffer: {e:?}"
                )));
            }
        }
    };

    // CUDA-private RGBI buffer.
    let mut cuda_rgbi_raw: *mut c_void = std::ptr::null_mut();
    unsafe {
        sys::cudaMalloc(&mut cuda_rgbi_raw, rgbi_size_bytes).result().map_err(|e| {
            // Clean up the shared mapping before bailing.
            let _ = external_memory::destroy_external_memory(cuda_ext_mem);
            Error::GpuError(format!("cudaMalloc RGBI ({rgbi_size_bytes} bytes): {e:?}"))
        })?;
    }
    let cuda_rgbi_ptr = cuda_rgbi_raw as u64;

    // Pre-fill the shared buffer with 0xFF (alpha=255). Issued on the
    // shared stream so it serializes with subsequent decode work.
    unsafe {
        sys::cudaMemsetAsync(
            cuda_shared_dev_ptr as *mut c_void,
            0xFF,
            shared_size_bytes,
            stream,
        )
        .result()
        .map_err(|e| {
            let _ = sys::cudaFree(cuda_rgbi_raw).result();
            let _ = external_memory::destroy_external_memory(cuda_ext_mem);
            Error::GpuError(format!("cudaMemsetAsync alpha pre-fill: {e:?}"))
        })?;
        // Block until the pre-fill commits so no decode races it.
        sys::cudaStreamSynchronize(stream).result().map_err(|e| {
            let _ = sys::cudaFree(cuda_rgbi_raw).result();
            let _ = external_memory::destroy_external_memory(cuda_ext_mem);
            Error::GpuError(format!("cudaStreamSynchronize (pre-fill): {e:?}"))
        })?;
    }

    Ok(NvJpegSlot {
        texture: texture.clone(),
        surface_id: surface_id.to_string(),
        shared_buffer,
        cuda_ext_mem,
        cuda_shared_dev_ptr,
        cuda_rgbi_ptr,
    })
}

/// Match `vulkan_uuid` (`VkPhysicalDeviceIDProperties::deviceUUID`)
/// against every CUDA device's `cudaDeviceProp::uuid` and return the
/// matching ordinal, or `None` when no CUDA device shares the UUID
/// (or CUDA isn't available).
///
/// Defensive helper for the multi-GPU case: CUDA's default device may
/// not be the same physical device Vulkan selected, in which case
/// every `cudaImportExternalMemory(OPAQUE_FD)` call lands on the
/// wrong device and decode silently writes to the wrong GPU. Callers
/// pair this with `cudaSetDevice(matched_ordinal)` before any other
/// CUDA work. Returns `None` when CUDA can't be queried — caller
/// falls back to ordinal 0 (single-GPU behavior) with a warn log.
///
/// Duplicated in `streamlib-python-native::cuda` and
/// `streamlib-deno-native::cuda` — keep the three in sync. A future
/// refactor lifts this to a shared utility crate; today's
/// duplication is cheap enough that the engine/library/cdylib dep
/// graph doesn't need to grow yet.
fn match_cuda_device_to_vulkan_uuid(vulkan_uuid: &[u8; 16]) -> Option<i32> {
    let mut count: i32 = 0;
    if unsafe { sys::cudaGetDeviceCount(&mut count) }
        .result()
        .is_err()
    {
        return None;
    }
    for ordinal in 0..count {
        let mut props = MaybeUninit::<sys::cudaDeviceProp>::uninit();
        // SAFETY: `cudaGetDeviceProperties_v2` fully initializes the
        // out-pointer on success. On error we skip the ordinal.
        let ok = unsafe { sys::cudaGetDeviceProperties_v2(props.as_mut_ptr(), ordinal) }
            .result();
        if ok.is_err() {
            continue;
        }
        let props = unsafe { props.assume_init() };
        // `cudaDeviceProp::uuid` is `cudaUUID_t` which is
        // `#[repr(C)] struct { bytes: [c_char; 16] }`. Transmute to a
        // plain `[u8; 16]` for comparison against Vulkan's UUID bytes.
        let cuda_uuid_bytes: [u8; 16] =
            unsafe { std::mem::transmute::<sys::cudaUUID_t, [u8; 16]>(props.uuid) };
        if cuda_uuid_bytes == *vulkan_uuid {
            return Some(ordinal);
        }
    }
    None
}
