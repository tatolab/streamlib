// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// FFI cdylib — all public functions are unsafe extern "C" called from Deno via dlopen.
#![allow(clippy::missing_safety_doc)]

//! FFI cdylib for Deno subprocess processors to access iceoryx2 directly.
//!
//! Provides C ABI functions prefixed with `sldn_` that Deno loads via `Deno.dlopen()`.
//! This allows TypeScript processors to read/write iceoryx2 shared memory without
//! going through Rust host pipes (zero-copy data plane).

use std::collections::HashMap;
use std::ffi::{c_char, CStr};

use iceoryx2::port::publisher::Publisher;
use iceoryx2::port::subscriber::Subscriber;
use iceoryx2::prelude::*;
use streamlib_ipc_types::{FrameHeader, FRAME_HEADER_SIZE};

// ============================================================================
// Context
// ============================================================================

/// How frames should be read from an input port's buffer.
/// Mirrors the Rust-side `ReadMode` enum in `streamlib::iceoryx2::read_mode`.
const READ_MODE_SKIP_TO_LATEST: i32 = 0;
const READ_MODE_READ_NEXT_IN_ORDER: i32 = 1;

/// Per-processor native context holding iceoryx2 node and port state.
pub struct DenoNativeContext {
    processor_id: String,
    node: Node<ipc::Service>,
    subscribers: HashMap<String, SubscriberState>,
    publishers: HashMap<String, PublisherState>,
    /// Per-port read mode (port_name → READ_MODE_*). Default is SkipToLatest.
    port_read_modes: HashMap<String, i32>,
}

struct SubscriberState {
    subscriber: Subscriber<ipc::Service, [u8], ()>,
    /// Buffered payloads per port name (after poll).
    pending: HashMap<String, Vec<(Vec<u8>, i64)>>,
}

struct PublisherState {
    publisher: Publisher<ipc::Service, [u8], ()>,
    schema_name: String,
    dest_port: String,
}

impl DenoNativeContext {
    fn new(processor_id: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let node = NodeBuilder::new().create::<ipc::Service>()?;
        Ok(Self {
            processor_id: processor_id.to_string(),
            node,
            subscribers: HashMap::new(),
            publishers: HashMap::new(),
            port_read_modes: HashMap::new(),
        })
    }
}

// ============================================================================
// C ABI — Context lifecycle
// ============================================================================

/// Create a new native context for a Deno processor.
///
/// Returns an opaque pointer. Caller must call `sldn_context_destroy` when done.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sldn_context_create(
    processor_id: *const c_char,
) -> *mut DenoNativeContext {
    let id = if processor_id.is_null() {
        "unknown"
    } else {
        unsafe { CStr::from_ptr(processor_id) }.to_str().unwrap_or("unknown")
    };

    match DenoNativeContext::new(id) {
        Ok(ctx) => Box::into_raw(Box::new(ctx)),
        Err(e) => {
            tracing::error!("Failed to create context: {}", e);
            std::ptr::null_mut()
        }
    }
}

/// Destroy a native context, releasing all iceoryx2 resources.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sldn_context_destroy(ctx: *mut DenoNativeContext) {
    if !ctx.is_null() {
        let _ = unsafe { Box::from_raw(ctx) };
    }
}

/// Get current monotonic time in nanoseconds.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sldn_context_time_ns(_ctx: *const DenoNativeContext) -> i64 {
    let duration = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    duration.as_nanos() as i64
}

// ============================================================================
// C ABI — Input (subscribe + read)
// ============================================================================

/// Subscribe to an iceoryx2 service for reading data.
///
/// Returns 0 on success, -1 on failure.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sldn_input_subscribe(
    ctx: *mut DenoNativeContext,
    service_name: *const c_char,
) -> i32 {
    let ctx = match unsafe { ctx.as_mut() } {
        Some(c) => c,
        None => return -1,
    };
    let service_name = match unsafe { c_str_to_str(service_name) } {
        Some(s) => s,
        None => return -1,
    };

    let service_name_iox = match ServiceName::new(service_name) {
        Ok(n) => n,
        Err(e) => {
            tracing::error!(
                "[sldn:{}] Invalid service name '{}': {}",
                ctx.processor_id, service_name, e
            );
            return -1;
        }
    };

    let service = match ctx
        .node
        .service_builder(&service_name_iox)
        .publish_subscribe::<[u8]>()
        .max_publishers(16)
        .subscriber_max_buffer_size(16)
        .open_or_create()
    {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(
                "[sldn:{}] Failed to open service '{}': {}",
                ctx.processor_id, service_name, e
            );
            return -1;
        }
    };

    let subscriber = match service.subscriber_builder().buffer_size(16).create() {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(
                "[sldn:{}] Failed to create subscriber for '{}': {}",
                ctx.processor_id, service_name, e
            );
            return -1;
        }
    };

    ctx.subscribers.insert(
        service_name.to_string(),
        SubscriberState {
            subscriber,
            pending: HashMap::new(),
        },
    );

    0
}

/// Set the read mode for a specific input port.
///
/// `mode`: 0 = SkipToLatest (drain buffer, return newest — optimal for video),
///         1 = ReadNextInOrder (FIFO — required for audio).
///
/// Returns 0 on success, -1 on error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sldn_input_set_read_mode(
    ctx: *mut DenoNativeContext,
    port_name: *const c_char,
    mode: i32,
) -> i32 {
    let ctx = match unsafe { ctx.as_mut() } {
        Some(c) => c,
        None => return -1,
    };
    let port_name = match unsafe { c_str_to_str(port_name) } {
        Some(s) => s,
        None => return -1,
    };

    ctx.port_read_modes.insert(port_name.to_string(), mode);
    0
}

/// Poll all subscribed services for new data.
///
/// Returns 1 if any data was received, 0 if none, -1 on error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sldn_input_poll(ctx: *mut DenoNativeContext) -> i32 {
    let ctx = match unsafe { ctx.as_mut() } {
        Some(c) => c,
        None => return -1,
    };

    let mut has_data = false;

    for (_service_name, state) in ctx.subscribers.iter_mut() {
        while let Ok(Some(sample)) = state.subscriber.receive() {
            let buf: &[u8] = sample.payload();
            if buf.len() < FRAME_HEADER_SIZE {
                tracing::error!("received frame smaller than header ({} bytes)", buf.len());
                continue;
            }
            let header = FrameHeader::read_from_slice(buf);
            let port_name = header.port().to_string();
            let ts = header.timestamp_ns;
            let data_len = header.len as usize;
            if FRAME_HEADER_SIZE + data_len > buf.len() {
                tracing::error!("frame data truncated: header.len={} buf.len()={}", data_len, buf.len());
                continue;
            }
            let data = buf[FRAME_HEADER_SIZE..FRAME_HEADER_SIZE + data_len].to_vec();

            state.pending.entry(port_name).or_default().push((data, ts));
            has_data = true;
        }
    }

    if has_data {
        1
    } else {
        0
    }
}

/// Read data from a specific port.
///
/// Uses the port's read mode (set via `sldn_input_set_read_mode`):
/// - SkipToLatest (default): Drains buffer, returns only the newest payload.
/// - ReadNextInOrder: Returns oldest payload in FIFO order.
///
/// Returns 0 on success, 1 if no data available, -1 on error.
///
/// `out_len` receives the actual data length.
/// `out_ts` receives the timestamp in nanoseconds.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sldn_input_read(
    ctx: *mut DenoNativeContext,
    port_name: *const c_char,
    out_buf: *mut u8,
    buf_len: u32,
    out_len: *mut u32,
    out_ts: *mut i64,
) -> i32 {
    let ctx = match unsafe { ctx.as_mut() } {
        Some(c) => c,
        None => return -1,
    };
    let port_name = match unsafe { c_str_to_str(port_name) } {
        Some(s) => s,
        None => return -1,
    };

    let read_mode = ctx
        .port_read_modes
        .get(port_name)
        .copied()
        .unwrap_or(READ_MODE_SKIP_TO_LATEST);

    // Search all subscribers for pending data on this port
    for (_service_name, state) in ctx.subscribers.iter_mut() {
        if let Some(queue) = state.pending.get_mut(port_name) {
            if queue.is_empty() {
                continue;
            }

            let (data, ts) = if read_mode == READ_MODE_READ_NEXT_IN_ORDER {
                // FIFO: return oldest
                queue.remove(0)
            } else {
                // SkipToLatest: drain buffer, return newest
                let last = queue.len() - 1;
                let item = queue.swap_remove(last);
                queue.clear();
                item
            };

            let copy_len = data.len().min(buf_len as usize);
            if !out_buf.is_null() && copy_len > 0 {
                unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), out_buf, copy_len) };
            }
            if !out_len.is_null() {
                unsafe { *out_len = data.len() as u32 };
            }
            if !out_ts.is_null() {
                unsafe { *out_ts = ts };
            }
            return 0;
        }
    }

    // No data available
    if !out_len.is_null() {
        unsafe { *out_len = 0 };
    }
    1
}

// ============================================================================
// C ABI — Output (publish + write)
// ============================================================================

/// Create a publisher for an iceoryx2 service.
///
/// `dest_port` is the destination processor's input port name, used in FramePayload routing.
/// Returns 0 on success, -1 on failure.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sldn_output_publish(
    ctx: *mut DenoNativeContext,
    service_name: *const c_char,
    port_name: *const c_char,
    dest_port: *const c_char,
    schema_name: *const c_char,
    max_payload_bytes: usize,
) -> i32 {
    let ctx = match unsafe { ctx.as_mut() } {
        Some(c) => c,
        None => return -1,
    };
    let service_name = match unsafe { c_str_to_str(service_name) } {
        Some(s) => s,
        None => return -1,
    };
    let port_name = match unsafe { c_str_to_str(port_name) } {
        Some(s) => s,
        None => return -1,
    };
    let dest_port_str = match unsafe { c_str_to_str(dest_port) } {
        Some(s) => s,
        None => return -1,
    };
    let schema = match unsafe { c_str_to_str(schema_name) } {
        Some(s) => s,
        None => return -1,
    };

    let service_name_iox = match ServiceName::new(service_name) {
        Ok(n) => n,
        Err(e) => {
            tracing::error!(
                "[sldn:{}] Invalid service name '{}': {}",
                ctx.processor_id, service_name, e
            );
            return -1;
        }
    };

    let service = match ctx
        .node
        .service_builder(&service_name_iox)
        .publish_subscribe::<[u8]>()
        .max_publishers(16)
        .subscriber_max_buffer_size(16)
        .open_or_create()
    {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(
                "[sldn:{}] Failed to open service '{}': {}",
                ctx.processor_id, service_name, e
            );
            return -1;
        }
    };

    let publisher = match service.publisher_builder().initial_max_slice_len(max_payload_bytes + FRAME_HEADER_SIZE).create() {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(
                "[sldn:{}] Failed to create publisher for '{}': {}",
                ctx.processor_id, service_name, e
            );
            return -1;
        }
    };

    ctx.publishers.insert(
        port_name.to_string(),
        PublisherState {
            publisher,
            schema_name: schema.to_string(),
            dest_port: dest_port_str.to_string(),
        },
    );

    0
}

