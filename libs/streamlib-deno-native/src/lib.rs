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
            eprintln!("[sldn] Failed to create context: {}", e);
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
            eprintln!(
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
            eprintln!(
                "[sldn:{}] Failed to open service '{}': {}",
                ctx.processor_id, service_name, e
            );
            return -1;
        }
    };

    let subscriber = match service.subscriber_builder().buffer_size(16).create() {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
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
                eprintln!("[sldn] received frame smaller than header ({} bytes)", buf.len());
                continue;
            }
            let header = FrameHeader::read_from_slice(buf);
            let port_name = header.port().to_string();
            let ts = header.timestamp_ns;
            let data_len = header.len as usize;
            if FRAME_HEADER_SIZE + data_len > buf.len() {
                eprintln!("[sldn] frame data truncated: header.len={} buf.len()={}", data_len, buf.len());
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
            eprintln!(
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
            eprintln!(
                "[sldn:{}] Failed to open service '{}': {}",
                ctx.processor_id, service_name, e
            );
            return -1;
        }
    };

    let publisher = match service.publisher_builder().initial_max_slice_len(max_payload_bytes + FRAME_HEADER_SIZE).create() {
        Ok(p) => p,
        Err(e) => {
            eprintln!(
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
            eprintln!(
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
            eprintln!(
                "[sldn:{}] Failed to loan slice for port '{}': {:?}",
                ctx.processor_id, port_name, e
            );
            return -1;
        }
    };
    let sample = sample.write_from_slice(&frame);
    if let Err(e) = sample.send() {
        eprintln!(
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
            eprintln!("[sldn] IOSurface not found: {}", iosurface_id);
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
            eprintln!(
                "[sldn] IOSurface lock failed: surface={}, result={}",
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
            eprintln!(
                "[sldn] IOSurfaceCreate failed: {}x{} bpe={}",
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
    //! CPU access is via `mmap(fd)` on lock; Vulkan import via
    //! `vkImportMemoryFdInfoKHR` is deliberately deferred to a follow-up so
    //! the cdylib stays minimal until a GPU-consuming Deno subprocess needs it.
    use std::ffi::c_void;
    use std::os::unix::io::RawFd;

    pub struct SurfaceHandle {
        pub fd: RawFd,
        pub width: u32,
        pub height: u32,
        pub bytes_per_row: u32,
        pub size: u64,
        pub mapped_ptr: *mut u8,
        pub is_locked: bool,
    }

    impl Drop for SurfaceHandle {
        fn drop(&mut self) {
            if self.is_locked && !self.mapped_ptr.is_null() && self.size > 0 {
                unsafe {
                    libc::munmap(self.mapped_ptr as *mut c_void, self.size as usize);
                }
            }
            if self.fd >= 0 {
                unsafe {
                    libc::close(self.fd);
                }
            }
        }
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_gpu_surface_lookup(_iosurface_id: u32) -> *mut SurfaceHandle {
        eprintln!("[sldn] GPU surface lookup by IOSurface id is macOS-only; use broker check_out");
        std::ptr::null_mut()
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_gpu_surface_lock(
        handle: *mut SurfaceHandle,
        read_only: i32,
    ) -> i32 {
        let handle = match unsafe { handle.as_mut() } {
            Some(h) => h,
            None => return -1,
        };
        if handle.is_locked {
            return 0;
        }
        if handle.size == 0 || handle.fd < 0 {
            return -1;
        }
        let prot = if read_only != 0 {
            libc::PROT_READ
        } else {
            libc::PROT_READ | libc::PROT_WRITE
        };
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                handle.size as usize,
                prot,
                libc::MAP_SHARED,
                handle.fd,
                0,
            )
        };
        if ptr == libc::MAP_FAILED {
            eprintln!(
                "[sldn] mmap on DMA-BUF fd {} failed: {}",
                handle.fd,
                std::io::Error::last_os_error()
            );
            return -1;
        }
        handle.mapped_ptr = ptr as *mut u8;
        handle.is_locked = true;
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
        let result = unsafe {
            libc::munmap(handle.mapped_ptr as *mut c_void, handle.size as usize)
        };
        handle.mapped_ptr = std::ptr::null_mut();
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
        match unsafe { handle.as_ref() } {
            Some(h) => h.mapped_ptr,
            None => std::ptr::null_mut(),
        }
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
        eprintln!(
            "[sldn] GPU surface creation in subprocess is not supported on Linux; allocation \
             must go through escalate IPC (GpuContextFullAccess -> RHI -> SurfaceStore.check_in)"
        );
        std::ptr::null_mut()
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_gpu_surface_get_id(handle: *const SurfaceHandle) -> u32 {
        // Linux surface IDs are broker UUIDs (strings), not u32 IOSurfaceIDs;
        // return the fd as a best-effort numeric token. See the Python twin
        // in streamlib-python-native for the same behavior.
        unsafe { handle.as_ref() }.map(|h| h.fd as u32).unwrap_or(0)
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
        eprintln!("[sldn] GPU surface operations not supported on this platform");
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
        eprintln!("[sldn] GPU surface creation not supported on this platform");
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
// C ABI — Broker XPC client (macOS surface resolution)
// ============================================================================

#[cfg(target_os = "macos")]
mod broker_client {
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

    /// Opaque handle to a broker XPC connection.
    pub struct BrokerHandle {
        connection: XpcConnectionT,
        resolve_cache: HashMap<String, CachedSurface>,
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_broker_connect(
        xpc_service_name: *const c_char,
    ) -> *mut BrokerHandle {
        if xpc_service_name.is_null() {
            eprintln!("[sldn] broker_connect: null service name");
            return std::ptr::null_mut();
        }

        let connection =
            xpc_connection_create_mach_service(xpc_service_name, std::ptr::null_mut(), 0);

        if connection.is_null() {
            let name = CStr::from_ptr(xpc_service_name).to_string_lossy();
            eprintln!(
                "[sldn] broker_connect: failed to create XPC connection to '{}'",
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
        eprintln!("[sldn] broker_connect: connected to '{}'", name);

        Box::into_raw(Box::new(BrokerHandle {
            connection,
            resolve_cache: HashMap::new(),
        }))
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_broker_disconnect(broker: *mut BrokerHandle) {
        if !broker.is_null() {
            let handle = Box::from_raw(broker);
            for cached in handle.resolve_cache.values() {
                IOSurfaceDecrementUseCount(cached.surface_ref);
                CFRelease(cached.surface_ref);
            }
            xpc_connection_cancel(handle.connection);
        }
    }

    /// Resolve a broker pool_id to an IOSurface handle via XPC lookup.
    ///
    /// Returns a SurfaceHandle pointer (same type as sldn_gpu_surface_lookup).
    /// Results are cached — repeated lookups for the same pool_id are fast.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_broker_resolve_surface(
        broker: *mut BrokerHandle,
        pool_id: *const c_char,
    ) -> *mut SurfaceHandle {
        let broker = match broker.as_mut() {
            Some(b) => b,
            None => {
                eprintln!("[sldn] broker_resolve_surface: null broker handle");
                return std::ptr::null_mut();
            }
        };

        let pool_id_str = match c_str_to_str(pool_id) {
            Some(s) => s,
            None => {
                eprintln!("[sldn] broker_resolve_surface: null pool_id");
                return std::ptr::null_mut();
            }
        };

        // Check resolve cache
        if let Some(cached) = broker.resolve_cache.get(pool_id_str) {
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

        // Cache miss — XPC lookup to broker
        let request = xpc_dictionary_create(std::ptr::null(), std::ptr::null(), 0);
        if request.is_null() {
            eprintln!("[sldn] broker_resolve_surface: failed to create XPC request");
            return std::ptr::null_mut();
        }

        let op_key = CString::new("op").unwrap();
        let op_value = CString::new("lookup").unwrap();
        xpc_dictionary_set_string(request, op_key.as_ptr(), op_value.as_ptr());

        let sid_key = CString::new("surface_id").unwrap();
        let sid_value = CString::new(pool_id_str).unwrap();
        xpc_dictionary_set_string(request, sid_key.as_ptr(), sid_value.as_ptr());

        let reply = xpc_connection_send_message_with_reply_sync(broker.connection, request);
        xpc_release(request);

        if reply.is_null() || xpc_is_error(reply) {
            if !reply.is_null() {
                xpc_release(reply);
            }
            eprintln!(
                "[sldn] broker_resolve_surface: XPC lookup failed for '{}'",
                pool_id_str
            );
            return std::ptr::null_mut();
        }

        // Check for error message in reply
        let error_key = CString::new("error").unwrap();
        let error_ptr = xpc_dictionary_get_string(reply, error_key.as_ptr());
        if !error_ptr.is_null() {
            let error_msg = CStr::from_ptr(error_ptr).to_string_lossy();
            eprintln!(
                "[sldn] broker_resolve_surface: broker error for '{}': {}",
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
            eprintln!(
                "[sldn] broker_resolve_surface: invalid mach port for '{}'",
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
            eprintln!(
                "[sldn] broker_resolve_surface: IOSurfaceLookupFromMachPort failed for '{}'",
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
        if broker.resolve_cache.len() >= 128 {
            for (_key, cached) in broker.resolve_cache.drain() {
                IOSurfaceDecrementUseCount(cached.surface_ref);
                CFRelease(cached.surface_ref);
            }
        }

        broker.resolve_cache.insert(
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

    /// Create a new IOSurface, register it with the broker, and return a handle.
    ///
    /// `out_pool_id` receives the broker-assigned pool UUID as a null-terminated C string.
    /// `pool_id_buf_len` is the size of the out_pool_id buffer.
    ///
    /// Returns a SurfaceHandle pointer, or null on failure.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_broker_acquire_surface(
        broker: *mut BrokerHandle,
        width: u32,
        height: u32,
        bytes_per_element: u32,
        out_pool_id: *mut c_char,
        pool_id_buf_len: u32,
    ) -> *mut SurfaceHandle {
        let broker = match broker.as_mut() {
            Some(b) => b,
            None => {
                eprintln!("[sldn] broker_acquire_surface: null broker handle");
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
            eprintln!("[sldn] broker_acquire_surface: IOSurfaceCreateMachPort failed");
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

        // Register with broker via XPC
        let request = xpc_dictionary_create(std::ptr::null(), std::ptr::null(), 0);
        if request.is_null() {
            eprintln!("[sldn] broker_acquire_surface: failed to create XPC request");
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

        let reply = xpc_connection_send_message_with_reply_sync(broker.connection, request);
        xpc_release(request);

        // Deallocate our copy of the mach port
        let task = mach_task_self();
        mach_port_deallocate(task, mach_port);

        if reply.is_null() || xpc_is_error(reply) {
            if !reply.is_null() {
                xpc_release(reply);
            }
            eprintln!("[sldn] broker_acquire_surface: XPC register failed");
            let _ = Box::from_raw(surface_handle_ptr);
            return std::ptr::null_mut();
        }

        // Check for error in reply
        let error_key = CString::new("error").unwrap();
        let error_ptr = xpc_dictionary_get_string(reply, error_key.as_ptr());
        if !error_ptr.is_null() {
            let error_msg = CStr::from_ptr(error_ptr).to_string_lossy();
            eprintln!("[sldn] broker_acquire_surface: broker error: {}", error_msg);
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
}

#[cfg(target_os = "linux")]
mod broker_client {
    //! Linux broker consumer client (Deno twin of `streamlib-python-native`'s).
    //!
    //! Speaks the same Unix-socket + SCM_RIGHTS wire protocol as the Python
    //! shim, with `sldn_` prefix. Consumer-only per the safety posture in
    //! `docs/research/polyglot-dma-buf-fd.md` — subprocess allocation goes
    //! through the host via #325 escalate IPC.
    use std::collections::HashMap;
    use std::ffi::{c_char, CStr};
    use std::os::unix::io::RawFd;
    use std::os::unix::net::UnixStream;
    use std::sync::Mutex;

    use super::gpu_surface::SurfaceHandle;

    const MAX_RESOLVE_CACHE: usize = 128;

    struct CachedSurface {
        fd: RawFd,
        width: u32,
        height: u32,
        bytes_per_row: u32,
        size: u64,
    }

    impl Drop for CachedSurface {
        fn drop(&mut self) {
            if self.fd >= 0 {
                unsafe { libc::close(self.fd) };
            }
        }
    }

    pub struct BrokerHandle {
        socket_path: String,
        runtime_id: String,
        connection: Mutex<Option<UnixStream>>,
        resolve_cache: Mutex<HashMap<String, CachedSurface>>,
    }

    impl BrokerHandle {
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
    }

    mod wire {
        use std::os::unix::io::{AsRawFd, RawFd};
        use std::os::unix::net::UnixStream;

        pub fn send_message_with_fd(
            stream: &UnixStream,
            data: &[u8],
            fd: Option<RawFd>,
        ) -> std::io::Result<()> {
            let len_bytes = (data.len() as u32).to_be_bytes();
            let mut len_iov = libc::iovec {
                iov_base: len_bytes.as_ptr() as *mut libc::c_void,
                iov_len: 4,
            };
            let mut len_msg: libc::msghdr = unsafe { std::mem::zeroed() };
            len_msg.msg_iov = &mut len_iov;
            len_msg.msg_iovlen = 1;
            let n = unsafe { libc::sendmsg(stream.as_raw_fd(), &len_msg, 0) };
            if n < 0 {
                return Err(std::io::Error::last_os_error());
            }

            let mut iov = libc::iovec {
                iov_base: data.as_ptr() as *mut libc::c_void,
                iov_len: data.len(),
            };
            let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
            msg.msg_iov = &mut iov;
            msg.msg_iovlen = 1;

            const CMSG_SPACE_SIZE: usize =
                unsafe { libc::CMSG_SPACE(std::mem::size_of::<RawFd>() as u32) } as usize;
            #[repr(C)]
            union CmsgBuf {
                buf: [u8; CMSG_SPACE_SIZE],
                _align: libc::cmsghdr,
            }
            let mut cmsg_buf = CmsgBuf {
                buf: [0u8; CMSG_SPACE_SIZE],
            };

            if let Some(send_fd) = fd {
                msg.msg_control = unsafe { cmsg_buf.buf.as_mut_ptr() } as *mut libc::c_void;
                msg.msg_controllen = CMSG_SPACE_SIZE;
                let cmsg_ptr = unsafe { libc::CMSG_FIRSTHDR(&msg) };
                if !cmsg_ptr.is_null() {
                    unsafe {
                        (*cmsg_ptr).cmsg_level = libc::SOL_SOCKET;
                        (*cmsg_ptr).cmsg_type = libc::SCM_RIGHTS;
                        (*cmsg_ptr).cmsg_len =
                            libc::CMSG_LEN(std::mem::size_of::<RawFd>() as u32) as usize;
                        let fd_ptr = libc::CMSG_DATA(cmsg_ptr) as *mut RawFd;
                        *fd_ptr = send_fd;
                    }
                    msg.msg_controllen = CMSG_SPACE_SIZE;
                }
            }

            let n = unsafe { libc::sendmsg(stream.as_raw_fd(), &msg, 0) };
            if n < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        }

        pub fn recv_message_with_fd(
            stream: &UnixStream,
            msg_len: usize,
        ) -> std::io::Result<(Vec<u8>, Option<RawFd>)> {
            const CMSG_SPACE_SIZE: usize =
                unsafe { libc::CMSG_SPACE(std::mem::size_of::<RawFd>() as u32) } as usize;
            #[repr(C)]
            union CmsgBuf {
                buf: [u8; CMSG_SPACE_SIZE],
                _align: libc::cmsghdr,
            }
            let mut cmsg_buf = CmsgBuf {
                buf: [0u8; CMSG_SPACE_SIZE],
            };

            let mut buf = vec![0u8; msg_len];
            let mut iov = libc::iovec {
                iov_base: buf.as_mut_ptr() as *mut libc::c_void,
                iov_len: msg_len,
            };
            let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
            msg.msg_iov = &mut iov;
            msg.msg_iovlen = 1;
            msg.msg_control = unsafe { cmsg_buf.buf.as_mut_ptr() } as *mut libc::c_void;
            msg.msg_controllen = CMSG_SPACE_SIZE;

            let n = unsafe { libc::recvmsg(stream.as_raw_fd(), &mut msg, 0) };
            if n < 0 {
                return Err(std::io::Error::last_os_error());
            }
            if n == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "Connection closed",
                ));
            }
            if msg.msg_flags & libc::MSG_CTRUNC != 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "SCM_RIGHTS control message truncated",
                ));
            }

            let mut total_read = n as usize;
            while total_read < msg_len {
                let remaining = &mut buf[total_read..];
                let n = unsafe {
                    libc::read(
                        stream.as_raw_fd(),
                        remaining.as_mut_ptr() as *mut libc::c_void,
                        remaining.len(),
                    )
                };
                if n <= 0 {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "Connection closed during message read",
                    ));
                }
                total_read += n as usize;
            }

            let mut received_fd = None;
            let mut cmsg_ptr = unsafe { libc::CMSG_FIRSTHDR(&msg) };
            while !cmsg_ptr.is_null() {
                let cmsg = unsafe { &*cmsg_ptr };
                if cmsg.cmsg_level == libc::SOL_SOCKET && cmsg.cmsg_type == libc::SCM_RIGHTS {
                    let fd_ptr = unsafe { libc::CMSG_DATA(cmsg_ptr) } as *const RawFd;
                    received_fd = Some(unsafe { *fd_ptr });
                }
                cmsg_ptr = unsafe { libc::CMSG_NXTHDR(&msg, cmsg_ptr) };
            }
            let _ = &buf;
            let mut read_buf = vec![0u8; msg_len];
            read_buf.copy_from_slice(&buf[..msg_len]);
            Ok((read_buf, received_fd))
        }

        pub fn send_request(
            stream: &UnixStream,
            request: &serde_json::Value,
            fd: Option<RawFd>,
        ) -> std::io::Result<(serde_json::Value, Option<RawFd>)> {
            let request_bytes = serde_json::to_vec(request).map_err(|e| {
                std::io::Error::new(
                    std::io::ErrorKind::Other,
                    format!("Failed to serialize request: {}", e),
                )
            })?;
            send_message_with_fd(stream, &request_bytes, fd)?;

            let mut len_buf = [0u8; 4];
            let mut total = 0;
            while total < 4 {
                let n = unsafe {
                    libc::read(
                        stream.as_raw_fd(),
                        len_buf[total..].as_mut_ptr() as *mut libc::c_void,
                        4 - total,
                    )
                };
                if n <= 0 {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "Failed to read response length",
                    ));
                }
                total += n as usize;
            }
            let response_len = u32::from_be_bytes(len_buf) as usize;
            let (response_bytes, response_fd) = recv_message_with_fd(stream, response_len)?;
            let response: serde_json::Value = serde_json::from_slice(&response_bytes)
                .map_err(|e| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("Invalid JSON response: {}", e),
                    )
                })?;
            Ok((response, response_fd))
        }
    }

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

    /// Deno's `sldn_broker_connect` FFI is single-arg today (no runtime_id).
    /// Stamp a deterministic-but-unique runtime_id from the process id + a
    /// monotonic counter so broker logs distinguish subprocess instances.
    fn default_runtime_id() -> String {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
        format!("deno-subprocess-{}-{}", std::process::id(), seq)
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_broker_connect(socket_path: *const c_char) -> *mut BrokerHandle {
        let socket_path = match c_str_to_string(socket_path) {
            Some(s) if !s.is_empty() => s,
            _ => {
                eprintln!("[sldn] broker_connect (linux): null or empty socket path");
                return std::ptr::null_mut();
            }
        };
        let runtime_id = default_runtime_id();

        eprintln!(
            "[sldn] broker_connect (linux): registered socket_path='{}' runtime_id='{}' \
             (lazy; will connect on first resolve_surface)",
            socket_path, runtime_id
        );

        Box::into_raw(Box::new(BrokerHandle {
            socket_path,
            runtime_id,
            connection: Mutex::new(None),
            resolve_cache: Mutex::new(HashMap::new()),
        }))
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_broker_disconnect(broker: *mut BrokerHandle) {
        if !broker.is_null() {
            let _ = unsafe { Box::from_raw(broker) };
        }
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_broker_resolve_surface(
        broker: *mut BrokerHandle,
        pool_id: *const c_char,
    ) -> *mut SurfaceHandle {
        let broker = match unsafe { broker.as_ref() } {
            Some(b) => b,
            None => {
                eprintln!("[sldn] broker_resolve_surface (linux): null broker handle");
                return std::ptr::null_mut();
            }
        };
        let pool_id_str = match c_str_to_string(pool_id) {
            Some(s) if !s.is_empty() => s,
            _ => {
                eprintln!("[sldn] broker_resolve_surface (linux): null or empty pool_id");
                return std::ptr::null_mut();
            }
        };

        {
            let cache = broker.resolve_cache.lock().expect("poisoned");
            if let Some(cached) = cache.get(&pool_id_str) {
                let dup_fd = unsafe { libc::dup(cached.fd) };
                if dup_fd < 0 {
                    eprintln!(
                        "[sldn] broker_resolve_surface: dup cached fd failed for '{}': {}",
                        pool_id_str,
                        std::io::Error::last_os_error()
                    );
                    return std::ptr::null_mut();
                }
                return Box::into_raw(Box::new(SurfaceHandle {
                    fd: dup_fd,
                    width: cached.width,
                    height: cached.height,
                    bytes_per_row: cached.bytes_per_row,
                    size: cached.size,
                    mapped_ptr: std::ptr::null_mut(),
                    is_locked: false,
                }));
            }
        }

        let guard = match broker.lazy_connect() {
            Ok(g) => g,
            Err(e) => {
                eprintln!(
                    "[sldn] broker_resolve_surface: connect to '{}' failed: {}. \
                     Is the broker running? Start it with `sudo systemctl start streamlib-broker` \
                     or via scripts/dev-setup.sh.",
                    broker.socket_path, e
                );
                return std::ptr::null_mut();
            }
        };
        let stream = guard.as_ref().expect("connection just populated");

        let request = serde_json::json!({
            "op": "check_out",
            "surface_id": pool_id_str,
        });
        let (response, received_fd) = match wire::send_request(stream, &request, None) {
            Ok(r) => r,
            Err(e) => {
                eprintln!(
                    "[sldn] broker_resolve_surface: check_out for '{}' failed: {}",
                    pool_id_str, e
                );
                return std::ptr::null_mut();
            }
        };
        if let Some(err) = response.get("error").and_then(|v| v.as_str()) {
            eprintln!(
                "[sldn] broker_resolve_surface: broker error for '{}': {}",
                pool_id_str, err
            );
            if let Some(fd) = received_fd {
                unsafe { libc::close(fd) };
            }
            return std::ptr::null_mut();
        }

        let dma_buf_fd = match received_fd {
            Some(fd) => fd,
            None => {
                eprintln!(
                    "[sldn] broker_resolve_surface: no DMA-BUF fd for '{}'",
                    pool_id_str
                );
                return std::ptr::null_mut();
            }
        };

        let width = response.get("width").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
        let height = response.get("height").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
        let format_str = response
            .get("format")
            .and_then(|v| v.as_str())
            .unwrap_or("Bgra32");
        let bpp = bytes_per_pixel_from_format(format_str);
        let bytes_per_row = width.saturating_mul(bpp);
        let size = (height as u64).saturating_mul(bytes_per_row as u64);

        let cache_fd = unsafe { libc::dup(dma_buf_fd) };
        if cache_fd >= 0 {
            let mut cache = broker.resolve_cache.lock().expect("poisoned");
            if cache.len() >= MAX_RESOLVE_CACHE {
                eprintln!(
                    "[sldn] broker resolve cache exceeded {} entries, dropping all cached fds",
                    MAX_RESOLVE_CACHE
                );
                cache.clear();
            }
            cache.insert(
                pool_id_str.clone(),
                CachedSurface {
                    fd: cache_fd,
                    width,
                    height,
                    bytes_per_row,
                    size,
                },
            );
        }

        Box::into_raw(Box::new(SurfaceHandle {
            fd: dma_buf_fd,
            width,
            height,
            bytes_per_row,
            size,
            mapped_ptr: std::ptr::null_mut(),
            is_locked: false,
        }))
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_broker_acquire_surface(
        _broker: *mut BrokerHandle,
        _width: u32,
        _height: u32,
        _bytes_per_element: u32,
        _out_pool_id: *mut c_char,
        _pool_id_buf_len: u32,
    ) -> *mut SurfaceHandle {
        eprintln!(
            "[sldn] broker_acquire_surface: not supported on Linux; subprocess allocation must \
             escalate to the host (acquire_pixel_buffer / acquire_texture over the stdio IPC) — \
             the subprocess then calls resolve_surface with the returned handle_id."
        );
        std::ptr::null_mut()
    }

    /// Linux-only companion for `sldn_broker_resolve_surface`.
    ///
    /// Evicts the local cache entry for `pool_id` and sends a best-effort
    /// `release` op to the broker so it can drop its dup of the DMA-BUF FD.
    /// macOS doesn't ship an equivalent `sldn_broker_unregister_surface`; the
    /// XPC path relies on connection-close to release refs. The Linux broker
    /// behaves the same way at socket-close, but an explicit release
    /// shortens the lifetime window between subprocess handle drop and broker
    /// GC tick (see `prune_dead_runtimes` in the broker).
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_broker_unregister_surface(
        broker: *mut BrokerHandle,
        pool_id: *const c_char,
    ) {
        let broker = match unsafe { broker.as_ref() } {
            Some(b) => b,
            None => return,
        };
        let pool_id_str = match c_str_to_string(pool_id) {
            Some(s) if !s.is_empty() => s,
            _ => return,
        };

        {
            let mut cache = broker.resolve_cache.lock().expect("poisoned");
            let _ = cache.remove(&pool_id_str);
        }

        let guard = match broker.lazy_connect() {
            Ok(g) => g,
            Err(_) => return,
        };
        if let Some(stream) = guard.as_ref() {
            let request = serde_json::json!({
                "op": "release",
                "surface_id": pool_id_str,
                "runtime_id": broker.runtime_id,
            });
            let _ = wire::send_request(stream, &request, None);
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod broker_client {
    use std::ffi::{c_char, c_void};

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_broker_connect(_xpc_service_name: *const c_char) -> *mut c_void {
        eprintln!("[sldn] Broker operations not supported on this platform");
        std::ptr::null_mut()
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_broker_disconnect(_broker: *mut c_void) {}

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_broker_resolve_surface(
        _broker: *mut c_void,
        _pool_id: *const c_char,
    ) -> *mut c_void {
        std::ptr::null_mut()
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_broker_acquire_surface(
        _broker: *mut c_void,
        _width: u32,
        _height: u32,
        _bytes_per_element: u32,
        _out_pool_id: *mut c_char,
        _pool_id_buf_len: u32,
    ) -> *mut c_void {
        std::ptr::null_mut()
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

// ============================================================================
// Tests — Linux broker consumer shim round-trip (Deno twin of the Python tests)
// ============================================================================

#[cfg(all(test, target_os = "linux"))]
mod broker_linux_tests {
    use std::ffi::CString;
    use std::os::unix::io::{FromRawFd, IntoRawFd, RawFd};
    use std::path::PathBuf;

    use streamlib_broker::{unix_socket_service, BrokerState};
    use unix_socket_service::UnixSocketSurfaceService;

    use super::broker_client::{
        sldn_broker_connect, sldn_broker_disconnect, sldn_broker_resolve_surface,
        sldn_broker_unregister_surface,
    };
    use super::gpu_surface::{
        sldn_gpu_surface_base_address, sldn_gpu_surface_bytes_per_row, sldn_gpu_surface_height,
        sldn_gpu_surface_lock, sldn_gpu_surface_release, sldn_gpu_surface_unlock,
        sldn_gpu_surface_width,
    };

    fn tmp_socket_path(label: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        p.push(format!(
            "streamlib-deno-native-test-{}-{}-{}.sock",
            label,
            std::process::id(),
            nanos
        ));
        p
    }

    fn make_memfd_with(contents: &[u8]) -> RawFd {
        use std::io::{Seek, SeekFrom, Write};

        let name = CString::new("sldn-test-memfd").unwrap();
        let fd = unsafe { libc::memfd_create(name.as_ptr(), 0) };
        assert!(
            fd >= 0,
            "memfd_create failed: {}",
            std::io::Error::last_os_error()
        );
        assert_eq!(
            unsafe { libc::ftruncate(fd, contents.len() as libc::off_t) },
            0,
            "ftruncate failed: {}",
            std::io::Error::last_os_error()
        );
        let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
        file.write_all(contents).expect("memfd write");
        file.seek(SeekFrom::Start(0)).expect("memfd rewind");
        file.into_raw_fd()
    }

    #[test]
    fn resolve_surface_mmap_readback_matches_host_pattern() {
        let state = BrokerState::new();
        let socket_path = tmp_socket_path("mmap-readback");
        let mut service = UnixSocketSurfaceService::new(state.clone(), socket_path.clone());
        service.start().expect("service start");
        std::thread::sleep(std::time::Duration::from_millis(50));

        let width = 32u32;
        let height = 4u32;
        let bpp = 4u32;
        let size = (width * height * bpp) as usize;
        let mut pattern = Vec::with_capacity(size);
        for i in 0..size {
            pattern.push(((i * 23 + 5) & 0xFF) as u8);
        }
        let send_fd = make_memfd_with(&pattern);

        let host_stream =
            unix_socket_service::connect_to_broker(&socket_path).expect("host connect");
        let check_in_req = serde_json::json!({
            "op": "check_in",
            "runtime_id": "deno-host-test",
            "width": width,
            "height": height,
            "format": "Bgra32",
            "resource_type": "pixel_buffer",
        });
        let (resp, _) = unix_socket_service::send_request(
            &host_stream,
            &check_in_req,
            Some(send_fd),
        )
        .expect("host check_in");
        unsafe { libc::close(send_fd) };
        let surface_id = resp
            .get("surface_id")
            .and_then(|v| v.as_str())
            .expect("surface_id")
            .to_string();
        drop(host_stream);

        let c_socket = CString::new(socket_path.to_str().unwrap()).unwrap();
        let broker = unsafe { sldn_broker_connect(c_socket.as_ptr()) };
        assert!(!broker.is_null());

        let c_pool_id = CString::new(surface_id.as_str()).unwrap();
        let handle = unsafe { sldn_broker_resolve_surface(broker, c_pool_id.as_ptr()) };
        assert!(!handle.is_null(), "resolve_surface returned null");

        assert_eq!(unsafe { sldn_gpu_surface_width(handle) }, width);
        assert_eq!(unsafe { sldn_gpu_surface_height(handle) }, height);
        assert_eq!(
            unsafe { sldn_gpu_surface_bytes_per_row(handle) },
            width * bpp
        );

        let rc = unsafe { sldn_gpu_surface_lock(handle, 1) };
        assert_eq!(rc, 0, "sldn_gpu_surface_lock failed");
        let base = unsafe { sldn_gpu_surface_base_address(handle) };
        assert!(!base.is_null(), "base_address null after lock");
        let mapped: &[u8] = unsafe { std::slice::from_raw_parts(base, size) };
        assert_eq!(mapped, pattern.as_slice());
        assert_eq!(unsafe { sldn_gpu_surface_unlock(handle, 1) }, 0);

        unsafe { sldn_broker_unregister_surface(broker, c_pool_id.as_ptr()) };
        unsafe { sldn_gpu_surface_release(handle) };
        unsafe { sldn_broker_disconnect(broker) };
        service.stop();
    }

    #[test]
    fn resolve_surface_fails_cleanly_on_missing_socket() {
        let bogus_path = PathBuf::from("/nonexistent/streamlib-broker-test.sock");
        let c_socket = CString::new(bogus_path.to_str().unwrap()).unwrap();
        let broker = unsafe { sldn_broker_connect(c_socket.as_ptr()) };
        assert!(
            !broker.is_null(),
            "connect should succeed lazily even for a bogus socket path"
        );

        let c_pool_id = CString::new("any-surface-id").unwrap();
        let handle = unsafe { sldn_broker_resolve_surface(broker, c_pool_id.as_ptr()) };
        assert!(
            handle.is_null(),
            "resolve_surface should fail cleanly when the socket is unreachable"
        );

        unsafe { sldn_broker_disconnect(broker) };
    }
}
