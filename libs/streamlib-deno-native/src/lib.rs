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
use std::time::Duration;

use iceoryx2::port::listener::Listener;
use iceoryx2::port::notifier::Notifier;
use iceoryx2::port::publisher::Publisher;
use iceoryx2::port::subscriber::Subscriber;
use iceoryx2::prelude::*;
use streamlib_ipc_types::{FrameHeader, FRAME_HEADER_SIZE, MAX_FANIN_PER_DESTINATION};

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
    /// Single Listener for this processor's destination-paired Notify service.
    notify_listener: Option<Listener<ipc::Service>>,
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
    /// Notifier into the destination's paired Event service. Some when wired.
    notifier: Option<Notifier<ipc::Service>>,
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
            notify_listener: None,
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
        .max_publishers(MAX_FANIN_PER_DESTINATION)
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

/// Create a publisher for an iceoryx2 service, plus an optional notifier into
/// the destination's paired Event service.
///
/// `dest_port` is the destination processor's input port name, used in FramePayload routing.
/// `notify_service_name` may be the empty string or null to skip notifier setup.
/// When non-empty, `sldn_output_write` will call `notify()` after every successful `send()`.
///
/// Returns 0 on success, -1 on failure.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sldn_output_publish(
    ctx: *mut DenoNativeContext,
    service_name: *const c_char,
    port_name: *const c_char,
    dest_port: *const c_char,
    schema_name: *const c_char,
    max_payload_bytes: usize,
    notify_service_name: *const c_char,
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
        .max_publishers(MAX_FANIN_PER_DESTINATION)
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

    let notifier = match unsafe { c_str_to_str(notify_service_name) } {
        Some(name) if !name.is_empty() => match ServiceName::new(name) {
            Ok(notify_name_iox) => match ctx
                .node
                .service_builder(&notify_name_iox)
                .event()
                .max_notifiers(MAX_FANIN_PER_DESTINATION)
                .max_listeners(1)
                .open_or_create()
            {
                Ok(notify_service) => match notify_service.notifier_builder().create() {
                    Ok(n) => Some(n),
                    Err(e) => {
                        tracing::warn!(
                            "[sldn:{}] Failed to create notifier for '{}': {:?}",
                            ctx.processor_id, name, e
                        );
                        None
                    }
                },
                Err(e) => {
                    tracing::warn!(
                        "[sldn:{}] Failed to open notify service '{}': {:?}",
                        ctx.processor_id, name, e
                    );
                    None
                }
            },
            Err(e) => {
                tracing::warn!(
                    "[sldn:{}] Invalid notify service name '{}': {}",
                    ctx.processor_id, name, e
                );
                None
            }
        },
        _ => None,
    };

    ctx.publishers.insert(
        port_name.to_string(),
        PublisherState {
            publisher,
            schema_name: schema.to_string(),
            dest_port: dest_port_str.to_string(),
            notifier,
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

    if let Some(notifier) = state.notifier.as_ref()
        && let Err(e) = notifier.notify()
    {
        tracing::trace!(
            "[sldn:{}] notify() failed for port '{}': {:?}",
            ctx.processor_id, port_name, e
        );
    }

    0
}

// ============================================================================
// C ABI — Event service (Notifier/Listener for fd-multiplexed wakeups)
// ============================================================================

/// Subscribe to the destination's paired iceoryx2 Event service.
///
/// Idempotent — first call wins. Returns 0 on success, -1 on failure.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sldn_event_subscribe(
    ctx: *mut DenoNativeContext,
    notify_service_name: *const c_char,
) -> i32 {
    let ctx = match unsafe { ctx.as_mut() } {
        Some(c) => c,
        None => return -1,
    };
    if ctx.notify_listener.is_some() {
        return 0;
    }
    let name = match unsafe { c_str_to_str(notify_service_name) } {
        Some(s) if !s.is_empty() => s,
        _ => return -1,
    };
    let name_iox = match ServiceName::new(name) {
        Ok(n) => n,
        Err(e) => {
            tracing::error!(
                "[sldn:{}] Invalid notify service name '{}': {}",
                ctx.processor_id, name, e
            );
            return -1;
        }
    };
    let service = match ctx
        .node
        .service_builder(&name_iox)
        .event()
        .max_notifiers(MAX_FANIN_PER_DESTINATION)
        .max_listeners(1)
        .open_or_create()
    {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(
                "[sldn:{}] Failed to open notify service '{}': {:?}",
                ctx.processor_id, name, e
            );
            return -1;
        }
    };
    let listener = match service.listener_builder().create() {
        Ok(l) => l,
        Err(e) => {
            tracing::error!(
                "[sldn:{}] Failed to create listener for '{}': {:?}",
                ctx.processor_id, name, e
            );
            return -1;
        }
    };
    ctx.notify_listener = Some(listener);
    0
}