/// Write data to a specific output port.
///
/// Returns 0 on success, -1 on failure.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sldn_output_write(
    ctx: *mut DenoNativeContext,
    port_name: *const c_char,
    data: *const u8,
    data_len: u32,
    timestamp_ns: i64,
) -> i32 {
    let ctx = match unsafe { ctx.as_mut() } {
        Some(c) => c,
        None => return -1,
    };
    let port_name = match unsafe { c_str_to_str(port_name) } {
        Some(s) => s,
        None => return -1,
    };

    let state = match ctx.publishers.get(port_name) {
        Some(s) => s,
        None => {
            tracing::error!(
                "[sldn:{}] No publisher for port '{}'",
                ctx.processor_id, port_name
            );
            return -1;
        }
    };

    let data_slice = if data.is_null() || data_len == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(data, data_len as usize) }
    };

    let total_len = FRAME_HEADER_SIZE + data_slice.len();
    let mut frame = vec![0u8; total_len];
    FrameHeader::new(&state.dest_port, &state.schema_name, timestamp_ns, data_slice.len() as u32)
        .write_to_slice(&mut frame[..FRAME_HEADER_SIZE]);
    frame[FRAME_HEADER_SIZE..].copy_from_slice(data_slice);

    let sample = match state.publisher.loan_slice_uninit(total_len) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(
                "[sldn:{}] Failed to loan slice for port '{}': {:?}",
                ctx.processor_id, port_name, e
            );
            return -1;
        }
    };
    let sample = sample.write_from_slice(&frame);
    if let Err(e) = sample.send() {
        tracing::error!(
            "[sldn:{}] Failed to send sample for port '{}': {:?}",
            ctx.processor_id, port_name, e
        );
        return -1;
    }

    0
}

// ============================================================================
// C ABI — GPU Surface operations (macOS via raw C FFI)
// ============================================================================

#[cfg(target_os = "macos")]
mod gpu_surface {
    use std::ffi::c_void;

    type IOSurfaceID = u32;
    type IOSurfaceRef = *const c_void;
    const IOSURFACE_LOCK_READ_ONLY: u32 = 1;

    #[link(name = "IOSurface", kind = "framework")]
    extern "C" {
        fn IOSurfaceLookup(csid: IOSurfaceID) -> IOSurfaceRef;
        fn IOSurfaceCreate(properties: CFDictionaryRef) -> IOSurfaceRef;
        fn IOSurfaceGetID(buffer: IOSurfaceRef) -> IOSurfaceID;
        fn IOSurfaceGetWidth(buffer: IOSurfaceRef) -> usize;
        fn IOSurfaceGetHeight(buffer: IOSurfaceRef) -> usize;
        fn IOSurfaceGetBytesPerRow(buffer: IOSurfaceRef) -> usize;
        fn IOSurfaceGetBaseAddress(buffer: IOSurfaceRef) -> *mut u8;
        fn IOSurfaceLock(buffer: IOSurfaceRef, options: u32, seed: *mut u32) -> i32;
        fn IOSurfaceUnlock(buffer: IOSurfaceRef, options: u32, seed: *mut u32) -> i32;
        fn IOSurfaceIncrementUseCount(buffer: IOSurfaceRef);
        fn IOSurfaceDecrementUseCount(buffer: IOSurfaceRef);
    }

    type CFDictionaryRef = *const c_void;
    type CFStringRef = *const c_void;
    type CFNumberRef = *const c_void;
    type CFAllocatorRef = *const c_void;
    type CFIndex = isize;
    type CFNumberType = i32;
    const K_CF_NUMBER_INT_TYPE: CFNumberType = 9; // kCFNumberIntType

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        static kCFAllocatorDefault: CFAllocatorRef;

        fn CFStringCreateWithCString(
            alloc: CFAllocatorRef,
            c_str: *const u8,
            encoding: u32,
        ) -> CFStringRef;
        fn CFNumberCreate(
            alloc: CFAllocatorRef,
            the_type: CFNumberType,
            value_ptr: *const c_void,
        ) -> CFNumberRef;
        fn CFDictionaryCreate(
            alloc: CFAllocatorRef,
            keys: *const *const c_void,
            values: *const *const c_void,
            num_values: CFIndex,
            key_callbacks: *const c_void,
            value_callbacks: *const c_void,
        ) -> CFDictionaryRef;
        fn CFRelease(cf: *const c_void);

