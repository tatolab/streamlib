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
use streamlib_ipc_types::FramePayload;

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
    subscriber: Subscriber<ipc::Service, FramePayload, ()>,
    /// Buffered payloads per port name (after poll).
    pending: HashMap<String, Vec<(Vec<u8>, i64)>>,
}

struct PublisherState {
    publisher: Publisher<ipc::Service, FramePayload, ()>,
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
#[no_mangle]
pub unsafe extern "C" fn sldn_context_create(
    processor_id: *const c_char,
) -> *mut DenoNativeContext {
    let id = if processor_id.is_null() {
        "unknown"
    } else {
        CStr::from_ptr(processor_id).to_str().unwrap_or("unknown")
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
#[no_mangle]
pub unsafe extern "C" fn sldn_context_destroy(ctx: *mut DenoNativeContext) {
    if !ctx.is_null() {
        let _ = Box::from_raw(ctx);
    }
}

/// Get current monotonic time in nanoseconds.
#[no_mangle]
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
#[no_mangle]
pub unsafe extern "C" fn sldn_input_subscribe(
    ctx: *mut DenoNativeContext,
    service_name: *const c_char,
) -> i32 {
    let ctx = match ctx.as_mut() {
        Some(c) => c,
        None => return -1,
    };
    let service_name = match c_str_to_str(service_name) {
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
        .publish_subscribe::<FramePayload>()
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
#[no_mangle]
pub unsafe extern "C" fn sldn_input_set_read_mode(
    ctx: *mut DenoNativeContext,
    port_name: *const c_char,
    mode: i32,
) -> i32 {
    let ctx = match ctx.as_mut() {
        Some(c) => c,
        None => return -1,
    };
    let port_name = match c_str_to_str(port_name) {
        Some(s) => s,
        None => return -1,
    };

    ctx.port_read_modes.insert(port_name.to_string(), mode);
    0
}

/// Poll all subscribed services for new data.
///
/// Returns 1 if any data was received, 0 if none, -1 on error.
#[no_mangle]
pub unsafe extern "C" fn sldn_input_poll(ctx: *mut DenoNativeContext) -> i32 {
    let ctx = match ctx.as_mut() {
        Some(c) => c,
        None => return -1,
    };

    let mut has_data = false;

    for (_service_name, state) in ctx.subscribers.iter_mut() {
        while let Ok(Some(sample)) = state.subscriber.receive() {
            let payload = &*sample;
            let port_name = payload.port().to_string();
            let data = payload.data().to_vec();
            let ts = payload.timestamp_ns;

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
#[no_mangle]
pub unsafe extern "C" fn sldn_input_read(
    ctx: *mut DenoNativeContext,
    port_name: *const c_char,
    out_buf: *mut u8,
    buf_len: u32,
    out_len: *mut u32,
    out_ts: *mut i64,
) -> i32 {
    let ctx = match ctx.as_mut() {
        Some(c) => c,
        None => return -1,
    };
    let port_name = match c_str_to_str(port_name) {
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
                std::ptr::copy_nonoverlapping(data.as_ptr(), out_buf, copy_len);
            }
            if !out_len.is_null() {
                *out_len = data.len() as u32;
            }
            if !out_ts.is_null() {
                *out_ts = ts;
            }
            return 0;
        }
    }

    // No data available
    if !out_len.is_null() {
        *out_len = 0;
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
#[no_mangle]
pub unsafe extern "C" fn sldn_output_publish(
    ctx: *mut DenoNativeContext,
    service_name: *const c_char,
    port_name: *const c_char,
    dest_port: *const c_char,
    schema_name: *const c_char,
) -> i32 {
    let ctx = match ctx.as_mut() {
        Some(c) => c,
        None => return -1,
    };
    let service_name = match c_str_to_str(service_name) {
        Some(s) => s,
        None => return -1,
    };
    let port_name = match c_str_to_str(port_name) {
        Some(s) => s,
        None => return -1,
    };
    let dest_port_str = match c_str_to_str(dest_port) {
        Some(s) => s,
        None => return -1,
    };
    let schema = match c_str_to_str(schema_name) {
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
        .publish_subscribe::<FramePayload>()
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

    let publisher = match service.publisher_builder().create() {
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
#[no_mangle]
pub unsafe extern "C" fn sldn_output_write(
    ctx: *mut DenoNativeContext,
    port_name: *const c_char,
    data: *const u8,
    data_len: u32,
    timestamp_ns: i64,
) -> i32 {
    let ctx = match ctx.as_mut() {
        Some(c) => c,
        None => return -1,
    };
    let port_name = match c_str_to_str(port_name) {
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
        std::slice::from_raw_parts(data, data_len as usize)
    };

    let sample = match state.publisher.loan_uninit() {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "[sldn:{}] Failed to loan sample for port '{}': {}",
                ctx.processor_id, port_name, e
            );
            return -1;
        }
    };

    let sample = sample.write_payload(FramePayload::new(
        &state.dest_port,
        &state.schema_name,
        timestamp_ns,
        data_slice,
    ));

    if let Err(e) = sample.send() {
        eprintln!(
            "[sldn:{}] Failed to send sample for port '{}': {}",
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

    #[no_mangle]
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

    #[no_mangle]
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

    #[no_mangle]
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

    #[no_mangle]
    pub unsafe extern "C" fn sldn_gpu_surface_base_address(
        handle: *const SurfaceHandle,
    ) -> *mut u8 {
        match handle.as_ref() {
            Some(h) => h.base_address,
            None => std::ptr::null_mut(),
        }
    }

    #[no_mangle]
    pub unsafe extern "C" fn sldn_gpu_surface_width(handle: *const SurfaceHandle) -> u32 {
        handle.as_ref().map(|h| h.width).unwrap_or(0)
    }

    #[no_mangle]
    pub unsafe extern "C" fn sldn_gpu_surface_height(handle: *const SurfaceHandle) -> u32 {
        handle.as_ref().map(|h| h.height).unwrap_or(0)
    }

    #[no_mangle]
    pub unsafe extern "C" fn sldn_gpu_surface_bytes_per_row(handle: *const SurfaceHandle) -> u32 {
        handle.as_ref().map(|h| h.bytes_per_row).unwrap_or(0)
    }

    #[no_mangle]
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

    #[no_mangle]
    pub unsafe extern "C" fn sldn_gpu_surface_get_id(handle: *const SurfaceHandle) -> u32 {
        handle.as_ref().map(|h| h.surface_id).unwrap_or(0)
    }

    #[no_mangle]
    pub unsafe extern "C" fn sldn_gpu_surface_release(handle: *mut SurfaceHandle) {
        if !handle.is_null() {
            let h = Box::from_raw(handle);
            IOSurfaceDecrementUseCount(h.surface_ref);
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod gpu_surface {
    #[no_mangle]
    pub unsafe extern "C" fn sldn_gpu_surface_lookup(_iosurface_id: u32) -> *mut std::ffi::c_void {
        eprintln!("[sldn] GPU surface operations not supported on this platform");
        std::ptr::null_mut()
    }

    #[no_mangle]
    pub unsafe extern "C" fn sldn_gpu_surface_lock(
        _handle: *mut std::ffi::c_void,
        _read_only: i32,
    ) -> i32 {
        -1
    }

    #[no_mangle]
    pub unsafe extern "C" fn sldn_gpu_surface_unlock(
        _handle: *mut std::ffi::c_void,
        _read_only: i32,
    ) -> i32 {
        -1
    }

    #[no_mangle]
    pub unsafe extern "C" fn sldn_gpu_surface_base_address(
        _handle: *const std::ffi::c_void,
    ) -> *mut u8 {
        std::ptr::null_mut()
    }

    #[no_mangle]
    pub unsafe extern "C" fn sldn_gpu_surface_width(_handle: *const std::ffi::c_void) -> u32 {
        0
    }

    #[no_mangle]
    pub unsafe extern "C" fn sldn_gpu_surface_height(_handle: *const std::ffi::c_void) -> u32 {
        0
    }

    #[no_mangle]
    pub unsafe extern "C" fn sldn_gpu_surface_bytes_per_row(
        _handle: *const std::ffi::c_void,
    ) -> u32 {
        0
    }

    #[no_mangle]
    pub unsafe extern "C" fn sldn_gpu_surface_create(
        _width: u32,
        _height: u32,
        _bytes_per_element: u32,
    ) -> *mut std::ffi::c_void {
        eprintln!("[sldn] GPU surface creation not supported on this platform");
        std::ptr::null_mut()
    }

    #[no_mangle]
    pub unsafe extern "C" fn sldn_gpu_surface_get_id(_handle: *const std::ffi::c_void) -> u32 {
        0
    }

    #[no_mangle]
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

    #[no_mangle]
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

    #[no_mangle]
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
    #[no_mangle]
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
    #[no_mangle]
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

#[cfg(not(target_os = "macos"))]
mod broker_client {
    use std::ffi::{c_char, c_void};

    #[no_mangle]
    pub unsafe extern "C" fn sldn_broker_connect(_xpc_service_name: *const c_char) -> *mut c_void {
        eprintln!("[sldn] Broker operations not supported on this platform");
        std::ptr::null_mut()
    }

    #[no_mangle]
    pub unsafe extern "C" fn sldn_broker_disconnect(_broker: *mut c_void) {}

    #[no_mangle]
    pub unsafe extern "C" fn sldn_broker_resolve_surface(
        _broker: *mut c_void,
        _pool_id: *const c_char,
    ) -> *mut c_void {
        std::ptr::null_mut()
    }

    #[no_mangle]
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
    CStr::from_ptr(ptr).to_str().ok()
}