/// Block until a notify arrives or the timeout elapses.
///
/// Returns 1 if notified, 0 on timeout, -1 on error / no listener. Designed to
/// be invoked through Deno's `nonblocking: true` FFI option so the JS event
/// loop can stay responsive during the wait.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sldn_event_wait(
    ctx: *mut DenoNativeContext,
    timeout_ms: u32,
) -> i32 {
    let ctx = match unsafe { ctx.as_mut() } {
        Some(c) => c,
        None => return -1,
    };
    let Some(listener) = ctx.notify_listener.as_ref() else {
        return -1;
    };
    let mut woke = false;
    if let Err(e) = listener.timed_wait_all(
        |_id| {
            woke = true;
        },
        Duration::from_millis(timeout_ms as u64),
    ) {
        tracing::trace!(
            "[sldn:{}] timed_wait_all failed: {:?}",
            ctx.processor_id, e
        );
        return -1;
    }
    if woke {
        1
    } else {
        0
    }
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
    //! same shape as the host's `HostVulkanPixelBuffer::from_dma_buf_fd` so both
    //! ends speak the canonical driver-supported path. The import-side only —
    //! allocation always escalates to the host per the research doc.
    use std::os::unix::io::RawFd;
    use std::sync::Arc;

    use streamlib_consumer_rhi::{ConsumerVulkanDevice, ConsumerVulkanPixelBuffer, PixelFormat};

    /// Surface backend used for the currently-locked mapping. Reported via
    /// [`sldn_gpu_surface_backend`] so tests can assert the import took the
    /// Vulkan path rather than silently falling back.
    pub const SURFACE_BACKEND_NONE: u32 = 0;
    pub const SURFACE_BACKEND_VULKAN: u32 = 2;

    /// Map a surface-share wire format string (the Debug rendering of
    /// `PixelFormat`, e.g. `"Bgra32"`) back to the enum variant. Falls
    /// back to `Bgra32` with no warning — the metadata is only used for
    /// pixel-buffer accessor methods, not for the import itself.
    fn pixel_format_from_str(format_str: &str) -> PixelFormat {
        match format_str {
            "Bgra32" => PixelFormat::Bgra32,
            "Rgba32" => PixelFormat::Rgba32,
            "Argb32" => PixelFormat::Argb32,
            "Rgba64" => PixelFormat::Rgba64,
            "Gray8" => PixelFormat::Gray8,
            "Yuyv422" => PixelFormat::Yuyv422,
            "Uyvy422" => PixelFormat::Uyvy422,
            "Nv12VideoRange" => PixelFormat::Nv12VideoRange,
            "Nv12FullRange" => PixelFormat::Nv12FullRange,
            _ => PixelFormat::Bgra32,
        }
    }

    pub struct SurfaceHandle {
        /// One fd per DMA-BUF plane. Multi-plane DMA-BUFs (e.g. NV12 under
        /// DRM format modifiers with disjoint Y/UV allocations) carry one
        /// per plane, keyed by plane index; single-plane surfaces carry a
        /// one-element vec. Mirrors the Python-native twin.
        pub fds: Vec<RawFd>,
        /// Optional OPAQUE_FD timeline-semaphore handle the host attached
        /// when registering the surface (#531). Routed into the Vulkan
        /// adapter's `ConsumerVulkanTimelineSemaphore::from_imported_opaque_fd` so
        /// the subprocess reuses the host adapter's timeline-wait + signal
        /// path. `None` for surfaces without explicit Vulkan sync (OpenGL
        /// adapter, CPU-readback, legacy DMA-BUF consumer flows). The fd is
        /// closed when the handle is dropped, unless the import has taken
        /// ownership.
        pub sync_fd: Option<RawFd>,
        pub plane_sizes: Vec<u64>,
        pub plane_offsets: Vec<u64>,
        /// Per-plane row pitch in bytes (source-of-truth from the host's
        /// DRM-modifier-aware allocator). Required by EGL DMA-BUF import
        /// (`EGL_DMA_BUF_PLANE{N}_PITCH_EXT`).
        pub plane_strides: Vec<u64>,
        pub width: u32,
        pub height: u32,
        pub bytes_per_row: u32,
        /// Total byte size across all planes — the sum of `plane_sizes`.
        pub size: u64,
        /// DRM format modifier of the underlying host `VkImage`. Required
        /// by EGL DMA-BUF import; zero means LINEAR / not applicable.
        /// Render-target consumers MUST refuse LINEAR on NVIDIA — see
        /// `docs/learnings/nvidia-egl-dmabuf-render-target.md`.
        pub drm_format_modifier: u64,
        /// Format string from the wire response (e.g. `"Bgra8Unorm"`).
        /// Used to derive a DRM_FORMAT_* fourcc for EGL import.
        pub format: String,
        /// Host-mapped base address of plane 0, populated by `lock` (Vulkan
        /// path) or `plane_mmap` (CPU path). Multi-plane accessor reads
        /// from [`Self::plane_mapped_ptrs`].
        pub mapped_ptr: *mut u8,
        pub plane_mapped_ptrs: Vec<*mut u8>,
        pub is_locked: bool,
        /// Consumer-side Vulkan device attached by
        /// [`super::surface_client::sldn_surface_resolve_surface`].
        /// `None` means the service could not create a Vulkan device and
        /// lock will fail cleanly.
        pub vulkan_device: Option<Arc<ConsumerVulkanDevice>>,
        /// Imported pixel buffer — `Some` only while `is_locked`. Drop
        /// runs `vkDestroyBuffer` + `vkFreeMemory` via the consumer-rhi
        /// teardown path; `sldn_gpu_surface_unlock` takes() to tear down
        /// without dropping the surface handle.
        pub imported_pixel_buffer: Option<ConsumerVulkanPixelBuffer>,
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
            // Tear down any outstanding Vulkan import (lock without
            // unlock). Dropping the `ConsumerVulkanPixelBuffer` runs
            // `vkDestroyBuffer` + `vkFreeMemory`, which releases the
            // Vulkan-owned dup of `self.fds[0]` — not our fds.
            let _ = self.imported_pixel_buffer.take();
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
            // Close the sync FD if the Vulkan adapter import didn't take
            // ownership. The adapter's `register_surface` takes the fd by
            // value via `take_sync_fd` so a successful import zeros out
            // this slot before drop runs.
            if let Some(fd) = self.sync_fd {
                if fd >= 0 {
                    unsafe { libc::close(fd) };
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
        let format = pixel_format_from_str(&handle.format);
        let bytes_per_pixel = format.bits_per_pixel().div_ceil(8);
        let imported = match ConsumerVulkanPixelBuffer::from_dma_buf_fd(
            &device,
            dup_fd,
            handle.width,
            handle.height,
            bytes_per_pixel,
            format,
            plane0_size,
        ) {
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
        handle.mapped_ptr = imported.mapped_ptr();
        handle.imported_pixel_buffer = Some(imported);
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
        // Drop the imported pixel buffer — its `Drop` impl runs
        // `vkDestroyBuffer` + `vkUnmapMemory` + `vkFreeMemory` via the
        // consumer-rhi teardown path.
        let _ = handle.imported_pixel_buffer.take();
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

    /// Per-plane row pitch in bytes. Required by EGL DMA-BUF import
    /// (`EGL_DMA_BUF_PLANE{N}_PITCH_EXT`); the Vulkan import path reads
    /// from `bytes_per_row` instead. Returns `0` on null handle / out
    /// of range plane index.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_gpu_surface_plane_stride(
        handle: *const SurfaceHandle,
        plane_index: u32,
    ) -> u64 {
        unsafe { handle.as_ref() }
            .and_then(|h| h.plane_strides.get(plane_index as usize).copied())
            .unwrap_or(0)
    }

    /// Per-plane offset into its DMA-BUF fd. Returns `0` on null handle
    /// / out of range plane index.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_gpu_surface_plane_offset(
        handle: *const SurfaceHandle,
        plane_index: u32,
    ) -> u64 {
        unsafe { handle.as_ref() }
            .and_then(|h| h.plane_offsets.get(plane_index as usize).copied())
            .unwrap_or(0)
    }

    /// Per-plane DMA-BUF file descriptor, or `-1` on null handle / out of
    /// range plane index. The fd is owned by the [`SurfaceHandle`]; the
    /// caller MUST `dup()` if they need an independent copy.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_gpu_surface_plane_fd(
        handle: *const SurfaceHandle,
        plane_index: u32,
    ) -> i32 {
        unsafe { handle.as_ref() }
            .and_then(|h| h.fds.get(plane_index as usize).copied())
            .unwrap_or(-1)
    }

    /// DRM format modifier of the underlying host `VkImage`. Required by
    /// EGL DMA-BUF import; zero means LINEAR / not applicable.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_gpu_surface_drm_format_modifier(
        handle: *const SurfaceHandle,
    ) -> u64 {
        unsafe { handle.as_ref() }
            .map(|h| h.drm_format_modifier)
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

    use streamlib_consumer_rhi::ConsumerVulkanDevice;

    use super::gpu_surface::{SurfaceHandle, SURFACE_BACKEND_NONE};

    const MAX_RESOLVE_CACHE: usize = 128;

    struct CachedSurface {
        fds: Vec<RawFd>,
        plane_sizes: Vec<u64>,
        plane_offsets: Vec<u64>,
        plane_strides: Vec<u64>,
        width: u32,
        height: u32,
        bytes_per_row: u32,
        size: u64,
        drm_format_modifier: u64,
        format: String,
        /// Optional OPAQUE_FD timeline-semaphore handle the host attached at
        /// register time. Stored so cache hits can hand a fresh dup to each
        /// new `SurfaceHandle`; closed with the cache entry.
        sync_fd: Option<RawFd>,
    }

    impl Drop for CachedSurface {
        fn drop(&mut self) {
            for fd in &self.fds {
                if *fd >= 0 {
                    unsafe { libc::close(*fd) };
                }
            }
            if let Some(fd) = self.sync_fd {
                if fd >= 0 {
                    unsafe { libc::close(fd) };
                }
            }
        }
    }

    pub struct SurfaceShareHandle {
        socket_path: String,
        runtime_id: String,
        connection: Mutex<Option<UnixStream>>,
        resolve_cache: Mutex<HashMap<String, CachedSurface>>,
        /// Lazily-created per-handle consumer-side Vulkan device for
        /// DMA-BUF import. Populated on first
        /// [`sldn_surface_resolve_surface`] call; dropped with the handle.
        vulkan_device: Mutex<Option<Arc<ConsumerVulkanDevice>>>,
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

        fn get_or_init_vulkan_device(&self) -> Option<Arc<ConsumerVulkanDevice>> {
            let mut guard = self.vulkan_device.lock().expect("poisoned");
            if let Some(d) = guard.as_ref() {
                return Some(Arc::clone(d));
            }
            match ConsumerVulkanDevice::new() {
                Ok(d) => {
                    let arc = Arc::new(d);
                    *guard = Some(Arc::clone(&arc));
                    Some(arc)
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
        // a SurfaceHandle. Every handle carries an Arc<ConsumerVulkanDevice> so
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
                let cached_sync_dup: Option<RawFd> = match cached.sync_fd {
                    Some(src) => {
                        let dup = unsafe { libc::dup(src) };
                        if dup < 0 {
                            tracing::error!(
                                "surface_resolve_surface: dup cached sync_fd failed for '{}': {}",
                                pool_id_str,
                                std::io::Error::last_os_error()
                            );
                            for fd in &dup_fds {
                                unsafe { libc::close(*fd) };
                            }
                            return std::ptr::null_mut();
                        }
                        Some(dup)
                    }
                    None => None,
                };
                let n_planes = dup_fds.len();
                return Box::into_raw(Box::new(SurfaceHandle {
                    fds: dup_fds,
                    sync_fd: cached_sync_dup,
                    plane_sizes: cached.plane_sizes.clone(),
                    plane_offsets: cached.plane_offsets.clone(),
                    plane_strides: cached.plane_strides.clone(),
                    width: cached.width,
                    height: cached.height,
                    bytes_per_row: cached.bytes_per_row,
                    size: cached.size,
                    drm_format_modifier: cached.drm_format_modifier,
                    format: cached.format.clone(),
                    mapped_ptr: std::ptr::null_mut(),
                    plane_mapped_ptrs: vec![std::ptr::null_mut(); n_planes],
                    is_locked: false,
                    vulkan_device: Some(Arc::clone(&vulkan_device)),
                    imported_pixel_buffer: None,
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
                     and confirm STREAMLIB_SURFACE_SOCKET points at a live runtime.",
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
            wire::MAX_SCM_RIGHTS_FDS,
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

        // Peel off the optional trailing sync-FD (#531) — same wire shape
        // as the Python-native twin.
        let has_sync_fd = response
            .get("has_sync_fd")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let (received_fds, sync_fd): (Vec<RawFd>, Option<RawFd>) = if has_sync_fd
            && !received_fds.is_empty()
        {
            let mut all = received_fds;
            let sync = all.pop();
            (all, sync)
        } else {
            (received_fds, None)
        };

        if received_fds.is_empty() {
            tracing::error!(
                "surface_resolve_surface: no plane fds for '{}' after peeling sync_fd",
                pool_id_str
            );
            if let Some(s) = sync_fd {
                unsafe { libc::close(s) };
            }
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
        // `plane_strides` and `drm_format_modifier` are required by EGL
        // DMA-BUF import (#530); the Vulkan import path ignores them and
        // reads `bytes_per_row` instead.
        let plane_strides: Vec<u64> = response
            .get("plane_strides")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_u64()).collect())
            .filter(|v: &Vec<u64>| v.len() == received_fds.len())
            .unwrap_or_else(|| vec![bytes_per_row as u64; received_fds.len()]);
        let drm_format_modifier = response
            .get("drm_format_modifier")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
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
        let cache_sync_fd: Option<RawFd> = match sync_fd {
            Some(src) if cache_dup_ok => {
                let dup = unsafe { libc::dup(src) };
                if dup < 0 {
                    cache_dup_ok = false;
                    None
                } else {
                    Some(dup)
                }
            }
            _ => None,
        };
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
                    plane_strides: plane_strides.clone(),
                    width,
                    height,
                    bytes_per_row,
                    size,
                    drm_format_modifier,
                    format: format_str.to_string(),
                    sync_fd: cache_sync_fd,
                },
            );
        } else {
            for fd in &cache_fds {
                unsafe { libc::close(*fd) };
            }
            if let Some(fd) = cache_sync_fd {
                unsafe { libc::close(fd) };
            }
        }

        let n_planes = received_fds.len();
        Box::into_raw(Box::new(SurfaceHandle {
            fds: received_fds,
            sync_fd,
            plane_sizes,
            plane_offsets,
            plane_strides,
            width,
            height,
            bytes_per_row,
            size,
            drm_format_modifier,
            format: format_str.to_string(),
            mapped_ptr: std::ptr::null_mut(),
            plane_mapped_ptrs: vec![std::ptr::null_mut(); n_planes],
            is_locked: false,
            vulkan_device: Some(vulkan_device),
            imported_pixel_buffer: None,
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

}

// ============================================================================
// C ABI — OpenGL/EGL adapter runtime (#530, Linux)
//
// Deno twin of `streamlib-python-native::opengl`. Same shape, `sldn_`
// prefix. See the Python-side module for documentation on the wire format,
// thread-safety, and ordering invariants.
// ============================================================================

#[cfg(target_os = "linux")]
mod opengl {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use streamlib_adapter_abi::{
        StreamlibSurface, SurfaceAdapter as _, SurfaceFormat, SurfaceSyncState,
        SurfaceTransportHandle, SurfaceUsage,
    };
    use streamlib_adapter_opengl::{
        EglRuntime, HostSurfaceRegistration, OpenGlSurfaceAdapter,
        OwnedMakeCurrentGuard, DRM_FORMAT_ABGR8888, DRM_FORMAT_ARGB8888,
    };

    use super::gpu_surface::SurfaceHandle;

    pub struct OpenGlRuntimeHandle {
        egl: Arc<EglRuntime>,
        adapter: Arc<OpenGlSurfaceAdapter>,
        held: Mutex<HashMap<u64, HeldAcquire>>,
    }

    enum HeldKind {
        Read,
        Write,
    }

    struct HeldAcquire {
        kind: HeldKind,
        texture_id: u32,
        make_current: OwnedMakeCurrentGuard,
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_opengl_runtime_new() -> *mut OpenGlRuntimeHandle {
        let egl = match EglRuntime::new() {
            Ok(r) => r,
            Err(e) => {
                tracing::error!(
                    "sldn_opengl_runtime_new: EglRuntime::new failed: {}",
                    e
                );
                return std::ptr::null_mut();
            }
        };
        let adapter = Arc::new(OpenGlSurfaceAdapter::new(Arc::clone(&egl)));
        Box::into_raw(Box::new(OpenGlRuntimeHandle {
            egl,
            adapter,
            held: Mutex::new(HashMap::new()),
        }))
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_opengl_runtime_free(rt: *mut OpenGlRuntimeHandle) {
        if !rt.is_null() {
            let _ = unsafe { Box::from_raw(rt) };
        }
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_opengl_register_surface(
        rt: *mut OpenGlRuntimeHandle,
        surface_id: u64,
        gpu_handle: *const SurfaceHandle,
    ) -> i32 {
        let rt = match unsafe { rt.as_ref() } {
            Some(r) => r,
            None => {
                tracing::error!("sldn_opengl_register_surface: null runtime");
                return -1;
            }
        };
        let gpu = match unsafe { gpu_handle.as_ref() } {
            Some(g) => g,
            None => {
                tracing::error!("sldn_opengl_register_surface: null gpu_handle");
                return -1;
            }
        };
        let fd = match gpu.fds.first().copied() {
            Some(f) if f >= 0 => f,
            _ => {
                tracing::error!(
                    "sldn_opengl_register_surface: surface has no DMA-BUF fd"
                );
                return -1;
            }
        };
        let drm_fourcc = match drm_fourcc_for_format(&gpu.format) {
            Some(c) => c,
            None => {
                tracing::error!(
                    "sldn_opengl_register_surface: unsupported format '{}'",
                    gpu.format
                );
                return -1;
            }
        };
        let stride = gpu
            .plane_strides
            .first()
            .copied()
            .filter(|s| *s > 0)
            .unwrap_or(gpu.bytes_per_row as u64);
        let offset = gpu.plane_offsets.first().copied().unwrap_or(0);
        let registration = HostSurfaceRegistration {
            dma_buf_fd: fd,
            width: gpu.width,
            height: gpu.height,
            drm_fourcc,
            drm_format_modifier: gpu.drm_format_modifier,
            plane_offset: offset,
            plane_stride: stride,
        };
        match rt.adapter.register_host_surface(surface_id, registration) {
            Ok(()) => 0,
            Err(e) => {
                tracing::error!(
                    "sldn_opengl_register_surface: register_host_surface failed: {:?}",
                    e
                );
                -1
            }
        }
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_opengl_unregister_surface(
        rt: *mut OpenGlRuntimeHandle,
        surface_id: u64,
    ) -> i32 {
        let rt = match unsafe { rt.as_ref() } {
            Some(r) => r,
            None => return -1,
        };
        if rt.adapter.unregister_host_surface(surface_id) {
            0
        } else {
            -1
        }
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_opengl_acquire_write(
        rt: *mut OpenGlRuntimeHandle,
        surface_id: u64,
    ) -> u32 {
        acquire_inner(rt, surface_id, HeldKind::Write)
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_opengl_release_write(
        rt: *mut OpenGlRuntimeHandle,
        surface_id: u64,
    ) -> i32 {
        release_inner(rt, surface_id, HeldKind::Write)
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_opengl_acquire_read(
        rt: *mut OpenGlRuntimeHandle,
        surface_id: u64,
    ) -> u32 {
        acquire_inner(rt, surface_id, HeldKind::Read)
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_opengl_release_read(
        rt: *mut OpenGlRuntimeHandle,
        surface_id: u64,
    ) -> i32 {
        release_inner(rt, surface_id, HeldKind::Read)
    }

    fn acquire_inner(
        rt: *mut OpenGlRuntimeHandle,
        surface_id: u64,
        kind: HeldKind,
    ) -> u32 {
        let rt = match unsafe { rt.as_ref() } {
            Some(r) => r,
            None => {
                tracing::error!("sldn_opengl_acquire_*: null runtime");
                return 0;
            }
        };
        let make_current = match rt.egl.arc_lock_make_current() {
            Ok(g) => g,
            Err(e) => {
                tracing::error!(
                    "sldn_opengl_acquire_*: arc_lock_make_current: {}",
                    e
                );
                return 0;
            }
        };
        let surface = StreamlibSurface::new(
            surface_id,
            0,
            0,
            SurfaceFormat::Bgra8,
            SurfaceUsage::RENDER_TARGET,
            SurfaceTransportHandle::empty(),
            SurfaceSyncState::default(),
        );
        let texture_id = match kind {
            HeldKind::Write => match rt.adapter.acquire_write(&surface) {
                Ok(g) => {
                    let t = g.view().gl_texture_id();
                    std::mem::forget(g);
                    t
                }
                Err(e) => {
                    tracing::error!(
                        "sldn_opengl_acquire_write: adapter.acquire_write: {:?}",
                        e
                    );
                    return 0;
                }
            },
            HeldKind::Read => match rt.adapter.acquire_read(&surface) {
                Ok(g) => {
                    let t = g.view().gl_texture_id();
                    std::mem::forget(g);
                    t
                }
                Err(e) => {
                    tracing::error!(
                        "sldn_opengl_acquire_read: adapter.acquire_read: {:?}",
                        e
                    );
                    return 0;
                }
            },
        };
        let mut held = rt.held.lock().expect("opengl held: poisoned");
        held.insert(
            surface_id,
            HeldAcquire {
                kind,
                texture_id,
                make_current,
            },
        );
        texture_id
    }

    fn release_inner(
        rt: *mut OpenGlRuntimeHandle,
        surface_id: u64,
        expected: HeldKind,
    ) -> i32 {
        let rt = match unsafe { rt.as_ref() } {
            Some(r) => r,
            None => return -1,
        };
        let removed = {
            let mut held = rt.held.lock().expect("opengl held: poisoned");
            held.remove(&surface_id)
        };
        let held = match removed {
            Some(h) => h,
            None => {
                tracing::error!(
                    "sldn_opengl_release_*: no acquire held for surface_id {}",
                    surface_id
                );
                return -1;
            }
        };
        if !matches!((&held.kind, &expected), (HeldKind::Read, HeldKind::Read) | (HeldKind::Write, HeldKind::Write)) {
            tracing::error!(
                "sldn_opengl_release_*: surface_id {} held in different mode than \
                 release call expected — releasing it anyway",
                surface_id
            );
        }
        // CRITICAL: drop the make-current guard before calling
        // `end_*_access` (the adapter's mutex is not reentrant).
        drop(held.make_current);
        match held.kind {
            HeldKind::Read => rt.adapter.end_read_access(surface_id),
            HeldKind::Write => rt.adapter.end_write_access(surface_id),
        }
        let _ = held.texture_id;
        0
    }

    fn drm_fourcc_for_format(format: &str) -> Option<u32> {
        match format {
            "Bgra8Unorm" | "Bgra8UnormSrgb" => Some(DRM_FORMAT_ARGB8888),
            "Rgba8Unorm" | "Rgba8UnormSrgb" => Some(DRM_FORMAT_ABGR8888),
            _ => None,
        }
    }
}

#[cfg(not(target_os = "linux"))]
mod opengl {
    use std::ffi::c_void;

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_opengl_runtime_new() -> *mut c_void {
        tracing::error!("sldn_opengl_*: OpenGL adapter runtime is Linux-only");
        std::ptr::null_mut()
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_opengl_runtime_free(_rt: *mut c_void) {}

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_opengl_register_surface(
        _rt: *mut c_void,
        _surface_id: u64,
        _gpu_handle: *const c_void,
    ) -> i32 {
        -1
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_opengl_unregister_surface(
        _rt: *mut c_void,
        _surface_id: u64,
    ) -> i32 {
        -1
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_opengl_acquire_write(
        _rt: *mut c_void,
        _surface_id: u64,
    ) -> u32 {
        0
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_opengl_release_write(
        _rt: *mut c_void,
        _surface_id: u64,
    ) -> i32 {
        -1
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_opengl_acquire_read(
        _rt: *mut c_void,
        _surface_id: u64,
    ) -> u32 {
        0
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_opengl_release_read(
        _rt: *mut c_void,
        _surface_id: u64,
    ) -> i32 {
        -1
    }
}

// ============================================================================
// C ABI — Vulkan adapter runtime (#531, Linux)
//
// Subprocess-side runtime mirroring the Python-native twin's `slpn_vulkan_*`
// surface. Reuses `streamlib_adapter_vulkan::VulkanSurfaceAdapter` against a
// subprocess-local `ConsumerVulkanDevice` from the RHI: same timeline-wait, same
// layout-transition, same per-surface state machine. The cdylib never
// re-implements layout transitions, command-pool lifetimes, fence handling,
// or queue-mutex coordination — every line of that lives in
// `streamlib-adapter-vulkan`.
// ============================================================================

#[cfg(target_os = "linux")]
mod vulkan {
    use std::collections::HashMap;
    use std::os::unix::io::RawFd;
    use std::sync::{Arc, Mutex};

    use streamlib_consumer_rhi::{
        ConsumerMarker, ConsumerVulkanDevice, ConsumerVulkanTexture,
        ConsumerVulkanTimelineSemaphore, TextureFormat,
    };
    use streamlib_adapter_abi::{
        StreamlibSurface, SurfaceAdapter as _, SurfaceFormat, SurfaceSyncState,
        SurfaceTransportHandle, SurfaceUsage,
    };
    use streamlib_adapter_vulkan::{
        raw_handles, HostSurfaceRegistration, VulkanLayout, VulkanSurfaceAdapter,
    };

    use super::gpu_surface::SurfaceHandle;

    pub struct VulkanRuntimeHandle {
        device: Arc<ConsumerVulkanDevice>,
        adapter: Arc<VulkanSurfaceAdapter<ConsumerVulkanDevice>>,
        registered: Mutex<HashMap<u64, RegisteredSurface>>,
    }

    /// Tracks which surface_ids have been registered. The adapter owns
    /// the imported VkImage + timeline; this registry only exists to
    /// reject double-registers / double-unregisters at the FFI boundary.
    struct RegisteredSurface;

    #[repr(C)]
    pub struct SldnVulkanView {
        pub vk_image: u64,
        pub vk_image_layout: i32,
    }

    #[repr(C)]
    pub struct SldnVulkanRawHandles {
        pub vk_instance: u64,
        pub vk_physical_device: u64,
        pub vk_device: u64,
        pub vk_queue: u64,
        pub vk_queue_family_index: u32,
        pub api_version: u32,
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_vulkan_runtime_new() -> *mut VulkanRuntimeHandle {
        let device = match ConsumerVulkanDevice::new() {
            Ok(d) => Arc::new(d),
            Err(e) => {
                tracing::error!(
                    "sldn_vulkan_runtime_new: ConsumerVulkanDevice::new failed: {}",
                    e
                );
                return std::ptr::null_mut();
            }
        };
        let adapter = Arc::new(VulkanSurfaceAdapter::new(Arc::clone(&device)));
        Box::into_raw(Box::new(VulkanRuntimeHandle {
            device,
            adapter,
            registered: Mutex::new(HashMap::new()),
        }))
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_vulkan_runtime_free(rt: *mut VulkanRuntimeHandle) {
        if !rt.is_null() {
            let _ = unsafe { Box::from_raw(rt) };
        }
    }

    fn texture_format_from_str(format: &str) -> Option<TextureFormat> {
        match format {
            "Bgra8Unorm" => Some(TextureFormat::Bgra8Unorm),
            "Bgra8UnormSrgb" => Some(TextureFormat::Bgra8UnormSrgb),
            "Rgba8Unorm" => Some(TextureFormat::Rgba8Unorm),
            "Rgba8UnormSrgb" => Some(TextureFormat::Rgba8UnormSrgb),
            _ => None,
        }
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_vulkan_register_surface(
        rt: *mut VulkanRuntimeHandle,
        surface_id: u64,
        gpu_handle: *mut SurfaceHandle,
    ) -> i32 {
        let rt = match unsafe { rt.as_ref() } {
            Some(r) => r,
            None => {
                tracing::error!("sldn_vulkan_register_surface: null runtime");
                return -1;
            }
        };
        let gpu = match unsafe { gpu_handle.as_mut() } {
            Some(g) => g,
            None => {
                tracing::error!("sldn_vulkan_register_surface: null gpu_handle");
                return -1;
            }
        };
        if gpu.fds.is_empty() {
            tracing::error!(
                "sldn_vulkan_register_surface: surface has no DMA-BUF fds"
            );
            return -1;
        }
        if gpu.drm_format_modifier == 0 {
            tracing::error!(
                "sldn_vulkan_register_surface: surface has DRM_FORMAT_MOD_LINEAR \
                 (zero modifier) — render-target Vulkan import requires a tiled \
                 modifier; see docs/learnings/nvidia-egl-dmabuf-render-target.md"
            );
            return -1;
        }
        let texture_format = match texture_format_from_str(&gpu.format) {
            Some(f) => f,
            None => {
                tracing::error!(
                    "sldn_vulkan_register_surface: unsupported format '{}' \
                     (v1 supports Bgra8Unorm, Bgra8UnormSrgb, Rgba8Unorm, Rgba8UnormSrgb)",
                    gpu.format
                );
                return -1;
            }
        };
        let allocation_size = gpu.size;

        let texture = match ConsumerVulkanTexture::import_render_target_dma_buf(
            &rt.device,
            &gpu.fds,
            &gpu.plane_offsets,
            &gpu.plane_strides,
            gpu.drm_format_modifier,
            gpu.width,
            gpu.height,
            texture_format,
            allocation_size,
        ) {
            Ok(t) => t,
            Err(e) => {
                tracing::error!(
                    "sldn_vulkan_register_surface: import_render_target_dma_buf: {}",
                    e
                );
                return -1;
            }
        };
        let raw_sync_fd: RawFd = match gpu.sync_fd.take() {
            Some(fd) => fd,
            None => {
                tracing::error!(
                    "sldn_vulkan_register_surface: surface '{}' has no sync_fd — \
                     the host must register the texture with an exportable \
                     `ConsumerVulkanTimelineSemaphore`.",
                    surface_id
                );
                return -1;
            }
        };
        let timeline = match ConsumerVulkanTimelineSemaphore::from_imported_opaque_fd(
            &rt.device,
            raw_sync_fd,
        ) {
            Ok(s) => Arc::new(s),
            Err(e) => {
                gpu.sync_fd = Some(raw_sync_fd);
                tracing::error!(
                    "sldn_vulkan_register_surface: from_imported_opaque_fd: {}",
                    e
                );
                return -1;
            }
        };

        let registration = HostSurfaceRegistration::<ConsumerMarker> {
            texture: Arc::new(texture),
            timeline,
            initial_layout: VulkanLayout::UNDEFINED,
        };

        if let Err(e) = rt
            .adapter
            .register_host_surface(surface_id, registration)
        {
            tracing::error!(
                "sldn_vulkan_register_surface: register_host_surface failed: {:?}",
                e
            );
            return -1;
        }

        rt.registered
            .lock()
            .expect("sldn_vulkan registered: poisoned")
            .insert(surface_id, RegisteredSurface);
        0
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_vulkan_unregister_surface(
        rt: *mut VulkanRuntimeHandle,
        surface_id: u64,
    ) -> i32 {
        let rt = match unsafe { rt.as_ref() } {
            Some(r) => r,
            None => return -1,
        };
        let removed = rt
            .registered
            .lock()
            .expect("sldn_vulkan registered: poisoned")
            .remove(&surface_id);
        if removed.is_none() {
            return -1;
        }
        if rt.adapter.unregister_host_surface(surface_id) {
            0
        } else {
            -1
        }
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_vulkan_acquire_write(
        rt: *mut VulkanRuntimeHandle,
        surface_id: u64,
        out_view: *mut SldnVulkanView,
    ) -> i32 {
        acquire_inner(rt, surface_id, out_view, AcquireKind::Write)
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_vulkan_release_write(
        rt: *mut VulkanRuntimeHandle,
        surface_id: u64,
    ) -> i32 {
        release_inner(rt, surface_id, AcquireKind::Write)
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_vulkan_acquire_read(
        rt: *mut VulkanRuntimeHandle,
        surface_id: u64,
        out_view: *mut SldnVulkanView,
    ) -> i32 {
        acquire_inner(rt, surface_id, out_view, AcquireKind::Read)
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_vulkan_release_read(
        rt: *mut VulkanRuntimeHandle,
        surface_id: u64,
    ) -> i32 {
        release_inner(rt, surface_id, AcquireKind::Read)
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_vulkan_raw_handles(
        rt: *mut VulkanRuntimeHandle,
        out: *mut SldnVulkanRawHandles,
    ) -> i32 {
        let rt = match unsafe { rt.as_ref() } {
            Some(r) => r,
            None => return -1,
        };
        let out = match unsafe { out.as_mut() } {
            Some(o) => o,
            None => return -1,
        };
        let handles = raw_handles(rt.device.as_ref());
        out.vk_instance = handles.vk_instance;
        out.vk_physical_device = handles.vk_physical_device;
        out.vk_device = handles.vk_device;
        out.vk_queue = handles.vk_queue;
        out.vk_queue_family_index = handles.vk_queue_family_index;
        out.api_version = handles.api_version;
        0
    }

    #[derive(Clone, Copy)]
    enum AcquireKind {
        Read,
        Write,
    }

    fn acquire_inner(
        rt: *mut VulkanRuntimeHandle,
        surface_id: u64,
        out_view: *mut SldnVulkanView,
        kind: AcquireKind,
    ) -> i32 {
        let rt = match unsafe { rt.as_ref() } {
            Some(r) => r,
            None => {
                tracing::error!("sldn_vulkan_acquire_*: null runtime");
                return -1;
            }
        };
        let out_view = match unsafe { out_view.as_mut() } {
            Some(v) => v,
            None => {
                tracing::error!("sldn_vulkan_acquire_*: null out_view");
                return -1;
            }
        };
        let surface = StreamlibSurface::new(
            surface_id,
            0,
            0,
            SurfaceFormat::Bgra8,
            SurfaceUsage::RENDER_TARGET,
            SurfaceTransportHandle::empty(),
            SurfaceSyncState::default(),
        );
        match kind {
            AcquireKind::Write => {
                use streamlib_adapter_abi::VulkanWritable;
                match rt.adapter.acquire_write(&surface) {
                    Ok(g) => {
                        out_view.vk_image = g.view().vk_image().0;
                        out_view.vk_image_layout = g.view().vk_image_layout().0;
                        std::mem::forget(g);
                        0
                    }
                    Err(e) => {
                        tracing::error!(
                            "sldn_vulkan_acquire_write: adapter.acquire_write: {:?}",
                            e
                        );
                        -1
                    }
                }
            }
            AcquireKind::Read => {
                use streamlib_adapter_abi::VulkanWritable;
                match rt.adapter.acquire_read(&surface) {
                    Ok(g) => {
                        out_view.vk_image = g.view().vk_image().0;
                        out_view.vk_image_layout = g.view().vk_image_layout().0;
                        std::mem::forget(g);
                        0
                    }
                    Err(e) => {
                        tracing::error!(
                            "sldn_vulkan_acquire_read: adapter.acquire_read: {:?}",
                            e
                        );
                        -1
                    }
                }
            }
        }
    }

    fn release_inner(
        rt: *mut VulkanRuntimeHandle,
        surface_id: u64,
        kind: AcquireKind,
    ) -> i32 {
        let rt = match unsafe { rt.as_ref() } {
            Some(r) => r,
            None => return -1,
        };
        match kind {
            AcquireKind::Read => rt.adapter.end_read_access(surface_id),
            AcquireKind::Write => rt.adapter.end_write_access(surface_id),
        }
        0
    }
}

// ============================================================================
// C ABI — cpu-readback adapter runtime (#562, Linux)
//
// Mirror of `streamlib-python-native`'s cpu-readback module: same
// adapter, same trigger, same FFI shape; the only difference is the
// `sldn_*` prefix the Deno SDK reaches via Deno.dlopen.
// ============================================================================

#[cfg(target_os = "linux")]
mod cpu_readback {
    use std::collections::HashMap;
    use std::ffi::c_void;
    use std::os::unix::io::RawFd;
    use std::sync::{Arc, Mutex};

    use streamlib_adapter_abi::{
        AdapterError, StreamlibSurface, SurfaceAdapter as _, SurfaceFormat, SurfaceSyncState,
        SurfaceTransportHandle, SurfaceUsage,
    };
    use streamlib_adapter_cpu_readback::{
        CpuReadbackCopyTrigger, CpuReadbackSurfaceAdapter, CpuReadbackTriggerContext,
        HostSurfaceRegistration,
    };
    use streamlib_consumer_rhi::{
        ConsumerMarker, ConsumerVulkanDevice, ConsumerVulkanPixelBuffer,
        ConsumerVulkanTimelineSemaphore, PixelFormat,
    };

    use super::gpu_surface::SurfaceHandle;

    pub const SLDN_CPU_READBACK_MAX_PLANES: usize = 4;

    pub const SLDN_CPU_READBACK_DIRECTION_IMAGE_TO_BUFFER: u32 = 0;
    pub const SLDN_CPU_READBACK_DIRECTION_BUFFER_TO_IMAGE: u32 = 1;

    pub const SLDN_CPU_READBACK_OK: i32 = 0;
    pub const SLDN_CPU_READBACK_ERR: i32 = -1;
    pub const SLDN_CPU_READBACK_CONTENDED: i32 = 1;

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct SldnCpuReadbackPlane {
        pub mapped_ptr: *mut u8,
        pub width: u32,
        pub height: u32,
        pub bytes_per_pixel: u32,
        pub byte_size: u64,
    }

    #[repr(C)]
    pub struct SldnCpuReadbackView {
        pub width: u32,
        pub height: u32,
        pub format: u32,
        pub plane_count: u32,
        pub planes: [SldnCpuReadbackPlane; SLDN_CPU_READBACK_MAX_PLANES],
    }

    /// See `SlpnCpuReadbackTriggerCallback` in
    /// `streamlib-python-native` for the contract — same shape, sldn_
    /// prefix.
    pub type SldnCpuReadbackTriggerCallback = unsafe extern "C" fn(
        user_data: *mut c_void,
        surface_id: u64,
        direction: u32,
    ) -> u64;

    pub struct EscalateCpuReadbackCopyTrigger {
        callback: Mutex<Option<RegisteredCallback>>,
    }

    struct RegisteredCallback {
        callback: SldnCpuReadbackTriggerCallback,
        user_data: usize,
    }

    impl EscalateCpuReadbackCopyTrigger {
        pub fn new() -> Self {
            Self {
                callback: Mutex::new(None),
            }
        }

        pub fn install(
            &self,
            callback: SldnCpuReadbackTriggerCallback,
            user_data: *mut c_void,
        ) {
            *self.callback.lock().expect("trigger callback poisoned") = Some(RegisteredCallback {
                callback,
                user_data: user_data as usize,
            });
        }

        fn dispatch(&self, surface_id: u64, direction: u32) -> Result<u64, AdapterError> {
            let registered = self.callback.lock().expect("trigger callback poisoned");
            let entry = registered.as_ref().ok_or_else(|| AdapterError::IpcDisconnected {
                reason:
                    "cpu-readback trigger callback not installed; SDK must call \
                     sldn_cpu_readback_set_trigger_callback before any acquire"
                        .into(),
            })?;
            let value = unsafe {
                (entry.callback)(
                    entry.user_data as *mut c_void,
                    surface_id,
                    direction,
                )
            };
            if value == 0 {
                return Err(AdapterError::IpcDisconnected {
                    reason: format!(
                        "cpu-readback trigger callback returned 0 (sentinel for failure) for surface_id={surface_id} direction={direction}"
                    ),
                });
            }
            Ok(value)
        }
    }

    impl CpuReadbackCopyTrigger<ConsumerMarker> for EscalateCpuReadbackCopyTrigger {
        fn run_copy_image_to_buffer(
            &self,
            ctx: &CpuReadbackTriggerContext<'_, ConsumerMarker>,
        ) -> Result<u64, AdapterError> {
            self.dispatch(ctx.surface_id, SLDN_CPU_READBACK_DIRECTION_IMAGE_TO_BUFFER)
        }

        fn run_copy_buffer_to_image(
            &self,
            ctx: &CpuReadbackTriggerContext<'_, ConsumerMarker>,
        ) -> Result<u64, AdapterError> {
            self.dispatch(ctx.surface_id, SLDN_CPU_READBACK_DIRECTION_BUFFER_TO_IMAGE)
        }
    }

    pub struct CpuReadbackRuntimeHandle {
        device: Arc<ConsumerVulkanDevice>,
        adapter: Arc<CpuReadbackSurfaceAdapter<ConsumerVulkanDevice>>,
        trigger: Arc<EscalateCpuReadbackCopyTrigger>,
        registered: Mutex<HashMap<u64, RegisteredSurface>>,
    }

    struct RegisteredSurface {
        format: SurfaceFormat,
        width: u32,
        height: u32,
        plane_count: u32,
        plane_mapped_ptrs: Vec<*mut u8>,
        plane_widths: Vec<u32>,
        plane_heights: Vec<u32>,
        plane_bytes_per_pixel: Vec<u32>,
        plane_byte_sizes: Vec<u64>,
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_cpu_readback_runtime_new() -> *mut CpuReadbackRuntimeHandle {
        let device = match ConsumerVulkanDevice::new() {
            Ok(d) => Arc::new(d),
            Err(e) => {
                tracing::error!(
                    "sldn_cpu_readback_runtime_new: ConsumerVulkanDevice::new failed: {}",
                    e
                );
                return std::ptr::null_mut();
            }
        };
        let trigger = Arc::new(EscalateCpuReadbackCopyTrigger::new());
        let adapter = Arc::new(CpuReadbackSurfaceAdapter::new(
            Arc::clone(&device),
            Arc::clone(&trigger) as Arc<dyn CpuReadbackCopyTrigger<ConsumerMarker>>,
        ));
        Box::into_raw(Box::new(CpuReadbackRuntimeHandle {
            device,
            adapter,
            trigger,
            registered: Mutex::new(HashMap::new()),
        }))
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_cpu_readback_runtime_free(rt: *mut CpuReadbackRuntimeHandle) {
        if !rt.is_null() {
            let _ = unsafe { Box::from_raw(rt) };
        }
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_cpu_readback_set_trigger_callback(
        rt: *mut CpuReadbackRuntimeHandle,
        callback: SldnCpuReadbackTriggerCallback,
        user_data: *mut c_void,
    ) -> i32 {
        let rt = match unsafe { rt.as_ref() } {
            Some(r) => r,
            None => return SLDN_CPU_READBACK_ERR,
        };
        rt.trigger.install(callback, user_data);
        SLDN_CPU_READBACK_OK
    }

    fn surface_format_from_u32(value: u32) -> Option<SurfaceFormat> {
        match value {
            0 => Some(SurfaceFormat::Bgra8),
            1 => Some(SurfaceFormat::Rgba8),
            2 => Some(SurfaceFormat::Nv12),
            _ => None,
        }
    }

    fn pixel_format_for_plane(format: SurfaceFormat, plane: u32) -> PixelFormat {
        match (format, plane) {
            (SurfaceFormat::Bgra8, 0) => PixelFormat::Bgra32,
            (SurfaceFormat::Rgba8, 0) => PixelFormat::Rgba32,
            (SurfaceFormat::Nv12, 0) => PixelFormat::Gray8,
            (SurfaceFormat::Nv12, 1) => PixelFormat::Gray8,
            _ => PixelFormat::Unknown,
        }
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_cpu_readback_register_surface(
        rt: *mut CpuReadbackRuntimeHandle,
        surface_id: u64,
        gpu_handle: *mut SurfaceHandle,
        surface_format: u32,
    ) -> i32 {
        let rt = match unsafe { rt.as_ref() } {
            Some(r) => r,
            None => {
                tracing::error!("sldn_cpu_readback_register_surface: null runtime");
                return SLDN_CPU_READBACK_ERR;
            }
        };
        let gpu = match unsafe { gpu_handle.as_mut() } {
            Some(g) => g,
            None => {
                tracing::error!("sldn_cpu_readback_register_surface: null gpu_handle");
                return SLDN_CPU_READBACK_ERR;
            }
        };
        let format = match surface_format_from_u32(surface_format) {
            Some(f) => f,
            None => {
                tracing::error!(
                    "sldn_cpu_readback_register_surface: unknown surface_format={}",
                    surface_format
                );
                return SLDN_CPU_READBACK_ERR;
            }
        };
        let plane_count = format.plane_count() as usize;
        if gpu.fds.len() != plane_count {
            tracing::error!(
                "sldn_cpu_readback_register_surface: format {:?} requires {} plane(s); gpu_handle has {} fd(s)",
                format,
                plane_count,
                gpu.fds.len()
            );
            return SLDN_CPU_READBACK_ERR;
        }
        let surface_width = gpu.width;
        let surface_height = gpu.height;

        let mut staging_planes: Vec<Arc<ConsumerVulkanPixelBuffer>> =
            Vec::with_capacity(plane_count);
        for plane_idx in 0..plane_count {
            let plane_idx_u32 = plane_idx as u32;
            let plane_width = format.plane_width(surface_width, plane_idx_u32);
            let plane_height = format.plane_height(surface_height, plane_idx_u32);
            let plane_bpp = format.plane_bytes_per_pixel(plane_idx_u32);
            let plane_size = gpu
                .plane_sizes
                .get(plane_idx)
                .copied()
                .filter(|s| *s > 0)
                .unwrap_or_else(|| {
                    (plane_width as u64) * (plane_height as u64) * (plane_bpp as u64)
                });
            let pixel_format = pixel_format_for_plane(format, plane_idx_u32);
            let pb = match ConsumerVulkanPixelBuffer::from_dma_buf_fd(
                &rt.device,
                gpu.fds[plane_idx],
                plane_width,
                plane_height,
                plane_bpp,
                pixel_format,
                plane_size,
            ) {
                Ok(b) => Arc::new(b),
                Err(e) => {
                    tracing::error!(
                        "sldn_cpu_readback_register_surface: import plane {} fd={}: {}",
                        plane_idx,
                        gpu.fds[plane_idx],
                        e
                    );
                    return SLDN_CPU_READBACK_ERR;
                }
            };
            staging_planes.push(pb);
        }

        let raw_sync_fd: RawFd = match gpu.sync_fd.take() {
            Some(fd) => fd,
            None => {
                tracing::error!(
                    "sldn_cpu_readback_register_surface: surface '{}' has no sync_fd — \
                     the host must register it via SurfaceStore::register_pixel_buffer_with_timeline \
                     with an exportable HostVulkanTimelineSemaphore.",
                    surface_id
                );
                return SLDN_CPU_READBACK_ERR;
            }
        };
        let timeline = match ConsumerVulkanTimelineSemaphore::from_imported_opaque_fd(
            &rt.device,
            raw_sync_fd,
        ) {
            Ok(s) => Arc::new(s),
            Err(e) => {
                gpu.sync_fd = Some(raw_sync_fd);
                tracing::error!(
                    "sldn_cpu_readback_register_surface: from_imported_opaque_fd: {}",
                    e
                );
                return SLDN_CPU_READBACK_ERR;
            }
        };

        let mut plane_mapped_ptrs = Vec::with_capacity(plane_count);
        let mut plane_widths = Vec::with_capacity(plane_count);
        let mut plane_heights = Vec::with_capacity(plane_count);
        let mut plane_bytes_per_pixel = Vec::with_capacity(plane_count);
        let mut plane_byte_sizes = Vec::with_capacity(plane_count);
        for (idx, pb) in staging_planes.iter().enumerate() {
            let w = format.plane_width(surface_width, idx as u32);
            let h = format.plane_height(surface_height, idx as u32);
            let bpp = format.plane_bytes_per_pixel(idx as u32);
            plane_mapped_ptrs.push(pb.mapped_ptr());
            plane_widths.push(w);
            plane_heights.push(h);
            plane_bytes_per_pixel.push(bpp);
            plane_byte_sizes.push((w as u64) * (h as u64) * (bpp as u64));
        }

        let registration = HostSurfaceRegistration::<ConsumerMarker> {
            texture: None,
            staging_planes,
            timeline,
            initial_image_layout: vulkanalia::vk::ImageLayout::GENERAL.as_raw(),
            format,
            width: surface_width,
            height: surface_height,
        };

        if let Err(e) = rt.adapter.register_host_surface(surface_id, registration) {
            tracing::error!(
                "sldn_cpu_readback_register_surface: register_host_surface({}): {:?}",
                surface_id,
                e
            );
            return SLDN_CPU_READBACK_ERR;
        }

        rt.registered
            .lock()
            .expect("sldn_cpu_readback registered: poisoned")
            .insert(
                surface_id,
                RegisteredSurface {
                    format,
                    width: surface_width,
                    height: surface_height,
                    plane_count: plane_count as u32,
                    plane_mapped_ptrs,
                    plane_widths,
                    plane_heights,
                    plane_bytes_per_pixel,
                    plane_byte_sizes,
                },
            );
        SLDN_CPU_READBACK_OK
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_cpu_readback_unregister_surface(
        rt: *mut CpuReadbackRuntimeHandle,
        surface_id: u64,
    ) -> i32 {
        let rt = match unsafe { rt.as_ref() } {
            Some(r) => r,
            None => return SLDN_CPU_READBACK_ERR,
        };
        let removed = rt
            .registered
            .lock()
            .expect("sldn_cpu_readback registered: poisoned")
            .remove(&surface_id);
        if removed.is_none() {
            return SLDN_CPU_READBACK_ERR;
        }
        if rt.adapter.unregister_host_surface(surface_id) {
            SLDN_CPU_READBACK_OK
        } else {
            SLDN_CPU_READBACK_ERR
        }
    }

    fn populate_view(
        rt: &CpuReadbackRuntimeHandle,
        surface_id: u64,
        out: &mut SldnCpuReadbackView,
    ) -> i32 {
        let registered = rt
            .registered
            .lock()
            .expect("sldn_cpu_readback registered: poisoned");
        let entry = match registered.get(&surface_id) {
            Some(e) => e,
            None => {
                tracing::error!(
                    "sldn_cpu_readback acquire: surface_id {} not registered",
                    surface_id
                );
                return SLDN_CPU_READBACK_ERR;
            }
        };
        out.width = entry.width;
        out.height = entry.height;
        out.format = entry.format as u32;
        out.plane_count = entry.plane_count;
        out.planes = [SldnCpuReadbackPlane {
            mapped_ptr: std::ptr::null_mut(),
            width: 0,
            height: 0,
            bytes_per_pixel: 0,
            byte_size: 0,
        }; SLDN_CPU_READBACK_MAX_PLANES];
        for idx in 0..(entry.plane_count as usize).min(SLDN_CPU_READBACK_MAX_PLANES) {
            out.planes[idx] = SldnCpuReadbackPlane {
                mapped_ptr: entry.plane_mapped_ptrs[idx],
                width: entry.plane_widths[idx],
                height: entry.plane_heights[idx],
                bytes_per_pixel: entry.plane_bytes_per_pixel[idx],
                byte_size: entry.plane_byte_sizes[idx],
            };
        }
        SLDN_CPU_READBACK_OK
    }

    fn make_descriptor(surface_id: u64) -> StreamlibSurface {
        StreamlibSurface::new(
            surface_id,
            0,
            0,
            SurfaceFormat::Bgra8,
            SurfaceUsage::CPU_READBACK,
            SurfaceTransportHandle::empty(),
            SurfaceSyncState::default(),
        )
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_cpu_readback_acquire_read(
        rt: *mut CpuReadbackRuntimeHandle,
        surface_id: u64,
        out_view: *mut SldnCpuReadbackView,
    ) -> i32 {
        let rt = match unsafe { rt.as_ref() } {
            Some(r) => r,
            None => return SLDN_CPU_READBACK_ERR,
        };
        let out = match unsafe { out_view.as_mut() } {
            Some(v) => v,
            None => return SLDN_CPU_READBACK_ERR,
        };
        let surface = make_descriptor(surface_id);
        match rt.adapter.acquire_read(&surface) {
            Ok(g) => {
                std::mem::forget(g);
                populate_view(rt, surface_id, out)
            }
            Err(e) => {
                tracing::error!(
                    "sldn_cpu_readback_acquire_read({}): {:?}",
                    surface_id,
                    e
                );
                SLDN_CPU_READBACK_ERR
            }
        }
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_cpu_readback_acquire_write(
        rt: *mut CpuReadbackRuntimeHandle,
        surface_id: u64,
        out_view: *mut SldnCpuReadbackView,
    ) -> i32 {
        let rt = match unsafe { rt.as_ref() } {
            Some(r) => r,
            None => return SLDN_CPU_READBACK_ERR,
        };
        let out = match unsafe { out_view.as_mut() } {
            Some(v) => v,
            None => return SLDN_CPU_READBACK_ERR,
        };
        let surface = make_descriptor(surface_id);
        match rt.adapter.acquire_write(&surface) {
            Ok(g) => {
                std::mem::forget(g);
                populate_view(rt, surface_id, out)
            }
            Err(e) => {
                tracing::error!(
                    "sldn_cpu_readback_acquire_write({}): {:?}",
                    surface_id,
                    e
                );
                SLDN_CPU_READBACK_ERR
            }
        }
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_cpu_readback_try_acquire_read(
        rt: *mut CpuReadbackRuntimeHandle,
        surface_id: u64,
        out_view: *mut SldnCpuReadbackView,
    ) -> i32 {
        let rt = match unsafe { rt.as_ref() } {
            Some(r) => r,
            None => return SLDN_CPU_READBACK_ERR,
        };
        let out = match unsafe { out_view.as_mut() } {
            Some(v) => v,
            None => return SLDN_CPU_READBACK_ERR,
        };
        let surface = make_descriptor(surface_id);
        match rt.adapter.try_acquire_read(&surface) {
            Ok(Some(g)) => {
                std::mem::forget(g);
                populate_view(rt, surface_id, out)
            }
            Ok(None) => SLDN_CPU_READBACK_CONTENDED,
            Err(e) => {
                tracing::error!(
                    "sldn_cpu_readback_try_acquire_read({}): {:?}",
                    surface_id,
                    e
                );
                SLDN_CPU_READBACK_ERR
            }
        }
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_cpu_readback_try_acquire_write(
        rt: *mut CpuReadbackRuntimeHandle,
        surface_id: u64,
        out_view: *mut SldnCpuReadbackView,
    ) -> i32 {
        let rt = match unsafe { rt.as_ref() } {
            Some(r) => r,
            None => return SLDN_CPU_READBACK_ERR,
        };
        let out = match unsafe { out_view.as_mut() } {
            Some(v) => v,
            None => return SLDN_CPU_READBACK_ERR,
        };
        let surface = make_descriptor(surface_id);
        match rt.adapter.try_acquire_write(&surface) {
            Ok(Some(g)) => {
                std::mem::forget(g);
                populate_view(rt, surface_id, out)
            }
            Ok(None) => SLDN_CPU_READBACK_CONTENDED,
            Err(e) => {
                tracing::error!(
                    "sldn_cpu_readback_try_acquire_write({}): {:?}",
                    surface_id,
                    e
                );
                SLDN_CPU_READBACK_ERR
            }
        }
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_cpu_readback_release_read(
        rt: *mut CpuReadbackRuntimeHandle,
        surface_id: u64,
    ) -> i32 {
        let rt = match unsafe { rt.as_ref() } {
            Some(r) => r,
            None => return SLDN_CPU_READBACK_ERR,
        };
        rt.adapter.end_read_access(surface_id);
        SLDN_CPU_READBACK_OK
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_cpu_readback_release_write(
        rt: *mut CpuReadbackRuntimeHandle,
        surface_id: u64,
    ) -> i32 {
        let rt = match unsafe { rt.as_ref() } {
            Some(r) => r,
            None => return SLDN_CPU_READBACK_ERR,
        };
        rt.adapter.end_write_access(surface_id);
        SLDN_CPU_READBACK_OK
    }
}

#[cfg(not(target_os = "linux"))]
mod cpu_readback {
    use std::ffi::c_void;

    pub const SLDN_CPU_READBACK_MAX_PLANES: usize = 4;

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct SldnCpuReadbackPlane {
        pub mapped_ptr: *mut u8,
        pub width: u32,
        pub height: u32,
        pub bytes_per_pixel: u32,
        pub byte_size: u64,
    }

    #[repr(C)]
    pub struct SldnCpuReadbackView {
        pub width: u32,
        pub height: u32,
        pub format: u32,
        pub plane_count: u32,
        pub planes: [SldnCpuReadbackPlane; SLDN_CPU_READBACK_MAX_PLANES],
    }

    pub type SldnCpuReadbackTriggerCallback = unsafe extern "C" fn(
        user_data: *mut c_void,
        surface_id: u64,
        direction: u32,
    ) -> u64;

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_cpu_readback_runtime_new() -> *mut c_void {
        tracing::error!("sldn_cpu_readback_*: cpu-readback adapter runtime is Linux-only");
        std::ptr::null_mut()
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_cpu_readback_runtime_free(_rt: *mut c_void) {}

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_cpu_readback_set_trigger_callback(
        _rt: *mut c_void,
        _callback: SldnCpuReadbackTriggerCallback,
        _user_data: *mut c_void,
    ) -> i32 {
        -1
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_cpu_readback_register_surface(
        _rt: *mut c_void,
        _surface_id: u64,
        _gpu_handle: *mut c_void,
        _surface_format: u32,
    ) -> i32 {
        -1
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_cpu_readback_unregister_surface(
        _rt: *mut c_void,
        _surface_id: u64,
    ) -> i32 {
        -1
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_cpu_readback_acquire_read(
        _rt: *mut c_void,
        _surface_id: u64,
        _out_view: *mut SldnCpuReadbackView,
    ) -> i32 {
        -1
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_cpu_readback_acquire_write(
        _rt: *mut c_void,
        _surface_id: u64,
        _out_view: *mut SldnCpuReadbackView,
    ) -> i32 {
        -1
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_cpu_readback_try_acquire_read(
        _rt: *mut c_void,
        _surface_id: u64,
        _out_view: *mut SldnCpuReadbackView,
    ) -> i32 {
        -1
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_cpu_readback_try_acquire_write(
        _rt: *mut c_void,
        _surface_id: u64,
        _out_view: *mut SldnCpuReadbackView,
    ) -> i32 {
        -1
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_cpu_readback_release_read(
        _rt: *mut c_void,
        _surface_id: u64,
    ) -> i32 {
        -1
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_cpu_readback_release_write(
        _rt: *mut c_void,
        _surface_id: u64,
    ) -> i32 {
        -1
    }
}

#[cfg(not(target_os = "linux"))]
mod vulkan {
    use std::ffi::c_void;

    #[repr(C)]
    pub struct SldnVulkanView {
        pub vk_image: u64,
        pub vk_image_layout: i32,
    }

    #[repr(C)]
    pub struct SldnVulkanRawHandles {
        pub vk_instance: u64,
        pub vk_physical_device: u64,
        pub vk_device: u64,
        pub vk_queue: u64,
        pub vk_queue_family_index: u32,
        pub api_version: u32,
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_vulkan_runtime_new() -> *mut c_void {
        tracing::error!("sldn_vulkan_*: Vulkan adapter runtime is Linux-only");
        std::ptr::null_mut()
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_vulkan_runtime_free(_rt: *mut c_void) {}

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_vulkan_register_surface(
        _rt: *mut c_void,
        _surface_id: u64,
        _gpu_handle: *mut c_void,
    ) -> i32 {
        -1
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_vulkan_unregister_surface(
        _rt: *mut c_void,
        _surface_id: u64,
    ) -> i32 {
        -1
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_vulkan_acquire_write(
        _rt: *mut c_void,
        _surface_id: u64,
        _out_view: *mut SldnVulkanView,
    ) -> i32 {
        -1
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_vulkan_release_write(
        _rt: *mut c_void,
        _surface_id: u64,
    ) -> i32 {
        -1
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_vulkan_acquire_read(
        _rt: *mut c_void,
        _surface_id: u64,
        _out_view: *mut SldnVulkanView,
    ) -> i32 {
        -1
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_vulkan_release_read(
        _rt: *mut c_void,
        _surface_id: u64,
    ) -> i32 {
        -1
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sldn_vulkan_raw_handles(
        _rt: *mut c_void,
        _out: *mut SldnVulkanRawHandles,
    ) -> i32 {
        -1
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