        static kCFTypeDictionaryKeyCallBacks: c_void;
        static kCFTypeDictionaryValueCallBacks: c_void;
    }

    const K_CF_STRING_ENCODING_UTF8: u32 = 0x08000100;

    /// Create a CFString from a null-terminated C string literal.
    unsafe fn cf_string(s: &[u8]) -> CFStringRef {
        CFStringCreateWithCString(kCFAllocatorDefault, s.as_ptr(), K_CF_STRING_ENCODING_UTF8)
    }

    /// Create a CFNumber from an i32 value.
    unsafe fn cf_number_i32(val: i32) -> CFNumberRef {
        CFNumberCreate(
            kCFAllocatorDefault,
            K_CF_NUMBER_INT_TYPE,
            &val as *const i32 as *const c_void,
        )
    }

    /// Opaque handle to an IOSurface.
    pub struct SurfaceHandle {
        pub(crate) surface_ref: IOSurfaceRef,
        pub surface_id: u32,
        pub width: u32,
        pub height: u32,
        pub bytes_per_row: u32,
        pub base_address: *mut u8,
        pub is_locked: bool,
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_gpu_surface_lookup(iosurface_id: u32) -> *mut SurfaceHandle {
        let surface_ref = IOSurfaceLookup(iosurface_id);
        if surface_ref.is_null() {
            tracing::error!("IOSurface not found: {}", iosurface_id);
            return std::ptr::null_mut();
        }

        IOSurfaceIncrementUseCount(surface_ref);

        let handle = SurfaceHandle {
            surface_ref,
            surface_id: iosurface_id,
            width: IOSurfaceGetWidth(surface_ref) as u32,
            height: IOSurfaceGetHeight(surface_ref) as u32,
            bytes_per_row: IOSurfaceGetBytesPerRow(surface_ref) as u32,
            base_address: std::ptr::null_mut(),
            is_locked: false,
        };

        Box::into_raw(Box::new(handle))
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_gpu_surface_lock(
        handle: *mut SurfaceHandle,
        read_only: i32,
    ) -> i32 {
        let handle = match handle.as_mut() {
            Some(h) => h,
            None => return -1,
        };

        let options = if read_only != 0 {
            IOSURFACE_LOCK_READ_ONLY
        } else {
            0
        };

        let result = IOSurfaceLock(handle.surface_ref, options, std::ptr::null_mut());
        if result != 0 {
            tracing::error!(
                "IOSurface lock failed: surface={}, result={}",
                handle.surface_id, result
            );
            return -1;
        }

        handle.base_address = IOSurfaceGetBaseAddress(handle.surface_ref);
        handle.is_locked = true;
        0
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_gpu_surface_unlock(
        handle: *mut SurfaceHandle,
        read_only: i32,
    ) -> i32 {
        let handle = match handle.as_mut() {
            Some(h) => h,
            None => return -1,
        };

        let options = if read_only != 0 {
            IOSURFACE_LOCK_READ_ONLY
        } else {
            0
        };

        let result = IOSurfaceUnlock(handle.surface_ref, options, std::ptr::null_mut());
        handle.base_address = std::ptr::null_mut();
        handle.is_locked = false;

        if result != 0 {
            -1
        } else {
            0
        }
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_gpu_surface_base_address(
        handle: *const SurfaceHandle,
    ) -> *mut u8 {
        match handle.as_ref() {
            Some(h) => h.base_address,
            None => std::ptr::null_mut(),
        }
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_gpu_surface_width(handle: *const SurfaceHandle) -> u32 {
        handle.as_ref().map(|h| h.width).unwrap_or(0)
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_gpu_surface_height(handle: *const SurfaceHandle) -> u32 {
        handle.as_ref().map(|h| h.height).unwrap_or(0)
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_gpu_surface_bytes_per_row(handle: *const SurfaceHandle) -> u32 {
        handle.as_ref().map(|h| h.bytes_per_row).unwrap_or(0)
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_gpu_surface_create(
        width: u32,
        height: u32,
        bytes_per_element: u32,
    ) -> *mut SurfaceHandle {
        let bytes_per_row = width * bytes_per_element;
        let alloc_size = bytes_per_row * height;

        // IOSurface property keys (null-terminated C string literals)
        let k_width = cf_string(b"IOSurfaceWidth\0");
        let k_height = cf_string(b"IOSurfaceHeight\0");
        let k_bytes_per_element = cf_string(b"IOSurfaceBytesPerElement\0");
        let k_bytes_per_row = cf_string(b"IOSurfaceBytesPerRow\0");
        let k_alloc_size = cf_string(b"IOSurfaceAllocSize\0");
        let k_pixel_format = cf_string(b"IOSurfacePixelFormat\0");

        // BGRA pixel format: 'BGRA' = 0x42475241
        let pixel_format: i32 = 0x42475241u32 as i32;

        let v_width = cf_number_i32(width as i32);
        let v_height = cf_number_i32(height as i32);
        let v_bpe = cf_number_i32(bytes_per_element as i32);
        let v_bpr = cf_number_i32(bytes_per_row as i32);
        let v_alloc = cf_number_i32(alloc_size as i32);
        let v_pixel_format = cf_number_i32(pixel_format);

        let keys: [*const c_void; 6] = [
            k_width,
            k_height,
            k_bytes_per_element,
            k_bytes_per_row,
            k_alloc_size,
            k_pixel_format,
        ];
        let values: [*const c_void; 6] = [v_width, v_height, v_bpe, v_bpr, v_alloc, v_pixel_format];

        let properties = CFDictionaryCreate(
            kCFAllocatorDefault,
            keys.as_ptr(),
            values.as_ptr(),
            6,
            &kCFTypeDictionaryKeyCallBacks as *const c_void,
            &kCFTypeDictionaryValueCallBacks as *const c_void,
        );

        let surface_ref = IOSurfaceCreate(properties);

        // Release CF objects
        CFRelease(properties);
        for k in &keys {
            CFRelease(*k);
        }
        for v in &values {
            CFRelease(*v);
        }

        if surface_ref.is_null() {
            tracing::error!(
                "IOSurfaceCreate failed: {}x{} bpe={}",
                width, height, bytes_per_element
            );
            return std::ptr::null_mut();
        }

        let surface_id = IOSurfaceGetID(surface_ref);

        let handle = SurfaceHandle {
            surface_ref,
            surface_id,
            width,
            height,
            bytes_per_row,
            base_address: std::ptr::null_mut(),
            is_locked: false,
        };

        Box::into_raw(Box::new(handle))
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_gpu_surface_get_id(handle: *const SurfaceHandle) -> u32 {
        handle.as_ref().map(|h| h.surface_id).unwrap_or(0)
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_gpu_surface_release(handle: *mut SurfaceHandle) {
        if !handle.is_null() {
            let h = Box::from_raw(handle);
            IOSurfaceDecrementUseCount(h.surface_ref);
        }
    }
}

#[cfg(target_os = "linux")]
mod gpu_surface {
    //! Linux GPU surface handle backed by a DMA-BUF file descriptor.
    //!
    //! Mirror of `streamlib-python-native`'s Linux `gpu_surface` module —
    //! identical shape, `sldn_` prefix. See that file for full background.
    //!
    //! CPU access on lock goes through a Vulkan DMA-BUF import
    //! (`VkImportMemoryFdInfoKHR` + `vkBindBufferMemory` + `vkMapMemory`) —
    //! same shape as the host's `VulkanPixelBuffer::from_dma_buf_fd` so both
    //! ends speak the canonical driver-supported path. The import-side only —
    //! allocation always escalates to the host per the research doc.
    use std::os::unix::io::RawFd;
    use std::sync::Arc;

    use vulkanalia::vk::{self, Handle as _};

    use super::surface_share_vulkan_linux::SurfaceShareVulkanDevice;

    /// Surface backend used for the currently-locked mapping. Reported via
    /// [`sldn_gpu_surface_backend`] so tests can assert the import took the
    /// Vulkan path rather than silently falling back.
    pub const SURFACE_BACKEND_NONE: u32 = 0;
    pub const SURFACE_BACKEND_VULKAN: u32 = 2;

    pub struct SurfaceHandle {
        /// One fd per DMA-BUF plane. Multi-plane DMA-BUFs (e.g. NV12 under
        /// DRM format modifiers with disjoint Y/UV allocations) carry one
        /// per plane, keyed by plane index; single-plane surfaces carry a
        /// one-element vec. Mirrors the Python-native twin.
        pub fds: Vec<RawFd>,
        pub plane_sizes: Vec<u64>,
        pub plane_offsets: Vec<u64>,
        pub width: u32,
        pub height: u32,
        pub bytes_per_row: u32,
        /// Total byte size across all planes — the sum of `plane_sizes`.
        pub size: u64,
        /// Host-mapped base address of plane 0, populated by `lock` (Vulkan
        /// path) or `plane_mmap` (CPU path). Multi-plane accessor reads
        /// from [`Self::plane_mapped_ptrs`].
        pub mapped_ptr: *mut u8,
        pub plane_mapped_ptrs: Vec<*mut u8>,
        pub is_locked: bool,
        /// Vulkan device attached by [`super::surface_client::sldn_surface_resolve_surface`].
        /// `None` means the service could not create a Vulkan device and lock
        /// will fail cleanly.
        pub vulkan_device: Option<Arc<SurfaceShareVulkanDevice>>,
        /// Imported `vk::Buffer` — valid only while `is_locked`.
        pub vulkan_buffer: vk::Buffer,
        /// Imported `vk::DeviceMemory` — valid only while `is_locked`.
        pub vulkan_memory: vk::DeviceMemory,
        /// Backend used for the current (or most recent) lock.
        pub backend: u32,
    }

    impl SurfaceHandle {
        pub fn base_address(&self, plane_index: usize) -> *mut u8 {
            if plane_index == 0 && !self.mapped_ptr.is_null() {
                return self.mapped_ptr;
            }
            match self.plane_mapped_ptrs.get(plane_index) {
                Some(&p) => p,
                None => std::ptr::null_mut(),
            }
        }
    }

    impl Drop for SurfaceHandle {
        fn drop(&mut self) {
            if self.is_locked {
                if let Some(device) = self.vulkan_device.as_ref() {
                    device.destroy_imported(self.vulkan_buffer, self.vulkan_memory);
                }
            }
            for (i, ptr) in self.plane_mapped_ptrs.iter().enumerate() {
                if !ptr.is_null() {
                    if let Some(size) = self.plane_sizes.get(i) {
                        unsafe { libc::munmap(*ptr as *mut libc::c_void, *size as usize) };
                    }
                }
            }
            for fd in &self.fds {
                if *fd >= 0 {
                    unsafe { libc::close(*fd) };
                }
            }
        }
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_gpu_surface_lookup(_iosurface_id: u32) -> *mut SurfaceHandle {
        tracing::error!("GPU surface lookup by IOSurface id is macOS-only; use handle check_out");
        std::ptr::null_mut()
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_gpu_surface_lock(
        handle: *mut SurfaceHandle,
        _read_only: i32,
    ) -> i32 {
        let handle = match unsafe { handle.as_mut() } {
            Some(h) => h,
            None => return -1,
        };
        if handle.is_locked {
            return 0;
        }
        // Vulkan lock imports plane 0 only — multi-plane Vulkan producers
        // do not exist in tree yet. Multi-plane consumers use
        // `sldn_gpu_surface_plane_mmap` for CPU access to non-0 planes.
        let fd0 = match handle.fds.first() {
            Some(&fd) if fd >= 0 => fd,
            _ => return -1,
        };
        let plane0_size = handle.plane_sizes.first().copied().unwrap_or(handle.size);
        if plane0_size == 0 {
            return -1;
        }
        let device = match handle.vulkan_device.as_ref() {
            Some(d) => Arc::clone(d),
            None => {
                tracing::error!(
                    "gpu_surface_lock: no Vulkan device attached — resolve_surface \
                     must have failed to initialize one"
                );
                return -1;
            }
        };
        let dup_fd = unsafe { libc::dup(fd0) };
        if dup_fd < 0 {
            tracing::error!(
                "gpu_surface_lock: dup fd failed: {}",
                std::io::Error::last_os_error()
            );
            return -1;
        }
        let imported = match device.import_dma_buf_fd(dup_fd, plane0_size) {
            Ok(i) => i,
            Err(e) => {
                tracing::error!(
                    "gpu_surface_lock: Vulkan DMA-BUF import failed for fd {} ({}B): {}",
                    fd0, plane0_size, e
                );
                unsafe { libc::close(dup_fd) };
                return -1;
            }
        };
        handle.vulkan_buffer = imported.buffer;
        handle.vulkan_memory = imported.memory;
        handle.mapped_ptr = imported.mapped_ptr;
        handle.is_locked = true;
        handle.backend = SURFACE_BACKEND_VULKAN;
        0
    }

    /// mmap a specific plane into user space. See the Python twin for the
    /// full contract.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_gpu_surface_plane_mmap(
        handle: *mut SurfaceHandle,
        plane_index: u32,
    ) -> i32 {
        let handle = match unsafe { handle.as_mut() } {
            Some(h) => h,
            None => return -1,
        };
        let idx = plane_index as usize;
        let fd = match handle.fds.get(idx) {
            Some(&fd) if fd >= 0 => fd,
            _ => return -1,
        };
        let size = match handle.plane_sizes.get(idx) {
            Some(&s) if s > 0 => s as usize,
            _ => return -1,
        };
        if handle
            .plane_mapped_ptrs
            .get(idx)
            .map(|p| !p.is_null())
            .unwrap_or(false)
        {
            return 0;
        }
        let offset = handle.plane_offsets.get(idx).copied().unwrap_or(0) as libc::off_t;
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd,
                offset,
            )
        };
        if ptr == libc::MAP_FAILED {
            tracing::error!(
                "sldn_gpu_surface_plane_mmap: mmap failed for plane {} (fd {}, size {}): {}",
                idx, fd, size, std::io::Error::last_os_error()
            );
            return -1;
        }
        handle.plane_mapped_ptrs[idx] = ptr as *mut u8;
        if idx == 0 && handle.mapped_ptr.is_null() {
            handle.mapped_ptr = ptr as *mut u8;
        }
        0
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_gpu_surface_unlock(
        handle: *mut SurfaceHandle,
        _read_only: i32,
    ) -> i32 {
        let handle = match unsafe { handle.as_mut() } {
            Some(h) => h,
            None => return -1,
        };
        if !handle.is_locked {
            return 0;
        }
        if let Some(device) = handle.vulkan_device.as_ref() {
            device.destroy_imported(handle.vulkan_buffer, handle.vulkan_memory);
        }
        handle.vulkan_buffer = vk::Buffer::null();
        handle.vulkan_memory = vk::DeviceMemory::null();
        handle.mapped_ptr = std::ptr::null_mut();
        handle.is_locked = false;
        0
    }

    /// Return which backend was used for the current (or most recent) lock.
    ///
    /// `0` = no backend active (handle never locked, or unlock cleared state),
    /// `2` = Vulkan DMA-BUF import via `VkImportMemoryFdInfoKHR`.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_gpu_surface_backend(handle: *const SurfaceHandle) -> u32 {
        unsafe { handle.as_ref() }
            .map(|h| h.backend)
            .unwrap_or(SURFACE_BACKEND_NONE)
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_gpu_surface_base_address(
        handle: *const SurfaceHandle,
    ) -> *mut u8 {
        match unsafe { handle.as_ref() } {
            Some(h) => h.base_address(0),
            None => std::ptr::null_mut(),
        }
    }

    /// Per-plane base address accessor. Returns null if the plane index is
    /// out of range, if the plane is not mmap'd, or if the handle is null.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_gpu_surface_plane_base_address(
        handle: *const SurfaceHandle,
        plane_index: u32,
    ) -> *mut u8 {
        match unsafe { handle.as_ref() } {
            Some(h) => h.base_address(plane_index as usize),
            None => std::ptr::null_mut(),
        }
    }

    /// Number of DMA-BUF planes on this surface (always >= 1).
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_gpu_surface_plane_count(
        handle: *const SurfaceHandle,
    ) -> u32 {
        unsafe { handle.as_ref() }
            .map(|h| h.fds.len() as u32)
            .unwrap_or(0)
    }

    /// Byte size of the given plane, or `0` if the plane index is out of
    /// range or the handle is null.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_gpu_surface_plane_size(
        handle: *const SurfaceHandle,
        plane_index: u32,
    ) -> u64 {
        unsafe { handle.as_ref() }
            .and_then(|h| h.plane_sizes.get(plane_index as usize).copied())
            .unwrap_or(0)
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_gpu_surface_width(handle: *const SurfaceHandle) -> u32 {
        unsafe { handle.as_ref() }.map(|h| h.width).unwrap_or(0)
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_gpu_surface_height(handle: *const SurfaceHandle) -> u32 {
        unsafe { handle.as_ref() }.map(|h| h.height).unwrap_or(0)
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_gpu_surface_bytes_per_row(
        handle: *const SurfaceHandle,
    ) -> u32 {
        unsafe { handle.as_ref() }.map(|h| h.bytes_per_row).unwrap_or(0)
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_gpu_surface_create(
        _width: u32,
        _height: u32,
        _bytes_per_element: u32,
    ) -> *mut SurfaceHandle {
        tracing::error!(
            "GPU surface creation in subprocess is not supported on Linux; allocation \
             must go through escalate IPC (GpuContextFullAccess -> RHI -> SurfaceStore.check_in)"
        );
        std::ptr::null_mut()
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_gpu_surface_get_id(handle: *const SurfaceHandle) -> u32 {
        // Linux surface IDs are handle UUIDs (strings), not u32 IOSurfaceIDs;
        // return the fd as a best-effort numeric token. See the Python twin
        // in streamlib-python-native for the same behavior.
        unsafe { handle.as_ref() }
            .and_then(|h| h.fds.first().copied())
            .map(|fd| fd as u32)
            .unwrap_or(0)
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_gpu_surface_release(handle: *mut SurfaceHandle) {
        if !handle.is_null() {
            let _ = unsafe { Box::from_raw(handle) };
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod gpu_surface {
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_gpu_surface_lookup(_iosurface_id: u32) -> *mut std::ffi::c_void {
        tracing::error!("GPU surface operations not supported on this platform");
        std::ptr::null_mut()
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_gpu_surface_lock(
        _handle: *mut std::ffi::c_void,
        _read_only: i32,
    ) -> i32 {
        -1
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_gpu_surface_unlock(
        _handle: *mut std::ffi::c_void,
        _read_only: i32,
    ) -> i32 {
        -1
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_gpu_surface_base_address(
        _handle: *const std::ffi::c_void,
    ) -> *mut u8 {
        std::ptr::null_mut()
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_gpu_surface_width(_handle: *const std::ffi::c_void) -> u32 {
        0
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_gpu_surface_height(_handle: *const std::ffi::c_void) -> u32 {
        0
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_gpu_surface_bytes_per_row(
        _handle: *const std::ffi::c_void,
    ) -> u32 {
        0
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_gpu_surface_create(
        _width: u32,
        _height: u32,
        _bytes_per_element: u32,
    ) -> *mut std::ffi::c_void {
        tracing::error!("GPU surface creation not supported on this platform");
        std::ptr::null_mut()
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_gpu_surface_get_id(_handle: *const std::ffi::c_void) -> u32 {
        0
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_gpu_surface_release(_handle: *mut std::ffi::c_void) {}
}

// ============================================================================
// C ABI — Surface-share XPC client (macOS surface resolution)
// ============================================================================

#[cfg(target_os = "macos")]
mod surface_client {
    use std::collections::HashMap;
    use std::ffi::{c_char, c_void, CStr, CString};

    use super::gpu_surface::SurfaceHandle;

    type XpcObjectT = *mut c_void;
    type XpcConnectionT = *mut c_void;
    type MachPortT = u32;
    type IOSurfaceRef = *const c_void;
    type IOSurfaceID = u32;

    #[link(name = "System", kind = "dylib")]
    extern "C" {
        fn xpc_connection_create_mach_service(
            name: *const c_char,
            target_queue: *mut c_void,
            flags: u64,
        ) -> XpcConnectionT;
        fn xpc_connection_set_event_handler(connection: XpcConnectionT, handler: *mut c_void);
        fn xpc_connection_resume(connection: XpcConnectionT);
        fn xpc_connection_cancel(connection: XpcConnectionT);
        fn xpc_connection_send_message_with_reply_sync(
            connection: XpcConnectionT,
            message: XpcObjectT,
        ) -> XpcObjectT;
        fn xpc_dictionary_create(
            keys: *const *const c_char,
            values: *const XpcObjectT,
            count: usize,
        ) -> XpcObjectT;
        fn xpc_dictionary_set_string(dict: XpcObjectT, key: *const c_char, value: *const c_char);
        fn xpc_dictionary_set_mach_send(dict: XpcObjectT, key: *const c_char, port: MachPortT);
        fn xpc_dictionary_get_string(dict: XpcObjectT, key: *const c_char) -> *const c_char;
        fn xpc_dictionary_copy_mach_send(dict: XpcObjectT, key: *const c_char) -> MachPortT;
        fn xpc_release(object: XpcObjectT);
        fn xpc_get_type(object: XpcObjectT) -> *const c_void;
        static _xpc_type_error: c_void;
    }

    #[link(name = "IOSurface", kind = "framework")]
    extern "C" {
        fn IOSurfaceLookupFromMachPort(port: MachPortT) -> IOSurfaceRef;
        fn IOSurfaceCreateMachPort(buffer: IOSurfaceRef) -> MachPortT;
        fn IOSurfaceGetID(buffer: IOSurfaceRef) -> IOSurfaceID;
        fn IOSurfaceGetWidth(buffer: IOSurfaceRef) -> usize;
        fn IOSurfaceGetHeight(buffer: IOSurfaceRef) -> usize;
        fn IOSurfaceGetBytesPerRow(buffer: IOSurfaceRef) -> usize;
        fn IOSurfaceIncrementUseCount(buffer: IOSurfaceRef);
        fn IOSurfaceDecrementUseCount(buffer: IOSurfaceRef);
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFRetain(cf: *const c_void) -> *const c_void;
        fn CFRelease(cf: *const c_void);
    }

    #[link(name = "System")]
    extern "C" {
        fn mach_port_deallocate(task: MachPortT, name: MachPortT) -> i32;
        fn mach_task_self() -> MachPortT;
    }

    // Minimal Obj-C block for XPC event handler (no-op, we use sync send/reply)
    #[repr(C)]
    struct Block {
        isa: *const c_void,
        flags: i32,
        reserved: i32,
        invoke: *const c_void,
        descriptor: *const BlockDescriptor,
    }

    #[repr(C)]
    struct BlockDescriptor {
        reserved: u64,
        size: u64,
    }

    extern "C" {
        static _NSConcreteMallocBlock: c_void;
    }

    const BLOCK_FLAGS_NEEDS_FREE: i32 = 1 << 24;

    fn xpc_is_error(object: XpcObjectT) -> bool {
        if object.is_null() {
            return false;
        }
        unsafe { std::ptr::eq(xpc_get_type(object), &_xpc_type_error as *const _) }
    }

    struct CachedSurface {
        surface_ref: IOSurfaceRef,
        surface_id: u32,
        width: u32,
        height: u32,
        bytes_per_row: u32,
    }

    /// Opaque handle to a handle XPC connection.
    pub struct SurfaceShareHandle {
        connection: XpcConnectionT,
        resolve_cache: HashMap<String, CachedSurface>,
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_surface_connect(
        xpc_service_name: *const c_char,
    ) -> *mut SurfaceShareHandle {
        if xpc_service_name.is_null() {
            tracing::error!("surface_connect: null service name");
            return std::ptr::null_mut();
        }

        let connection =
            xpc_connection_create_mach_service(xpc_service_name, std::ptr::null_mut(), 0);

        if connection.is_null() {
            let name = CStr::from_ptr(xpc_service_name).to_string_lossy();
            tracing::error!(
                "surface_connect: failed to create XPC connection to '{}'",
                name
            );
            return std::ptr::null_mut();
        }

        // Set minimal event handler (required before resume)
        extern "C" fn event_handler_trampoline(_block: *mut Block, _event: XpcObjectT) {}

        static DESCRIPTOR: BlockDescriptor = BlockDescriptor {
            reserved: 0,
            size: std::mem::size_of::<Block>() as u64,
        };

        let block = Box::new(Block {
            isa: unsafe { &_NSConcreteMallocBlock as *const _ },
            flags: BLOCK_FLAGS_NEEDS_FREE,
            reserved: 0,
            invoke: event_handler_trampoline as *const c_void,
            descriptor: &DESCRIPTOR,
        });
        let block_ptr = Box::into_raw(block) as *mut c_void;

        xpc_connection_set_event_handler(connection, block_ptr);
        xpc_connection_resume(connection);

        let name = CStr::from_ptr(xpc_service_name).to_string_lossy();
        tracing::error!("surface_connect: connected to '{}'", name);

        Box::into_raw(Box::new(SurfaceShareHandle {
            connection,
            resolve_cache: HashMap::new(),
        }))
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_surface_disconnect(handle: *mut SurfaceShareHandle) {
        if !handle.is_null() {
            let handle = Box::from_raw(handle);
            for cached in handle.resolve_cache.values() {
                IOSurfaceDecrementUseCount(cached.surface_ref);
                CFRelease(cached.surface_ref);
            }
            xpc_connection_cancel(handle.connection);
        }
    }

    /// Resolve a handle pool_id to an IOSurface handle via XPC lookup.
    ///
    /// Returns a SurfaceHandle pointer (same type as sldn_gpu_surface_lookup).
    /// Results are cached — repeated lookups for the same pool_id are fast.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_surface_resolve_surface(
        handle: *mut SurfaceShareHandle,
        pool_id: *const c_char,
    ) -> *mut SurfaceHandle {
        let handle = match handle.as_mut() {
            Some(b) => b,
            None => {
                tracing::error!("surface_resolve_surface: null handle");
                return std::ptr::null_mut();
            }
        };

        let pool_id_str = match c_str_to_str(pool_id) {
            Some(s) => s,
            None => {
                tracing::error!("surface_resolve_surface: null pool_id");
                return std::ptr::null_mut();
            }
        };

        // Check resolve cache
        if let Some(cached) = handle.resolve_cache.get(pool_id_str) {
            CFRetain(cached.surface_ref);
            IOSurfaceIncrementUseCount(cached.surface_ref);
            return Box::into_raw(Box::new(SurfaceHandle {
                surface_ref: cached.surface_ref,
                surface_id: cached.surface_id,
                width: cached.width,
                height: cached.height,
                bytes_per_row: cached.bytes_per_row,
                base_address: std::ptr::null_mut(),
                is_locked: false,
            }));
        }

        // Cache miss — XPC lookup to handle
        let request = xpc_dictionary_create(std::ptr::null(), std::ptr::null(), 0);
        if request.is_null() {
            tracing::error!("surface_resolve_surface: failed to create XPC request");
            return std::ptr::null_mut();
        }

        let op_key = CString::new("op").unwrap();
        let op_value = CString::new("lookup").unwrap();
        xpc_dictionary_set_string(request, op_key.as_ptr(), op_value.as_ptr());

        let sid_key = CString::new("surface_id").unwrap();
        let sid_value = CString::new(pool_id_str).unwrap();
        xpc_dictionary_set_string(request, sid_key.as_ptr(), sid_value.as_ptr());

        let reply = xpc_connection_send_message_with_reply_sync(handle.connection, request);
        xpc_release(request);

        if reply.is_null() || xpc_is_error(reply) {
            if !reply.is_null() {
                xpc_release(reply);
            }
            tracing::error!(
                "surface_resolve_surface: XPC lookup failed for '{}'",
                pool_id_str
            );
            return std::ptr::null_mut();
        }

        // Check for error message in reply
        let error_key = CString::new("error").unwrap();
        let error_ptr = xpc_dictionary_get_string(reply, error_key.as_ptr());
        if !error_ptr.is_null() {
            let error_msg = CStr::from_ptr(error_ptr).to_string_lossy();
            tracing::error!(
                "surface_resolve_surface: handle error for '{}': {}",
                pool_id_str, error_msg
            );
            xpc_release(reply);
            return std::ptr::null_mut();
        }

        // Extract mach port
        let port_key = CString::new("mach_port").unwrap();
        let mach_port = xpc_dictionary_copy_mach_send(reply, port_key.as_ptr());
        xpc_release(reply);

        if mach_port == 0 {
            tracing::error!(
                "surface_resolve_surface: invalid mach port for '{}'",
                pool_id_str
            );
            return std::ptr::null_mut();
        }

        // Import IOSurface from mach port
        let surface_ref = IOSurfaceLookupFromMachPort(mach_port);

        // Deallocate our copy of the mach port (IOSurface is retained by the lookup)
        let task = mach_task_self();
        mach_port_deallocate(task, mach_port);

        if surface_ref.is_null() {
            tracing::error!(
                "surface_resolve_surface: IOSurfaceLookupFromMachPort failed for '{}'",
                pool_id_str
            );
            return std::ptr::null_mut();
        }

        // Increment use count for cache entry
        IOSurfaceIncrementUseCount(surface_ref);

        let surface_id = IOSurfaceGetID(surface_ref);
        let width = IOSurfaceGetWidth(surface_ref) as u32;
        let height = IOSurfaceGetHeight(surface_ref) as u32;
        let bytes_per_row = IOSurfaceGetBytesPerRow(surface_ref) as u32;

        // Evict entire cache if it exceeds 128 entries
        if handle.resolve_cache.len() >= 128 {
            for (_key, cached) in handle.resolve_cache.drain() {
                IOSurfaceDecrementUseCount(cached.surface_ref);
                CFRelease(cached.surface_ref);
            }
        }

        handle.resolve_cache.insert(
            pool_id_str.to_string(),
            CachedSurface {
                surface_ref,
                surface_id,
                width,
                height,
                bytes_per_row,
            },
        );

        // Retain + increment use count for returned handle
        CFRetain(surface_ref);
        IOSurfaceIncrementUseCount(surface_ref);

        Box::into_raw(Box::new(SurfaceHandle {
            surface_ref,
            surface_id,
            width,
            height,
            bytes_per_row,
            base_address: std::ptr::null_mut(),
            is_locked: false,
        }))
    }

    /// Create a new IOSurface, register it with the handle, and return a handle.
    ///
    /// `out_pool_id` receives the surface-share-assigned pool UUID as a null-terminated C string.
    /// `pool_id_buf_len` is the size of the out_pool_id buffer.
    ///
    /// Returns a SurfaceHandle pointer, or null on failure.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_surface_acquire_surface(
        handle: *mut SurfaceShareHandle,
        width: u32,
        height: u32,
        bytes_per_element: u32,
        out_pool_id: *mut c_char,
        pool_id_buf_len: u32,
    ) -> *mut SurfaceHandle {
        let handle = match handle.as_mut() {
            Some(b) => b,
            None => {
                tracing::error!("surface_acquire_surface: null handle");
                return std::ptr::null_mut();
            }
        };

        // Create the IOSurface via the existing function
        let surface_handle_ptr =
            super::gpu_surface::sldn_gpu_surface_create(width, height, bytes_per_element);
        if surface_handle_ptr.is_null() {
            return std::ptr::null_mut();
        }

        let surface_handle = &*surface_handle_ptr;

        // Create mach port for the IOSurface
        let mach_port = IOSurfaceCreateMachPort(surface_handle.surface_ref);
        if mach_port == 0 {
            tracing::error!("surface_acquire_surface: IOSurfaceCreateMachPort failed");
            let _ = Box::from_raw(surface_handle_ptr);
            return std::ptr::null_mut();
        }

        // Generate a pool UUID
        // Use IOSurface ID + timestamp for uniqueness (simple, no uuid crate needed)
        let surface_id = IOSurfaceGetID(surface_handle.surface_ref);
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let pool_id = format!("deno-{}-{}", surface_id, ts);

        // Register with handle via XPC
        let request = xpc_dictionary_create(std::ptr::null(), std::ptr::null(), 0);
        if request.is_null() {
            tracing::error!("surface_acquire_surface: failed to create XPC request");
            let task = mach_task_self();
            mach_port_deallocate(task, mach_port);
            let _ = Box::from_raw(surface_handle_ptr);
            return std::ptr::null_mut();
        }

        let op_key = CString::new("op").unwrap();
        let op_value = CString::new("register").unwrap();
        xpc_dictionary_set_string(request, op_key.as_ptr(), op_value.as_ptr());

        let sid_key = CString::new("surface_id").unwrap();
        let sid_value = CString::new(pool_id.as_str()).unwrap();
        xpc_dictionary_set_string(request, sid_key.as_ptr(), sid_value.as_ptr());

        let rid_key = CString::new("runtime_id").unwrap();
        let rid_value = CString::new("deno-subprocess").unwrap();
        xpc_dictionary_set_string(request, rid_key.as_ptr(), rid_value.as_ptr());

        let port_key = CString::new("mach_port").unwrap();
        xpc_dictionary_set_mach_send(request, port_key.as_ptr(), mach_port);

        let reply = xpc_connection_send_message_with_reply_sync(handle.connection, request);
        xpc_release(request);

        // Deallocate our copy of the mach port
        let task = mach_task_self();
        mach_port_deallocate(task, mach_port);

        if reply.is_null() || xpc_is_error(reply) {
            if !reply.is_null() {
                xpc_release(reply);
            }
            tracing::error!("surface_acquire_surface: XPC register failed");
            let _ = Box::from_raw(surface_handle_ptr);
            return std::ptr::null_mut();
        }

        // Check for error in reply
        let error_key = CString::new("error").unwrap();
        let error_ptr = xpc_dictionary_get_string(reply, error_key.as_ptr());
        if !error_ptr.is_null() {
            let error_msg = CStr::from_ptr(error_ptr).to_string_lossy();
            tracing::error!("surface_acquire_surface: handle error: {}", error_msg);
            xpc_release(reply);
            let _ = Box::from_raw(surface_handle_ptr);
            return std::ptr::null_mut();
        }

        xpc_release(reply);

        // Copy pool_id to output buffer
        if !out_pool_id.is_null() && pool_id_buf_len > 0 {
            let bytes = pool_id.as_bytes();
            let copy_len = bytes.len().min((pool_id_buf_len - 1) as usize);
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), out_pool_id as *mut u8, copy_len);
            *out_pool_id.add(copy_len) = 0; // null terminate
        }

        surface_handle_ptr
    }

    unsafe fn c_str_to_str<'a>(ptr: *const c_char) -> Option<&'a str> {
        if ptr.is_null() {
            return None;
        }
        CStr::from_ptr(ptr).to_str().ok()
    }

    // Back-compat aliases for the legacy `sldn_broker_*` FFI names.
    // Every alias forwards to its canonical `sldn_surface_*` counterpart; the
    // symbol ships for one release cycle so Python/Deno apps pinned to the old
    // name keep working. Rust callers (tests, examples) see a `#[deprecated]`
    // warning. See `docs/migration/broker-to-surface-share.md` for the removal
    // plan.

    #[deprecated(note = "renamed to `sldn_surface_connect`; `sldn_broker_connect` is kept for one release cycle — see docs/migration/broker-to-surface-share.md")]
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_broker_connect(xpc_service_name: *const c_char) -> *mut SurfaceShareHandle {
        unsafe { sldn_surface_connect(xpc_service_name) }
    }

    #[deprecated(note = "renamed to `sldn_surface_disconnect`; `sldn_broker_disconnect` is kept for one release cycle — see docs/migration/broker-to-surface-share.md")]
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_broker_disconnect(handle: *mut SurfaceShareHandle) {
        unsafe { sldn_surface_disconnect(handle) }
    }

    #[deprecated(note = "renamed to `sldn_surface_resolve_surface`; `sldn_broker_resolve_surface` is kept for one release cycle — see docs/migration/broker-to-surface-share.md")]
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_broker_resolve_surface(handle: *mut SurfaceShareHandle,
        pool_id: *const c_char) -> *mut SurfaceHandle {
        unsafe { sldn_surface_resolve_surface(handle, pool_id) }
    }

    #[deprecated(note = "renamed to `sldn_surface_acquire_surface`; `sldn_broker_acquire_surface` is kept for one release cycle — see docs/migration/broker-to-surface-share.md")]
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_broker_acquire_surface(handle: *mut SurfaceShareHandle,
        width: u32,
        height: u32,
        bytes_per_element: u32,
        out_pool_id: *mut c_char,
        pool_id_buf_len: u32) -> *mut SurfaceHandle {
        unsafe { sldn_surface_acquire_surface(handle, width, height, bytes_per_element, out_pool_id, pool_id_buf_len) }
    }
}

#[cfg(target_os = "linux")]
mod surface_share_vulkan_linux {
    //! Minimal Vulkan device used by the Deno polyglot consumer to import
    //! DMA-BUF fds handed out by the surface-share service (issue #420). Twin
    //! of the Python native lib's `surface_share_vulkan_linux` — see that
    //! module for the full
    //! rationale; this is byte-for-byte the same implementation with a
    //! different module-scope comment header.
    //!
    //! Consumer-only per the subprocess-import-only safety posture: we load
    //! libvulkan.so via `libloading`, create a bare instance + logical device
    //! enabling only `VK_KHR_external_memory` + `VK_KHR_external_memory_fd` +
    //! `VK_EXT_external_memory_dma_buf`, and expose a single
    //! [`SurfaceShareVulkanDevice::import_dma_buf_fd`] method. Export paths
    //! (`vkGetMemoryFdKHR`) are intentionally absent.
    use std::ffi::{c_char, CStr};
    use std::os::unix::io::RawFd;
    use std::sync::Arc;

    use vulkanalia::loader::{LibloadingLoader, LIBRARY};
    use vulkanalia::prelude::v1_1::*;
    use vulkanalia::vk;

    pub struct SurfaceShareVulkanDevice {
        _entry: vulkanalia::Entry,
        instance: vulkanalia::Instance,
        device: vulkanalia::Device,
        memory_properties: vk::PhysicalDeviceMemoryProperties,
    }

    unsafe impl Send for SurfaceShareVulkanDevice {}
    unsafe impl Sync for SurfaceShareVulkanDevice {}

    pub struct ImportedBuffer {
        pub buffer: vk::Buffer,
        pub memory: vk::DeviceMemory,
        pub mapped_ptr: *mut u8,
    }

    impl SurfaceShareVulkanDevice {
        pub fn try_new() -> Result<Arc<Self>, String> {
            let loader = unsafe { LibloadingLoader::new(LIBRARY) }
                .map_err(|e| format!("load libvulkan: {e}"))?;
            let entry = unsafe { vulkanalia::Entry::new(loader) }
                .map_err(|e| format!("vulkan entry: {e}"))?;

            let app_info = vk::ApplicationInfo::builder()
                .application_name(b"streamlib-polyglot-consumer\0")
                .application_version(vk::make_version(0, 1, 0))
                .engine_name(b"streamlib\0")
                .engine_version(vk::make_version(0, 1, 0))
                .api_version(vk::make_version(1, 1, 0))
                .build();
            let instance_info = vk::InstanceCreateInfo::builder()
                .application_info(&app_info)
                .build();
            let instance = unsafe { entry.create_instance(&instance_info, None) }
                .map_err(|e| format!("create_instance: {e}"))?;

            let result = Self::select_and_create_device(&instance);
            match result {
                Ok((device, physical_device)) => {
                    let memory_properties = unsafe {
                        instance.get_physical_device_memory_properties(physical_device)
                    };
                    Ok(Arc::new(SurfaceShareVulkanDevice {
                        _entry: entry,
                        instance,
                        device,
                        memory_properties,
                    }))
                }
                Err(e) => {
                    unsafe { instance.destroy_instance(None) };
                    Err(e)
                }
            }
        }

        fn select_and_create_device(
            instance: &vulkanalia::Instance,
        ) -> Result<(vulkanalia::Device, vk::PhysicalDevice), String> {
            let physical_devices = unsafe { instance.enumerate_physical_devices() }
                .map_err(|e| format!("enumerate_physical_devices: {e}"))?;
            if physical_devices.is_empty() {
                return Err("no Vulkan-capable physical devices".into());
            }
            let physical_device = physical_devices
                .iter()
                .find(|&&pd| {
                    let p = unsafe { instance.get_physical_device_properties(pd) };
                    p.device_type == vk::PhysicalDeviceType::DISCRETE_GPU
                })
                .copied()
                .unwrap_or(physical_devices[0]);

            let available_ext =
                unsafe { instance.enumerate_device_extension_properties(physical_device, None) }
                    .map_err(|e| format!("enumerate_device_extension_properties: {e}"))?;
            let available_names: Vec<&CStr> = available_ext
                .iter()
                .map(|e| unsafe { CStr::from_ptr(e.extension_name.as_ptr()) })
                .collect();
            let ext_external_memory = c"VK_KHR_external_memory";
            let ext_external_memory_fd = c"VK_KHR_external_memory_fd";
            let ext_dma_buf = c"VK_EXT_external_memory_dma_buf";
            for required in [ext_external_memory, ext_external_memory_fd, ext_dma_buf] {
                if !available_names.contains(&required) {
                    return Err(format!(
                        "required device extension missing: {}",
                        required.to_string_lossy()
                    ));
                }
            }

            let queue_families =
                unsafe { instance.get_physical_device_queue_family_properties(physical_device) };
            if queue_families.is_empty() {
                return Err("physical device has no queue families".into());
            }
            let queue_family_index = 0u32;
            let queue_priorities = [1.0f32];
            let queue_create_infos = [vk::DeviceQueueCreateInfo::builder()
                .queue_family_index(queue_family_index)
                .queue_priorities(&queue_priorities)
                .build()];
            let device_extensions: Vec<*const c_char> = vec![
                ext_external_memory.as_ptr(),
                ext_external_memory_fd.as_ptr(),
                ext_dma_buf.as_ptr(),
            ];
            let device_info = vk::DeviceCreateInfo::builder()
                .queue_create_infos(&queue_create_infos)
                .enabled_extension_names(&device_extensions)
                .build();
            let device =
                unsafe { instance.create_device(physical_device, &device_info, None) }
                    .map_err(|e| format!("create_device: {e}"))?;
            Ok((device, physical_device))
        }

        fn find_memory_type(
            &self,
            type_filter: u32,
            required_flags: vk::MemoryPropertyFlags,
        ) -> Option<u32> {
            for i in 0..self.memory_properties.memory_type_count {
                let type_supported = (type_filter & (1 << i)) != 0;
                let flags = self.memory_properties.memory_types[i as usize].property_flags;
                if type_supported && flags.contains(required_flags) {
                    return Some(i);
                }
            }
            None
        }

        pub fn import_dma_buf_fd(
            &self,
            fd: RawFd,
            size: u64,
        ) -> Result<ImportedBuffer, String> {
            if size == 0 {
                return Err("import_dma_buf_fd: size is 0".into());
            }
            let device_size = size as vk::DeviceSize;

            let mut external_info = vk::ExternalMemoryBufferCreateInfo::builder()
                .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
                .build();
            let buffer_info = vk::BufferCreateInfo::builder()
                .size(device_size)
                .usage(
                    vk::BufferUsageFlags::TRANSFER_SRC
                        | vk::BufferUsageFlags::TRANSFER_DST
                        | vk::BufferUsageFlags::STORAGE_BUFFER,
                )
                .sharing_mode(vk::SharingMode::EXCLUSIVE)
                .push_next(&mut external_info)
                .build();
            let buffer = unsafe { self.device.create_buffer(&buffer_info, None) }
                .map_err(|e| format!("create_buffer: {e}"))?;

            let mem_req = unsafe { self.device.get_buffer_memory_requirements(buffer) };
            let memory_type_index = match self.find_memory_type(
                mem_req.memory_type_bits,
                vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
            ) {
                Some(i) => i,
                None => {
                    unsafe { self.device.destroy_buffer(buffer, None) };
                    return Err(format!(
                        "no HOST_VISIBLE|HOST_COHERENT memory type satisfies filter 0x{:x}",
                        mem_req.memory_type_bits
                    ));
                }
            };
            let alloc_size = device_size.max(mem_req.size);

            let mut import_info = vk::ImportMemoryFdInfoKHR::builder()
                .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
                .fd(fd)
                .build();
            let alloc_info = vk::MemoryAllocateInfo::builder()
                .allocation_size(alloc_size)
                .memory_type_index(memory_type_index)
                .push_next(&mut import_info)
                .build();

            let memory = match unsafe { self.device.allocate_memory(&alloc_info, None) } {
                Ok(m) => m,
                Err(e) => {
                    unsafe { self.device.destroy_buffer(buffer, None) };
                    return Err(format!("allocate_memory (import): {e}"));
                }
            };

            if let Err(e) = unsafe { self.device.bind_buffer_memory(buffer, memory, 0) } {
                unsafe {
                    self.device.free_memory(memory, None);
                    self.device.destroy_buffer(buffer, None);
                }
                return Err(format!("bind_buffer_memory: {e}"));
            }

            let mapped_ptr = match unsafe {
                self.device
                    .map_memory(memory, 0, alloc_size, vk::MemoryMapFlags::empty())
            } {
                Ok(p) => p as *mut u8,
                Err(e) => {
                    unsafe {
                        self.device.free_memory(memory, None);
                        self.device.destroy_buffer(buffer, None);
                    }
                    return Err(format!("map_memory: {e}"));
                }
            };

            Ok(ImportedBuffer {
                buffer,
                memory,
                mapped_ptr,
            })
        }

        pub fn destroy_imported(&self, buffer: vk::Buffer, memory: vk::DeviceMemory) {
            unsafe {
                self.device.unmap_memory(memory);
                self.device.destroy_buffer(buffer, None);
                self.device.free_memory(memory, None);
            }
        }
    }

    impl Drop for SurfaceShareVulkanDevice {
        fn drop(&mut self) {
            unsafe {
                let _ = self.device.device_wait_idle();
                self.device.destroy_device(None);
                self.instance.destroy_instance(None);
            }
        }
    }
}

#[cfg(target_os = "linux")]
mod surface_client {
    //! Linux handle consumer client (Deno twin of `streamlib-python-native`'s).
    //!
    //! Speaks the same Unix-socket + SCM_RIGHTS wire protocol as the Python
    //! shim, with `sldn_` prefix. Consumer-only per the subprocess-import-only
    //! safety posture — subprocess allocation goes through the host via #325
    //! escalate IPC.
    use std::collections::HashMap;
    use std::ffi::{c_char, CStr};
    use std::os::unix::io::RawFd;
    use std::os::unix::net::UnixStream;
    use std::sync::{Arc, Mutex};

    use super::surface_share_vulkan_linux::SurfaceShareVulkanDevice;
    use super::gpu_surface::{SurfaceHandle, SURFACE_BACKEND_NONE};

    use vulkanalia::vk::{self, Handle as _};

    const MAX_RESOLVE_CACHE: usize = 128;

    struct CachedSurface {
        fds: Vec<RawFd>,
        plane_sizes: Vec<u64>,
        plane_offsets: Vec<u64>,
        width: u32,
        height: u32,
        bytes_per_row: u32,
        size: u64,
    }

    impl Drop for CachedSurface {
        fn drop(&mut self) {
            for fd in &self.fds {
                if *fd >= 0 {
                    unsafe { libc::close(*fd) };
                }
            }
        }
    }

    pub struct SurfaceShareHandle {
        socket_path: String,
        runtime_id: String,
        connection: Mutex<Option<UnixStream>>,
        resolve_cache: Mutex<HashMap<String, CachedSurface>>,
        vulkan_device: Mutex<Option<Arc<SurfaceShareVulkanDevice>>>,
    }

    impl SurfaceShareHandle {
        fn lazy_connect(
            &self,
        ) -> std::io::Result<std::sync::MutexGuard<'_, Option<UnixStream>>> {
            let mut guard = self.connection.lock().expect("poisoned");
            if guard.is_none() {
                let stream = UnixStream::connect(&self.socket_path)?;
                stream.set_nonblocking(false)?;
                *guard = Some(stream);
            }
            Ok(guard)
        }

        fn get_or_init_vulkan_device(&self) -> Option<Arc<SurfaceShareVulkanDevice>> {
            let mut guard = self.vulkan_device.lock().expect("poisoned");
            if let Some(d) = guard.as_ref() {
                return Some(Arc::clone(d));
            }
            match SurfaceShareVulkanDevice::try_new() {
                Ok(d) => {
                    let cloned = Arc::clone(&d);
                    *guard = Some(d);
                    Some(cloned)
                }
                Err(e) => {
                    tracing::error!(
                        "handle: failed to create Vulkan device for DMA-BUF import: {}. \
                         resolve_surface will fail — the subprocess cannot map surface-share-published \
                         surfaces without a Vulkan-capable driver.",
                        e
                    );
                    None
                }
            }
        }
    }

    // Wire helpers come from the shared `streamlib-surface-client` crate so the
    // handle server and every polyglot cdylib speak a single-sourced protocol.
    // Aliased as `wire` here to preserve the original call-site shape.
    use streamlib_surface_client as wire;

    fn c_str_to_string(ptr: *const c_char) -> Option<String> {
        if ptr.is_null() {
            return None;
        }
        unsafe { CStr::from_ptr(ptr) }
            .to_str()
            .ok()
            .map(|s| s.to_string())
    }

    fn bytes_per_pixel_from_format(format_str: &str) -> u32 {
        match format_str {
            "Bgra32" | "Rgba32" | "Argb32" => 4,
            "Rgba64" => 8,
            "Gray8" => 1,
            "Yuyv422" | "Uyvy422" => 2,
            "Nv12VideoRange" | "Nv12FullRange" => 1,
            _ => 4,
        }
    }

    /// Deno's `sldn_surface_connect` FFI is single-arg today (no runtime_id).
    /// Stamp a deterministic-but-unique runtime_id from the process id + a
    /// monotonic counter so handle logs distinguish subprocess instances.
    fn default_runtime_id() -> String {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
        format!("deno-subprocess-{}-{}", std::process::id(), seq)
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_surface_connect(socket_path: *const c_char) -> *mut SurfaceShareHandle {
        let socket_path = match c_str_to_string(socket_path) {
            Some(s) if !s.is_empty() => s,
            _ => {
                tracing::error!("surface_connect (linux): null or empty socket path");
                return std::ptr::null_mut();
            }
        };
        let runtime_id = default_runtime_id();

        tracing::error!(
            "surface_connect (linux): registered socket_path='{}' runtime_id='{}' \
             (lazy; will connect on first resolve_surface)",
            socket_path, runtime_id
        );

        Box::into_raw(Box::new(SurfaceShareHandle {
            socket_path,
            runtime_id,
            connection: Mutex::new(None),
            resolve_cache: Mutex::new(HashMap::new()),
            vulkan_device: Mutex::new(None),
        }))
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_surface_disconnect(handle: *mut SurfaceShareHandle) {
        if !handle.is_null() {
            let _ = unsafe { Box::from_raw(handle) };
        }
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_surface_resolve_surface(
        handle: *mut SurfaceShareHandle,
        pool_id: *const c_char,
    ) -> *mut SurfaceHandle {
        let handle = match unsafe { handle.as_ref() } {
            Some(b) => b,
            None => {
                tracing::error!("surface_resolve_surface (linux): null handle");
                return std::ptr::null_mut();
            }
        };
        let pool_id_str = match c_str_to_string(pool_id) {
            Some(s) if !s.is_empty() => s,
            _ => {
                tracing::error!("surface_resolve_surface (linux): null or empty pool_id");
                return std::ptr::null_mut();
            }
        };

        // Lazy-create the per-handle Vulkan device before either path returns
        // a SurfaceHandle. Every handle carries an Arc<SurfaceShareVulkanDevice> so
        // [`sldn_gpu_surface_lock`] can import without plumbing the handle
        // pointer through the FFI surface.
        let vulkan_device = match handle.get_or_init_vulkan_device() {
            Some(d) => d,
            None => return std::ptr::null_mut(),
        };

        {
            let cache = handle.resolve_cache.lock().expect("poisoned");
            if let Some(cached) = cache.get(&pool_id_str) {
                let mut dup_fds: Vec<RawFd> = Vec::with_capacity(cached.fds.len());
                let mut dup_ok = true;
                for fd in &cached.fds {
                    let dup = unsafe { libc::dup(*fd) };
                    if dup < 0 {
                        tracing::error!(
                            "surface_resolve_surface: dup cached fd failed for '{}': {}",
                            pool_id_str,
                            std::io::Error::last_os_error()
                        );
                        dup_ok = false;
                        break;
                    }
                    dup_fds.push(dup);
                }
                if !dup_ok {
                    for fd in &dup_fds {
                        unsafe { libc::close(*fd) };
                    }
                    return std::ptr::null_mut();
                }
                let n_planes = dup_fds.len();
                return Box::into_raw(Box::new(SurfaceHandle {
                    fds: dup_fds,
                    plane_sizes: cached.plane_sizes.clone(),
                    plane_offsets: cached.plane_offsets.clone(),
                    width: cached.width,
                    height: cached.height,
                    bytes_per_row: cached.bytes_per_row,
                    size: cached.size,
                    mapped_ptr: std::ptr::null_mut(),
                    plane_mapped_ptrs: vec![std::ptr::null_mut(); n_planes],
                    is_locked: false,
                    vulkan_device: Some(Arc::clone(&vulkan_device)),
                    vulkan_buffer: vk::Buffer::null(),
                    vulkan_memory: vk::DeviceMemory::null(),
                    backend: SURFACE_BACKEND_NONE,
                }));
            }
        }

        let guard = match handle.lazy_connect() {
            Ok(g) => g,
            Err(e) => {
                tracing::error!(
                    "surface_resolve_surface: connect to '{}' failed: {}. \
                     The parent StreamRuntime owns this socket; check the runtime logs \
                     and confirm STREAMLIB_SURFACE_SOCKET (or legacy \
                     STREAMLIB_BROKER_SOCKET) points at a live runtime.",
                    handle.socket_path, e
                );
                return std::ptr::null_mut();
            }
        };
        let stream = guard.as_ref().expect("connection just populated");

        let request = serde_json::json!({
            "op": "check_out",
            "surface_id": pool_id_str,
        });
        let (response, received_fds) = match wire::send_request_with_fds(
            stream,
            &request,
            &[],
            wire::MAX_DMA_BUF_PLANES,
        ) {
            Ok(r) => r,
            Err(e) => {
                tracing::error!(
                    "surface_resolve_surface: check_out for '{}' failed: {}",
                    pool_id_str, e
                );
                return std::ptr::null_mut();
            }
        };
        if let Some(err) = response.get("error").and_then(|v| v.as_str()) {
            tracing::error!(
                "surface_resolve_surface: handle error for '{}': {}",
                pool_id_str, err
            );
            for fd in &received_fds {
                unsafe { libc::close(*fd) };
            }
            return std::ptr::null_mut();
        }

        if received_fds.is_empty() {
            tracing::error!(
                "surface_resolve_surface: no DMA-BUF fd for '{}'",
                pool_id_str
            );
            return std::ptr::null_mut();
        }

        let width = response.get("width").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
        let height = response.get("height").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
        let format_str = response
            .get("format")
            .and_then(|v| v.as_str())
            .unwrap_or("Bgra32");
        let bpp = bytes_per_pixel_from_format(format_str);
        let bytes_per_row = width.saturating_mul(bpp);

        // Zero size = "unknown" → use the width*bpp*height fallback so the
        // Vulkan import path still has a byte count.
        let fallback_total = (height as u64).saturating_mul(bytes_per_row as u64);
        let plane_sizes: Vec<u64> = {
            let raw: Option<Vec<u64>> = response
                .get("plane_sizes")
                .and_then(|v| v.as_array())
                .map(|arr| arr.iter().filter_map(|v| v.as_u64()).collect());
            match raw {
                Some(v) if v.len() == received_fds.len() => v
                    .into_iter()
                    .map(|s| if s == 0 { fallback_total } else { s })
                    .collect(),
                _ => vec![fallback_total; received_fds.len()],
            }
        };
        let plane_offsets: Vec<u64> = response
            .get("plane_offsets")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_u64()).collect())
            .filter(|v: &Vec<u64>| v.len() == received_fds.len())
            .unwrap_or_else(|| vec![0u64; received_fds.len()]);
        let size: u64 = plane_sizes.iter().copied().sum();

        let mut cache_fds: Vec<RawFd> = Vec::with_capacity(received_fds.len());
        let mut cache_dup_ok = true;
        for fd in &received_fds {
            let dup = unsafe { libc::dup(*fd) };
            if dup < 0 {
                cache_dup_ok = false;
                break;
            }
            cache_fds.push(dup);
        }
        if cache_dup_ok {
            let mut cache = handle.resolve_cache.lock().expect("poisoned");
            if cache.len() >= MAX_RESOLVE_CACHE {
                tracing::error!(
                    "handle resolve cache exceeded {} entries, dropping all cached fds",
                    MAX_RESOLVE_CACHE
                );
                cache.clear();
            }
            cache.insert(
                pool_id_str.clone(),
                CachedSurface {
                    fds: cache_fds,
                    plane_sizes: plane_sizes.clone(),
                    plane_offsets: plane_offsets.clone(),
                    width,
                    height,
                    bytes_per_row,
                    size,
                },
            );
        } else {
            for fd in &cache_fds {
                unsafe { libc::close(*fd) };
            }
        }

        let n_planes = received_fds.len();
        Box::into_raw(Box::new(SurfaceHandle {
            fds: received_fds,
            plane_sizes,
            plane_offsets,
            width,
            height,
            bytes_per_row,
            size,
            mapped_ptr: std::ptr::null_mut(),
            plane_mapped_ptrs: vec![std::ptr::null_mut(); n_planes],
            is_locked: false,
            vulkan_device: Some(vulkan_device),
            vulkan_buffer: vk::Buffer::null(),
            vulkan_memory: vk::DeviceMemory::null(),
            backend: SURFACE_BACKEND_NONE,
        }))
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_surface_acquire_surface(
        _handle: *mut SurfaceShareHandle,
        _width: u32,
        _height: u32,
        _bytes_per_element: u32,
        _out_pool_id: *mut c_char,
        _pool_id_buf_len: u32,
    ) -> *mut SurfaceHandle {
        tracing::error!(
            "surface_acquire_surface: not supported on Linux; subprocess allocation must \
             escalate to the host (acquire_pixel_buffer / acquire_texture over the stdio IPC) — \
             the subprocess then calls resolve_surface with the returned handle_id."
        );
        std::ptr::null_mut()
    }

    /// Linux-only companion for `sldn_surface_resolve_surface`.
    ///
    /// Evicts the local cache entry for `pool_id` and sends a best-effort
    /// `release` op to the handle so it can drop its dup of the DMA-BUF FD.
    /// macOS doesn't ship an equivalent `sldn_surface_unregister_surface`; the
    /// XPC path relies on connection-close to release refs. The Linux handle
    /// behaves the same way at socket-close, but an explicit release
    /// shortens the lifetime window between subprocess handle drop and handle
    /// GC tick (see `prune_dead_runtimes` in the handle).
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_surface_unregister_surface(
        handle: *mut SurfaceShareHandle,
        pool_id: *const c_char,
    ) {
        let handle = match unsafe { handle.as_ref() } {
            Some(b) => b,
            None => return,
        };
        let pool_id_str = match c_str_to_string(pool_id) {
            Some(s) if !s.is_empty() => s,
            _ => return,
        };

        {
            let mut cache = handle.resolve_cache.lock().expect("poisoned");
            let _ = cache.remove(&pool_id_str);
        }

        let guard = match handle.lazy_connect() {
            Ok(g) => g,
            Err(_) => return,
        };
        if let Some(stream) = guard.as_ref() {
            let request = serde_json::json!({
                "op": "release",
                "surface_id": pool_id_str,
                "runtime_id": handle.runtime_id,
            });
            let _ = wire::send_request_with_fds(stream, &request, &[], 0);
        }
    }

    // Back-compat aliases for the legacy `sldn_broker_*` FFI names.
    // Every alias forwards to its canonical `sldn_surface_*` counterpart; the
    // symbol ships for one release cycle so Python/Deno apps pinned to the old
    // name keep working. Rust callers (tests, examples) see a `#[deprecated]`
    // warning. See `docs/migration/broker-to-surface-share.md` for the removal
    // plan.

    #[deprecated(note = "renamed to `sldn_surface_connect`; `sldn_broker_connect` is kept for one release cycle — see docs/migration/broker-to-surface-share.md")]
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_broker_connect(socket_path: *const c_char) -> *mut SurfaceShareHandle {
        unsafe { sldn_surface_connect(socket_path) }
    }

    #[deprecated(note = "renamed to `sldn_surface_disconnect`; `sldn_broker_disconnect` is kept for one release cycle — see docs/migration/broker-to-surface-share.md")]
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_broker_disconnect(handle: *mut SurfaceShareHandle) {
        unsafe { sldn_surface_disconnect(handle) }
    }

    #[deprecated(note = "renamed to `sldn_surface_resolve_surface`; `sldn_broker_resolve_surface` is kept for one release cycle — see docs/migration/broker-to-surface-share.md")]
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_broker_resolve_surface(handle: *mut SurfaceShareHandle,
        pool_id: *const c_char) -> *mut SurfaceHandle {
        unsafe { sldn_surface_resolve_surface(handle, pool_id) }
    }

    #[deprecated(note = "renamed to `sldn_surface_acquire_surface`; `sldn_broker_acquire_surface` is kept for one release cycle — see docs/migration/broker-to-surface-share.md")]
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_broker_acquire_surface(_handle: *mut SurfaceShareHandle,
        _width: u32,
        _height: u32,
        _bytes_per_element: u32,
        _out_pool_id: *mut c_char,
        _pool_id_buf_len: u32) -> *mut SurfaceHandle {
        unsafe { sldn_surface_acquire_surface(_handle, _width, _height, _bytes_per_element, _out_pool_id, _pool_id_buf_len) }
    }

    #[deprecated(note = "renamed to `sldn_surface_unregister_surface`; `sldn_broker_unregister_surface` is kept for one release cycle — see docs/migration/broker-to-surface-share.md")]
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_broker_unregister_surface(handle: *mut SurfaceShareHandle,
        pool_id: *const c_char) {
        unsafe { sldn_surface_unregister_surface(handle, pool_id) }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod surface_client {
    use std::ffi::{c_char, c_void};

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_surface_connect(_xpc_service_name: *const c_char) -> *mut c_void {
        tracing::error!("Surface-share operations not supported on this platform");
        std::ptr::null_mut()
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_surface_disconnect(_handle: *mut c_void) {}

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_surface_resolve_surface(
        _handle: *mut c_void,
        _pool_id: *const c_char,
    ) -> *mut c_void {
        std::ptr::null_mut()
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_surface_acquire_surface(
        _handle: *mut c_void,
        _width: u32,
        _height: u32,
        _bytes_per_element: u32,
        _out_pool_id: *mut c_char,
        _pool_id_buf_len: u32,
    ) -> *mut c_void {
        std::ptr::null_mut()
    }

    // Back-compat aliases for the legacy `sldn_broker_*` FFI names.
    // Every alias forwards to its canonical `sldn_surface_*` counterpart; the
    // symbol ships for one release cycle so Python/Deno apps pinned to the old
    // name keep working. Rust callers (tests, examples) see a `#[deprecated]`
    // warning. See `docs/migration/broker-to-surface-share.md` for the removal
    // plan.

    #[deprecated(note = "renamed to `sldn_surface_connect`; `sldn_broker_connect` is kept for one release cycle — see docs/migration/broker-to-surface-share.md")]
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_broker_connect(_xpc_service_name: *const c_char) -> *mut c_void {
        unsafe { sldn_surface_connect(_xpc_service_name) }
    }

    #[deprecated(note = "renamed to `sldn_surface_disconnect`; `sldn_broker_disconnect` is kept for one release cycle — see docs/migration/broker-to-surface-share.md")]
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_broker_disconnect(_handle: *mut c_void) {
        unsafe { sldn_surface_disconnect(_handle) }
    }

    #[deprecated(note = "renamed to `sldn_surface_resolve_surface`; `sldn_broker_resolve_surface` is kept for one release cycle — see docs/migration/broker-to-surface-share.md")]
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_broker_resolve_surface(_handle: *mut c_void,
        _pool_id: *const c_char) -> *mut c_void {
        unsafe { sldn_surface_resolve_surface(_handle, _pool_id) }
    }

    #[deprecated(note = "renamed to `sldn_surface_acquire_surface`; `sldn_broker_acquire_surface` is kept for one release cycle — see docs/migration/broker-to-surface-share.md")]
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_broker_acquire_surface(_handle: *mut c_void,
        _width: u32,
        _height: u32,
        _bytes_per_element: u32,
        _out_pool_id: *mut c_char,
        _pool_id_buf_len: u32) -> *mut c_void {
        unsafe { sldn_surface_acquire_surface(_handle, _width, _height, _bytes_per_element, _out_pool_id, _pool_id_buf_len) }
    }
}

// ============================================================================
// Helpers
// ============================================================================

unsafe fn c_str_to_str<'a>(ptr: *const c_char) -> Option<&'a str> {
    if ptr.is_null() {
        return None;
    }
    unsafe { CStr::from_ptr(ptr) }.to_str().ok()
}


