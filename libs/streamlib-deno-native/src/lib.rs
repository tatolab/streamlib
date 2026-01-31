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

/// Per-processor native context holding iceoryx2 node and port state.
pub struct DenoNativeContext {
    processor_id: String,
    node: Node<ipc::Service>,
    subscribers: HashMap<String, SubscriberState>,
    publishers: HashMap<String, PublisherState>,
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

    let subscriber = match service.subscriber_builder().create() {
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
/// Copies the oldest pending payload into the provided buffer.
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

    // Search all subscribers for pending data on this port
    for (_service_name, state) in ctx.subscribers.iter_mut() {
        if let Some(queue) = state.pending.get_mut(port_name) {
            if let Some((data, ts)) = queue.first() {
                let copy_len = data.len().min(buf_len as usize);
                if !out_buf.is_null() && copy_len > 0 {
                    std::ptr::copy_nonoverlapping(data.as_ptr(), out_buf, copy_len);
                }
                if !out_len.is_null() {
                    *out_len = data.len() as u32;
                }
                if !out_ts.is_null() {
                    *out_ts = *ts;
                }
                queue.remove(0);
                return 0;
            }
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

    let state = match ctx.publishers.get(&port_name.to_string()) {
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
        fn IOSurfaceGetWidth(buffer: IOSurfaceRef) -> usize;
        fn IOSurfaceGetHeight(buffer: IOSurfaceRef) -> usize;
        fn IOSurfaceGetBytesPerRow(buffer: IOSurfaceRef) -> usize;
        fn IOSurfaceGetBaseAddress(buffer: IOSurfaceRef) -> *mut u8;
        fn IOSurfaceLock(buffer: IOSurfaceRef, options: u32, seed: *mut u32) -> i32;
        fn IOSurfaceUnlock(buffer: IOSurfaceRef, options: u32, seed: *mut u32) -> i32;
        fn IOSurfaceIncrementUseCount(buffer: IOSurfaceRef);
        fn IOSurfaceDecrementUseCount(buffer: IOSurfaceRef);
    }

    /// Opaque handle to an IOSurface.
    pub struct SurfaceHandle {
        surface_ref: IOSurfaceRef,
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
    pub unsafe extern "C" fn sldn_gpu_surface_release(_handle: *mut std::ffi::c_void) {}
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
