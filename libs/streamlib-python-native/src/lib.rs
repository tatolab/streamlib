// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// FFI cdylib — all public functions are unsafe extern "C" called from Python via ctypes.
#![allow(clippy::missing_safety_doc)]

//! FFI cdylib for Python subprocess processors to access iceoryx2 directly.
//!
//! Provides C ABI functions prefixed with `slpn_` that Python loads via `ctypes.CDLL()`.
//! This allows Python processors to read/write iceoryx2 shared memory without
//! going through Rust host pipes (zero-copy data plane).

use std::collections::HashMap;
use std::ffi::{c_char, CStr};

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
pub struct PythonNativeContext {
    processor_id: String,
    node: Node<ipc::Service>,
    subscribers: HashMap<String, SubscriberState>,
    publishers: HashMap<String, PublisherState>,
    /// Per-port read mode (port_name → READ_MODE_*). Default is SkipToLatest.
    port_read_modes: HashMap<String, i32>,
    /// Single Listener for this processor's destination-paired Notify service.
    /// All inputs share this listener (destination-centric service shape).
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
    /// Notifier into the destination's paired Event service. Some when the
    /// destination wired a notify_service_name; None for legacy callers.
    notifier: Option<Notifier<ipc::Service>>,
}

impl PythonNativeContext {
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

/// Create a new native context for a Python processor.
///
/// Returns an opaque pointer. Caller must call `slpn_context_destroy` when done.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn slpn_context_create(
    processor_id: *const c_char,
) -> *mut PythonNativeContext {
    let id = if processor_id.is_null() {
        "unknown"
    } else {
        unsafe { CStr::from_ptr(processor_id) }.to_str().unwrap_or("unknown")
    };

    match PythonNativeContext::new(id) {
        Ok(ctx) => Box::into_raw(Box::new(ctx)),
        Err(e) => {
            tracing::error!("Failed to create context: {}", e);
            std::ptr::null_mut()
        }
    }
}

/// Destroy a native context, releasing all iceoryx2 resources.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn slpn_context_destroy(ctx: *mut PythonNativeContext) {
    if !ctx.is_null() {
        let _ = unsafe { Box::from_raw(ctx) };
    }
}

/// Get current monotonic time in nanoseconds.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn slpn_context_time_ns(_ctx: *const PythonNativeContext) -> i64 {
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
pub unsafe extern "C" fn slpn_input_subscribe(
    ctx: *mut PythonNativeContext,
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
                "[slpn:{}] Invalid service name '{}': {}",
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
                "[slpn:{}] Failed to open service '{}': {}",
                ctx.processor_id, service_name, e
            );
            return -1;
        }
    };

    let subscriber = match service.subscriber_builder().buffer_size(16).create() {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(
                "[slpn:{}] Failed to create subscriber for '{}': {}",
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
pub unsafe extern "C" fn slpn_input_set_read_mode(
    ctx: *mut PythonNativeContext,
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
pub unsafe extern "C" fn slpn_input_poll(ctx: *mut PythonNativeContext) -> i32 {
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
/// Uses the port's read mode (set via `slpn_input_set_read_mode`):
/// - SkipToLatest (default): Drains buffer, returns only the newest payload.
/// - ReadNextInOrder: Returns oldest payload in FIFO order.
///
/// Returns 0 on success, 1 if no data available, -1 on error.
///
/// `out_len` receives the actual data length.
/// `out_ts` receives the timestamp in nanoseconds.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn slpn_input_read(
    ctx: *mut PythonNativeContext,
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
/// `notify_service_name` may be the empty string or null to skip notifier setup
/// (legacy / no-notify path). When non-empty, `slpn_output_write` will call
/// `notify()` after every successful `send()`.
///
/// Returns 0 on success, -1 on failure.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn slpn_output_publish(
    ctx: *mut PythonNativeContext,
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
                "[slpn:{}] Invalid service name '{}': {}",
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
                "[slpn:{}] Failed to open service '{}': {}",
                ctx.processor_id, service_name, e
            );
            return -1;
        }
    };

    let publisher = match service.publisher_builder().initial_max_slice_len(max_payload_bytes + FRAME_HEADER_SIZE).create() {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(
                "[slpn:{}] Failed to create publisher for '{}': {}",
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
                            "[slpn:{}] Failed to create notifier for '{}': {:?}",
                            ctx.processor_id, name, e
                        );
                        None
                    }
                },
                Err(e) => {
                    tracing::warn!(
                        "[slpn:{}] Failed to open notify service '{}': {:?}",
                        ctx.processor_id, name, e
                    );
                    None
                }
            },
            Err(e) => {
                tracing::warn!(
                    "[slpn:{}] Invalid notify service name '{}': {}",
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
pub unsafe extern "C" fn slpn_output_write(
    ctx: *mut PythonNativeContext,
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
                "[slpn:{}] No publisher for port '{}'",
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
                "[slpn:{}] Failed to loan slice for port '{}': {:?}",
                ctx.processor_id, port_name, e
            );
            return -1;
        }
    };
    let sample = sample.write_from_slice(&frame);
    if let Err(e) = sample.send() {
        tracing::error!(
            "[slpn:{}] Failed to send sample for port '{}': {:?}",
            ctx.processor_id, port_name, e
        );
        return -1;
    }

    if let Some(notifier) = state.notifier.as_ref()
        && let Err(e) = notifier.notify()
    {
        tracing::trace!(
            "[slpn:{}] notify() failed for port '{}': {:?}",
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
/// Creates a Listener whose fd Python can `select` on alongside stdin. Idempotent
/// per ctx — first call wins; subsequent calls are no-ops since each processor
/// only ever has one destination-paired Event service. Returns 0 on success,
/// -1 on failure.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn slpn_event_subscribe(
    ctx: *mut PythonNativeContext,
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
                "[slpn:{}] Invalid notify service name '{}': {}",
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
                "[slpn:{}] Failed to open notify service '{}': {:?}",
                ctx.processor_id, name, e
            );
            return -1;
        }
    };
    let listener = match service.listener_builder().create() {
        Ok(l) => l,
        Err(e) => {
            tracing::error!(
                "[slpn:{}] Failed to create listener for '{}': {:?}",
                ctx.processor_id, name, e
            );
            return -1;
        }
    };
    ctx.notify_listener = Some(listener);
    0
}

/// Returns the underlying listener fd for `select`/`poll`, or -1 if not subscribed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn slpn_event_listener_fd(ctx: *mut PythonNativeContext) -> i32 {
    let ctx = match unsafe { ctx.as_mut() } {
        Some(c) => c,
        None => return -1,
    };
    match ctx.notify_listener.as_ref() {
        // SAFETY: native_handle() is unsafe per iceoryx2-bb-posix because the
        // returned int must not outlive the Listener. We hand it to Python
        // which uses it transiently inside select(); the Listener stays alive
        // for the lifetime of the context.
        Some(l) => unsafe { l.file_descriptor().native_handle() },
        None => -1,
    }
}

/// Drain pending event-IDs so the listener fd transitions back to not-readable.
///
/// Call after `select` reports the fd readable, before the next `select`.
/// Returns 0 on success, -1 if no listener is subscribed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn slpn_event_drain(ctx: *mut PythonNativeContext) -> i32 {
    let ctx = match unsafe { ctx.as_mut() } {
        Some(c) => c,
        None => return -1,
    };
    let Some(listener) = ctx.notify_listener.as_ref() else {
        return -1;
    };
    if let Err(e) = listener.try_wait_all(|_id| {}) {
        tracing::trace!(
            "[slpn:{}] event drain try_wait_all failed: {:?}",
            ctx.processor_id, e
        );
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
    pub unsafe extern "C" fn slpn_gpu_surface_lookup(iosurface_id: u32) -> *mut SurfaceHandle {
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
    pub unsafe extern "C" fn slpn_gpu_surface_lock(
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
    pub unsafe extern "C" fn slpn_gpu_surface_unlock(
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
    pub unsafe extern "C" fn slpn_gpu_surface_base_address(
        handle: *const SurfaceHandle,
    ) -> *mut u8 {
        match handle.as_ref() {
            Some(h) => h.base_address,
            None => std::ptr::null_mut(),
        }
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_gpu_surface_width(handle: *const SurfaceHandle) -> u32 {
        handle.as_ref().map(|h| h.width).unwrap_or(0)
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_gpu_surface_height(handle: *const SurfaceHandle) -> u32 {
        handle.as_ref().map(|h| h.height).unwrap_or(0)
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_gpu_surface_bytes_per_row(handle: *const SurfaceHandle) -> u32 {
        handle.as_ref().map(|h| h.bytes_per_row).unwrap_or(0)
    }

    /// Get the raw IOSurfaceRef pointer for CGL texture binding.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_gpu_surface_iosurface_ref(
        handle: *const SurfaceHandle,
    ) -> *const std::ffi::c_void {
        match handle.as_ref() {
            Some(h) => h.surface_ref,
            None => std::ptr::null(),
        }
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_gpu_surface_create(
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
    pub unsafe extern "C" fn slpn_gpu_surface_get_id(handle: *const SurfaceHandle) -> u32 {
        handle.as_ref().map(|h| h.surface_id).unwrap_or(0)
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_gpu_surface_release(handle: *mut SurfaceHandle) {
        if !handle.is_null() {
            let h = Box::from_raw(handle);
            IOSurfaceDecrementUseCount(h.surface_ref);
            CFRelease(h.surface_ref);
        }
    }
}

#[cfg(target_os = "linux")]
mod gpu_surface {
    //! Linux GPU surface handle backed by a DMA-BUF file descriptor.
    //!
    //! The handle is produced by [`super::surface_client::slpn_surface_resolve_surface`]
    //! (after a handle `check_out` over `SCM_RIGHTS`) and consumed by the
    //! `slpn_gpu_surface_*` FFI symbols — same shape as the macOS IOSurface
    //! variant so Python's `ctypes` bindings don't branch by platform.
    //!
    //! CPU access on lock goes through a Vulkan DMA-BUF import
    //! (`VkImportMemoryFdInfoKHR` + `vkBindBufferMemory` + `vkMapMemory`) —
    //! same shape as the host's `VulkanPixelBuffer::from_dma_buf_fd` so both
    //! ends speak the canonical driver-supported path. The import-side only —
    //! allocation always escalates to the host per the research doc.
    use std::ffi::c_void;
    use std::os::unix::io::RawFd;
    use std::sync::Arc;

    use vulkanalia::vk::{self, Handle as _};

    use super::surface_share_vulkan_linux::SurfaceShareVulkanDevice;

    /// Surface backend used for the currently-locked mapping. Reported via
    /// [`slpn_gpu_surface_backend`] so tests can assert the import took the
    /// Vulkan path rather than silently falling back.
    pub const SURFACE_BACKEND_NONE: u32 = 0;
    pub const SURFACE_BACKEND_VULKAN: u32 = 2;

    pub struct SurfaceHandle {
        /// One fd per DMA-BUF plane. Single-plane surfaces carry a
        /// one-element vec; multi-plane DMA-BUFs (e.g. NV12 under DRM format
        /// modifiers with disjoint Y/UV allocations) carry one per plane,
        /// keyed by plane index.
        pub fds: Vec<RawFd>,
        /// Optional OPAQUE_FD timeline-semaphore handle the host attached
        /// when registering the surface (#531). Routed into the Vulkan
        /// adapter's `VulkanTimelineSemaphore::from_imported_opaque_fd` so
        /// the subprocess reuses the host adapter's timeline-wait + signal
        /// path. `None` for surfaces without explicit Vulkan sync (OpenGL
        /// adapter, CPU-readback, legacy DMA-BUF consumer flows). The fd is
        /// closed when the handle is dropped, unless the import has taken
        /// ownership (Vulkan import takes ownership on success).
        pub sync_fd: Option<RawFd>,
        pub plane_sizes: Vec<u64>,
        pub plane_offsets: Vec<u64>,
        /// Per-plane row pitch in bytes, copied from the surface-share
        /// lookup response. Required by EGL DMA-BUF import
        /// (`EGL_DMA_BUF_PLANE{N}_PITCH_EXT`); the Vulkan import path
        /// reads from `bytes_per_row` instead.
        pub plane_strides: Vec<u64>,
        pub width: u32,
        pub height: u32,
        pub bytes_per_row: u32,
        /// Total byte size across all planes — the sum of `plane_sizes`,
        /// kept cached so the Vulkan single-plane import path (`lock`) can
        /// continue to pass it without recomputing.
        pub size: u64,
        /// DRM format modifier of the underlying host `VkImage`. Required
        /// by EGL DMA-BUF import
        /// (`EGL_DMA_BUF_PLANE0_MODIFIER_LO/HI_EXT`); zero means
        /// LINEAR / not applicable. Render-target consumers MUST refuse
        /// LINEAR on NVIDIA — see
        /// `docs/learnings/nvidia-egl-dmabuf-render-target.md`.
        pub drm_format_modifier: u64,
        /// Format string from the wire response (e.g. `"Bgra8Unorm"`).
        /// Used to derive a DRM_FORMAT_* fourcc for EGL import.
        pub format: String,
        /// Host-mapped base address of plane 0, populated by `lock`. The
        /// multi-plane accessor reads from [`Self::plane_mapped_ptrs`].
        pub mapped_ptr: *mut u8,
        /// Per-plane mapped base addresses. Always the same length as
        /// `fds`; `null` until a plane is mmap'd. The single-plane Vulkan
        /// `lock` path populates index 0; `slpn_gpu_surface_plane_mmap`
        /// mmaps a specific plane on demand.
        pub plane_mapped_ptrs: Vec<*mut u8>,
        pub is_locked: bool,
        /// Vulkan device attached by [`super::surface_client::slpn_surface_resolve_surface`].
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
        /// Return the mapped base address for a given plane, or null if the
        /// plane has not been mapped (or the index is out of range). Index
        /// 0 on a single-plane surface that has been `lock`'d returns the
        /// Vulkan-mapped pointer from `mapped_ptr`.
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
            // Tear down any outstanding Vulkan import (lock without unlock).
            // `lock` imports a dup of `self.fds[0]`; Vulkan owns that dup.
            // Freeing the imported memory releases the dup, not our fds.
            if self.is_locked {
                if let Some(device) = self.vulkan_device.as_ref() {
                    device.destroy_imported(self.vulkan_buffer, self.vulkan_memory);
                }
            }
            // Unmap any plane-specific mappings made via mmap. Plane 0's
            // `mapped_ptr` (if populated by Vulkan `lock`) does not need an
            // munmap — Vulkan manages its own backing memory.
            for (i, ptr) in self.plane_mapped_ptrs.iter().enumerate() {
                if !ptr.is_null() {
                    if let Some(size) = self.plane_sizes.get(i) {
                        unsafe { libc::munmap(*ptr as *mut libc::c_void, *size as usize) };
                    }
                }
            }
            // Every plane fd stays with the SurfaceHandle across lock/unlock
            // cycles — close them last.
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
    pub unsafe extern "C" fn slpn_gpu_surface_lookup(_iosurface_id: u32) -> *mut SurfaceHandle {
        tracing::error!("GPU surface lookup by IOSurface id is macOS-only; use handle check_out");
        std::ptr::null_mut()
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_gpu_surface_lock(
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
        // Vulkan lock imports plane 0 only — there is no multi-plane Vulkan
        // producer in tree yet. Multi-plane consumers can mmap individual
        // planes via `slpn_gpu_surface_plane_mmap` without taking this path.
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
        // Dup the fd before import: vkAllocateMemory takes ownership of the
        // fd on success. Keeping the original fd on the SurfaceHandle lets
        // the caller lock/unlock/lock again (each lock imports a fresh dup).
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

    /// mmap a specific plane into user space. Intended for polyglot
    /// consumers that need CPU-side access to a plane the Vulkan path did
    /// not import (index > 0), or for tests that read back plane content
    /// without requiring a Vulkan-capable device.
    ///
    /// Returns `0` on success, `-1` on failure. The mapping is torn down
    /// when the [`SurfaceHandle`] is released.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_gpu_surface_plane_mmap(
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
            // Already mapped — idempotent.
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
                "slpn_gpu_surface_plane_mmap: mmap failed for plane {} (fd {}, size {}): {}",
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
    pub unsafe extern "C" fn slpn_gpu_surface_unlock(
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
    ///
    /// Exposed so tests and polyglot drivers can confirm the Vulkan path was
    /// taken rather than silently falling back.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_gpu_surface_backend(handle: *const SurfaceHandle) -> u32 {
        unsafe { handle.as_ref() }
            .map(|h| h.backend)
            .unwrap_or(SURFACE_BACKEND_NONE)
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_gpu_surface_base_address(
        handle: *const SurfaceHandle,
    ) -> *mut u8 {
        match unsafe { handle.as_ref() } {
            Some(h) => h.base_address(0),
            None => std::ptr::null_mut(),
        }
    }

    /// Per-plane base address accessor. Returns null if the plane index is
    /// out of range, if the plane is not mmap'd (call
    /// [`slpn_gpu_surface_plane_mmap`] first), or if the handle is null.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_gpu_surface_plane_base_address(
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
    pub unsafe extern "C" fn slpn_gpu_surface_plane_count(
        handle: *const SurfaceHandle,
    ) -> u32 {
        unsafe { handle.as_ref() }
            .map(|h| h.fds.len() as u32)
            .unwrap_or(0)
    }

    /// Byte size of the given plane, or `0` if the plane index is out of
    /// range or the handle is null.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_gpu_surface_plane_size(
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
    pub unsafe extern "C" fn slpn_gpu_surface_plane_stride(
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
    pub unsafe extern "C" fn slpn_gpu_surface_plane_offset(
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
    pub unsafe extern "C" fn slpn_gpu_surface_plane_fd(
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
    pub unsafe extern "C" fn slpn_gpu_surface_drm_format_modifier(
        handle: *const SurfaceHandle,
    ) -> u64 {
        unsafe { handle.as_ref() }
            .map(|h| h.drm_format_modifier)
            .unwrap_or(0)
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_gpu_surface_width(handle: *const SurfaceHandle) -> u32 {
        unsafe { handle.as_ref() }.map(|h| h.width).unwrap_or(0)
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_gpu_surface_height(handle: *const SurfaceHandle) -> u32 {
        unsafe { handle.as_ref() }.map(|h| h.height).unwrap_or(0)
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_gpu_surface_bytes_per_row(
        handle: *const SurfaceHandle,
    ) -> u32 {
        unsafe { handle.as_ref() }.map(|h| h.bytes_per_row).unwrap_or(0)
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_gpu_surface_create(
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
    pub unsafe extern "C" fn slpn_gpu_surface_get_id(handle: *const SurfaceHandle) -> u32 {
        // Linux surface IDs are handle UUIDs (strings), not u32 IOSurfaceIDs.
        // Return the fd as a best-effort numeric token so Python code that
        // unconditionally calls this doesn't get a u32 collision with a real
        // IOSurface id. Callers that need the string surface_id should keep
        // the handle pool_id they already passed to resolve_surface.
        unsafe { handle.as_ref() }
            .and_then(|h| h.fds.first().copied())
            .map(|fd| fd as u32)
            .unwrap_or(0)
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_gpu_surface_iosurface_ref(
        _handle: *const SurfaceHandle,
    ) -> *const c_void {
        // No IOSurface equivalent on Linux.
        std::ptr::null()
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_gpu_surface_release(handle: *mut SurfaceHandle) {
        if !handle.is_null() {
            let _ = unsafe { Box::from_raw(handle) };
            // Drop impl closes fd and munmaps if locked.
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod gpu_surface {
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_gpu_surface_lookup(_iosurface_id: u32) -> *mut std::ffi::c_void {
        tracing::error!("GPU surface operations not supported on this platform");
        std::ptr::null_mut()
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_gpu_surface_lock(
        _handle: *mut std::ffi::c_void,
        _read_only: i32,
    ) -> i32 {
        -1
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_gpu_surface_unlock(
        _handle: *mut std::ffi::c_void,
        _read_only: i32,
    ) -> i32 {
        -1
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_gpu_surface_base_address(
        _handle: *const std::ffi::c_void,
    ) -> *mut u8 {
        std::ptr::null_mut()
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_gpu_surface_width(_handle: *const std::ffi::c_void) -> u32 {
        0
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_gpu_surface_height(_handle: *const std::ffi::c_void) -> u32 {
        0
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_gpu_surface_bytes_per_row(
        _handle: *const std::ffi::c_void,
    ) -> u32 {
        0
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_gpu_surface_create(
        _width: u32,
        _height: u32,
        _bytes_per_element: u32,
    ) -> *mut std::ffi::c_void {
        tracing::error!("GPU surface creation not supported on this platform");
        std::ptr::null_mut()
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_gpu_surface_get_id(_handle: *const std::ffi::c_void) -> u32 {
        0
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_gpu_surface_iosurface_ref(
        _handle: *const std::ffi::c_void,
    ) -> *const std::ffi::c_void {
        std::ptr::null()
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_gpu_surface_release(_handle: *mut std::ffi::c_void) {}
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
        fn xpc_connection_send_message(connection: XpcConnectionT, message: XpcObjectT);
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
        runtime_id: String,
        resolve_cache: HashMap<String, CachedSurface>,
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_surface_connect(
        xpc_service_name: *const c_char,
        runtime_id: *const c_char,
    ) -> *mut SurfaceShareHandle {
        if xpc_service_name.is_null() {
            tracing::error!("surface_connect: null service name");
            return std::ptr::null_mut();
        }

        let rid = if runtime_id.is_null() {
            "python-subprocess".to_string()
        } else {
            CStr::from_ptr(runtime_id).to_string_lossy().into_owned()
        };

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
        tracing::error!(
            "surface_connect: connected to '{}' with runtime_id='{}'",
            name, rid
        );

        Box::into_raw(Box::new(SurfaceShareHandle {
            connection,
            runtime_id: rid,
            resolve_cache: HashMap::new(),
        }))
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_surface_disconnect(handle: *mut SurfaceShareHandle) {
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
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_surface_resolve_surface(
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
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_surface_acquire_surface(
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
            super::gpu_surface::slpn_gpu_surface_create(width, height, bytes_per_element);
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
        let surface_id = IOSurfaceGetID(surface_handle.surface_ref);
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let pool_id = format!("python-{}-{}", surface_id, ts);

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
        let rid_value = CString::new(handle.runtime_id.as_str()).unwrap();
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

    /// Unregister a surface from the handle (fire-and-forget).
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_surface_unregister_surface(
        handle: *mut SurfaceShareHandle,
        pool_id: *const c_char,
    ) {
        let handle = match handle.as_mut() {
            Some(b) => b,
            None => return,
        };

        let pool_id_str = match c_str_to_str(pool_id) {
            Some(s) => s,
            None => return,
        };

        let request = xpc_dictionary_create(std::ptr::null(), std::ptr::null(), 0);
        if request.is_null() {
            return;
        }

        let op_key = CString::new("op").unwrap();
        let op_value = CString::new("unregister").unwrap();
        xpc_dictionary_set_string(request, op_key.as_ptr(), op_value.as_ptr());

        let sid_key = CString::new("surface_id").unwrap();
        let sid_value = CString::new(pool_id_str).unwrap();
        xpc_dictionary_set_string(request, sid_key.as_ptr(), sid_value.as_ptr());

        let rid_key = CString::new("runtime_id").unwrap();
        let rid_value = CString::new(handle.runtime_id.as_str()).unwrap();
        xpc_dictionary_set_string(request, rid_key.as_ptr(), rid_value.as_ptr());

        // Fire and forget — handle unregister doesn't send a reply
        xpc_connection_send_message(handle.connection, request);
        xpc_release(request);
    }

    unsafe fn c_str_to_str<'a>(ptr: *const c_char) -> Option<&'a str> {
        if ptr.is_null() {
            return None;
        }
        CStr::from_ptr(ptr).to_str().ok()
    }

}

#[cfg(target_os = "linux")]
mod surface_share_vulkan_linux {
    //! Minimal Vulkan device used by the polyglot consumer to import DMA-BUF
    //! fds handed out by the handle (issue #420).
    //!
    //! Consumer-only per the subprocess-import-only safety posture: we load
    //! libvulkan.so via `libloading`, create a bare instance + logical device
    //! enabling only `VK_KHR_external_memory` + `VK_KHR_external_memory_fd` +
    //! `VK_EXT_external_memory_dma_buf`, and expose a single
    //! [`SurfaceShareVulkanDevice::import_dma_buf_fd`] method. Export paths
    //! (`vkGetMemoryFdKHR`) are intentionally absent — allocation is the
    //! host's job via escalate IPC.
    //!
    //! One instance+device per [`super::surface_client::SurfaceShareHandle`], lazily
    //! created on first [`super::surface_client::slpn_surface_resolve_surface`]
    //! and torn down with the handle.
    use std::ffi::{c_char, CStr};
    use std::os::unix::io::RawFd;
    use std::sync::Arc;

    use vulkanalia::loader::{LibloadingLoader, LIBRARY};
    use vulkanalia::prelude::v1_1::*;
    use vulkanalia::vk;

    /// Minimal per-service Vulkan device used only for DMA-BUF import.
    pub struct SurfaceShareVulkanDevice {
        _entry: vulkanalia::Entry,
        instance: vulkanalia::Instance,
        device: vulkanalia::Device,
        memory_properties: vk::PhysicalDeviceMemoryProperties,
    }

    // Vulkan handles are thread-safe; vulkanalia wrappers don't auto-impl
    // these because they wrap function pointers via raw loaders.
    unsafe impl Send for SurfaceShareVulkanDevice {}
    unsafe impl Sync for SurfaceShareVulkanDevice {}

    /// A successfully-imported DMA-BUF, persistently mapped.
    pub struct ImportedBuffer {
        pub buffer: vk::Buffer,
        pub memory: vk::DeviceMemory,
        pub mapped_ptr: *mut u8,
    }

    impl SurfaceShareVulkanDevice {
        /// Lazily create the service's Vulkan device. Returns `Err` with a
        /// human-readable reason when Vulkan is unavailable or a required
        /// extension is missing — caller is expected to null out the
        /// resolve_surface result and log the reason.
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

            // Vulkan requires at least one queue at device creation even
            // though we never submit. Pick family 0 — every conformant driver
            // has at least one family.
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

        /// Import a DMA-BUF fd as a `HOST_VISIBLE | HOST_COHERENT` buffer,
        /// persistently mapped. Mirrors the host's
        /// `VulkanPixelBuffer::from_dma_buf_fd` shape.
        ///
        /// On success, Vulkan takes ownership of `fd` — the caller must
        /// **not** `close(fd)`. On error, the caller retains ownership.
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
            // fd ownership transferred to Vulkan on success.

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

        /// Tear down an imported buffer in the reverse order of creation:
        /// `vkUnmapMemory` → `vkDestroyBuffer` → `vkFreeMemory`.
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
    //! Linux handle consumer client.
    //!
    //! Mirrors the macOS XPC shim's FFI surface (same `slpn_surface_*` symbols,
    //! same arg shapes) but speaks the Unix-socket + SCM_RIGHTS wire protocol
    //! that the runtime's per-process surface handle listens on. Consumer-only
    //! per the subprocess-import-only safety posture —
    //! allocation always goes through the host via #325 escalate IPC.
    //!
    //! Lifecycle:
    //!   - `slpn_surface_connect(socket_path, runtime_id)` stores config only;
    //!     the Unix socket is lazily opened on the first op (per the research
    //!     doc's "fail at first use" decision). Subprocesses that never touch
    //!     GPU surfaces never need the handle up.
    //!   - `slpn_surface_resolve_surface(handle, pool_id)` sends a `check_out`
    //!     op, receives a DMA-BUF fd via SCM_RIGHTS, caches the fd keyed by
    //!     pool_id, returns a [`SurfaceHandle`] with its own fd dup.
    //!   - `slpn_surface_unregister_surface(handle, pool_id)` evicts the cache
    //!     and sends a `release` op.
    //!   - `slpn_surface_acquire_surface(...)` returns null with a clear error
    //!     — allocation is host-only.
    use std::collections::HashMap;
    use std::ffi::{c_char, CStr};
    use std::os::unix::io::RawFd;
    use std::os::unix::net::UnixStream;
    use std::sync::{Arc, Mutex};

    use super::surface_share_vulkan_linux::SurfaceShareVulkanDevice;
    use super::gpu_surface::{SurfaceHandle, SURFACE_BACKEND_NONE};

    use vulkanalia::vk::{self, Handle as _};

    /// Maximum cached resolved surfaces before we drop the whole cache.
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

    /// Opaque handle handle returned to Python as `*mut SurfaceShareHandle`.
    pub struct SurfaceShareHandle {
        socket_path: String,
        runtime_id: String,
        connection: Mutex<Option<UnixStream>>,
        resolve_cache: Mutex<HashMap<String, CachedSurface>>,
        /// Lazily-created per-handle Vulkan device for DMA-BUF import (#420).
        /// Populated on first [`slpn_surface_resolve_surface`] call; dropped
        /// with the handle.
        vulkan_device: Mutex<Option<Arc<SurfaceShareVulkanDevice>>>,
    }

    impl SurfaceShareHandle {
        /// Return a guard over the (possibly lazily-opened) socket connection.
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

        /// Return the per-handle Vulkan device, creating it on first use.
        /// Returns `None` (with a logged reason) if Vulkan is unavailable —
        /// resolve_surface then fails cleanly rather than handing back a
        /// SurfaceHandle whose lock would fail later.
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

    // =========================================================================
    // Wire helpers come from the shared `streamlib-surface-client` crate so the
    // handle server and every polyglot cdylib speak a single-sourced protocol.
    // Aliased as `wire` here to preserve the original call-site shape.
    // =========================================================================
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

    /// Derive a fallback bytes-per-row for a given format string. The handle
    /// wire format emits `format!("{:?}", pixel_buffer.format())` which is the
    /// Debug rendering of the `PixelFormat` enum variant name (e.g.
    /// `"Bgra32"`). Returns `bytes_per_pixel` to compute the default row
    /// stride; real driver-reported stride is a follow-up (handle's lookup
    /// response does not carry it today).
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

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_surface_connect(
        socket_path: *const c_char,
        runtime_id: *const c_char,
    ) -> *mut SurfaceShareHandle {
        let socket_path = match c_str_to_string(socket_path) {
            Some(s) if !s.is_empty() => s,
            _ => {
                tracing::error!("surface_connect (linux): null or empty socket path");
                return std::ptr::null_mut();
            }
        };
        let runtime_id =
            c_str_to_string(runtime_id).unwrap_or_else(|| "python-subprocess".to_string());

        // Intentional: do NOT open the socket yet. Per the research doc,
        // lazy-connect + fail-at-first-use decouples subprocess lifecycle
        // from handle lifecycle.
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
    pub unsafe extern "C" fn slpn_surface_disconnect(handle: *mut SurfaceShareHandle) {
        if !handle.is_null() {
            let _ = unsafe { Box::from_raw(handle) };
            // Drop impls close sockets and all cached fds.
        }
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_surface_resolve_surface(
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
        // [`slpn_gpu_surface_lock`] can import without plumbing the handle
        // pointer through the FFI surface.
        let vulkan_device = match handle.get_or_init_vulkan_device() {
            Some(d) => d,
            None => return std::ptr::null_mut(),
        };

        // Cache hit — dup each stored plane fd so the returned SurfaceHandle
        // owns an independent set of fds.
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
                    vulkan_buffer: vk::Buffer::null(),
                    vulkan_memory: vk::DeviceMemory::null(),
                    backend: SURFACE_BACKEND_NONE,
                }));
            }
        }

        // Cache miss — connect lazily, send check_out, receive fd + metadata.
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

        // Peel off the optional trailing sync-FD when the response carries
        // one. The host's surface-share service appends it to SCM_RIGHTS
        // after the DMA-BUF plane FDs and signals its presence with
        // `has_sync_fd: true` so subprocess code can route it into the
        // Vulkan adapter's timeline-semaphore import (#531).
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

        // Pull plane sizes/offsets from the response. A zero size means
        // "unknown" — the handle did not have per-plane layout at check-in
        // time (e.g. legacy single-plane callers that don't emit
        // `plane_sizes`). Substitute the width*bytes_per_row*height
        // fallback so the Vulkan import path still has a byte count to
        // allocate against.
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
        // `plane_strides` is the source-of-truth row pitch from the host's
        // DRM-modifier-aware allocator. Falls back to width*bpp when the
        // host omits it (legacy producers); EGL DMA-BUF import requires the
        // real stride or the resulting GL_TEXTURE_2D will sample wrong.
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

        // Cache: dup every plane for the cache's own copy so the returned
        // handle owns its own fds independently. Same for the optional
        // sync_fd — the cache and the handed-out handle each get their
        // own dup, so neither closes from underneath the other.
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
            vulkan_buffer: vk::Buffer::null(),
            vulkan_memory: vk::DeviceMemory::null(),
            backend: SURFACE_BACKEND_NONE,
        }))
    }

    /// Allocation on Linux goes through the host's escalate IPC path
    /// (`#325`'s `acquire_pixel_buffer` → `GpuContextFullAccess` → RHI →
    /// `SurfaceStore.check_in`). Returning null here matches the research
    /// doc's safety posture: subprocess native libs are deliberately
    /// consumer-only so RHI invariants (NVIDIA DMA-BUF pool discipline, VMA
    /// export pools, queue-submit mutexing) cover every allocation.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_surface_acquire_surface(
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

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_surface_unregister_surface(
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

        // Evict the cached fd (its Drop closes the fd).
        {
            let mut cache = handle.resolve_cache.lock().expect("poisoned");
            let _ = cache.remove(&pool_id_str);
        }

        // Best-effort release on the wire.
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
    pub unsafe extern "C" fn slpn_surface_connect(
        _xpc_service_name: *const c_char,
        _runtime_id: *const c_char,
    ) -> *mut c_void {
        tracing::error!("Surface-share operations not supported on this platform");
        std::ptr::null_mut()
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_surface_disconnect(_handle: *mut c_void) {}

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_surface_resolve_surface(
        _handle: *mut c_void,
        _pool_id: *const c_char,
    ) -> *mut c_void {
        std::ptr::null_mut()
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_surface_acquire_surface(
        _handle: *mut c_void,
        _width: u32,
        _height: u32,
        _bytes_per_element: u32,
        _out_pool_id: *mut c_char,
        _pool_id_buf_len: u32,
    ) -> *mut c_void {
        std::ptr::null_mut()
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_surface_unregister_surface(
        _handle: *mut c_void,
        _pool_id: *const c_char,
    ) {
    }

}

// ============================================================================
// C ABI — OpenGL/EGL adapter runtime (#530, Linux)
//
// Brings up `streamlib-adapter-opengl::EglRuntime` + `OpenGlSurfaceAdapter`
// inside the subprocess and exposes scoped acquire/release that returns the
// imported `GL_TEXTURE_2D` id. Any GL library (PyOpenGL, ctypes against
// `libGLESv2.so`, a game-engine binding, …) can use the texture as long as
// it operates on whatever EGL context is current on the calling thread —
// `slpn_opengl_acquire_*` makes the adapter's context current and
// `slpn_opengl_release_*` releases it.
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

    /// Process-scoped OpenGL runtime: one EGL display + GL context + adapter
    /// per subprocess. Created on first FFI call; held until the cdylib is
    /// torn down. Multiple per-surface acquires can be in flight serially —
    /// the adapter's make-current mutex serializes EGL/GL access.
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
        // SAFETY-relevant: drop this BEFORE calling `end_*_access` on the
        // adapter — `end_*_access` re-locks the make-current mutex
        // internally for `glFinish`, and the adapter's mutex is not
        // reentrant. The release FFI op handles the ordering.
        make_current: OwnedMakeCurrentGuard,
    }

    /// Initialize an `EglRuntime` + `OpenGlSurfaceAdapter`. Returns NULL
    /// on failure (typically because `libEGL.so.1` is not installed or the
    /// driver doesn't support `EGL_EXT_image_dma_buf_import_modifiers`).
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_opengl_runtime_new() -> *mut OpenGlRuntimeHandle {
        let egl = match EglRuntime::new() {
            Ok(r) => r,
            Err(e) => {
                tracing::error!(
                    "slpn_opengl_runtime_new: EglRuntime::new failed: {}",
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

    /// Release the runtime's adapter, EGL context, and any pending acquire
    /// guards (best-effort — pending acquires are released via the adapter's
    /// `Drop`). Idempotent; null is a no-op.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_opengl_runtime_free(rt: *mut OpenGlRuntimeHandle) {
        if !rt.is_null() {
            let _ = unsafe { Box::from_raw(rt) };
        }
    }

    /// Register a host surface with the OpenGL adapter, importing its
    /// DMA-BUF FD as an `EGLImage` and binding to a fresh `GL_TEXTURE_2D`.
    ///
    /// `surface_id` must be unique across the runtime's lifetime —
    /// duplicates return -1. The caller (Python wrapper) owns the
    /// `surface_id` namespace.
    ///
    /// Reads DMA-BUF FD + plane metadata + DRM modifier from `gpu_handle`
    /// (produced by `slpn_surface_resolve_surface`); the FD is dup'd
    /// internally by EGL.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_opengl_register_surface(
        rt: *mut OpenGlRuntimeHandle,
        surface_id: u64,
        gpu_handle: *const SurfaceHandle,
    ) -> i32 {
        let rt = match unsafe { rt.as_ref() } {
            Some(r) => r,
            None => {
                tracing::error!("slpn_opengl_register_surface: null runtime");
                return -1;
            }
        };
        let gpu = match unsafe { gpu_handle.as_ref() } {
            Some(g) => g,
            None => {
                tracing::error!("slpn_opengl_register_surface: null gpu_handle");
                return -1;
            }
        };
        let fd = match gpu.fds.first().copied() {
            Some(f) if f >= 0 => f,
            _ => {
                tracing::error!(
                    "slpn_opengl_register_surface: surface has no DMA-BUF fd"
                );
                return -1;
            }
        };
        let drm_fourcc = match drm_fourcc_for_format(&gpu.format) {
            Some(c) => c,
            None => {
                tracing::error!(
                    "slpn_opengl_register_surface: unsupported format '{}'",
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
                    "slpn_opengl_register_surface: register_host_surface failed: {:?}",
                    e
                );
                -1
            }
        }
    }

    /// Drop a registered surface from the adapter (releases its EGLImage +
    /// GL_TEXTURE_2D). Returns 0 on success, -1 if not registered.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_opengl_unregister_surface(
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

    /// Acquire write access. Returns the imported `GL_TEXTURE_2D` id, or 0
    /// on contention / failure (a 0 GL texture id is reserved by GL itself,
    /// so it's a safe sentinel). Makes the adapter's EGL context current
    /// on the calling thread for the lifetime of the acquire. Pair every
    /// successful return with `slpn_opengl_release_write` from the SAME
    /// thread.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_opengl_acquire_write(
        rt: *mut OpenGlRuntimeHandle,
        surface_id: u64,
    ) -> u32 {
        acquire_inner(rt, surface_id, HeldKind::Write)
    }

    /// Release write access acquired via [`slpn_opengl_acquire_write`].
    /// Drains the GL command stream (`glFinish`) so cross-API consumers
    /// see the writes. Returns 0 on success, -1 if no write was held.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_opengl_release_write(
        rt: *mut OpenGlRuntimeHandle,
        surface_id: u64,
    ) -> i32 {
        release_inner(rt, surface_id, HeldKind::Write)
    }

    /// Acquire read access. Returns the imported `GL_TEXTURE_2D` id, or 0
    /// on contention / failure.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_opengl_acquire_read(
        rt: *mut OpenGlRuntimeHandle,
        surface_id: u64,
    ) -> u32 {
        acquire_inner(rt, surface_id, HeldKind::Read)
    }

    /// Release read access acquired via [`slpn_opengl_acquire_read`].
    /// Returns 0 on success, -1 if no read was held.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_opengl_release_read(
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
                tracing::error!("slpn_opengl_acquire_*: null runtime");
                return 0;
            }
        };
        let make_current = match rt.egl.arc_lock_make_current() {
            Ok(g) => g,
            Err(e) => {
                tracing::error!(
                    "slpn_opengl_acquire_*: arc_lock_make_current: {}",
                    e
                );
                return 0;
            }
        };
        // The adapter only reads `surface.id` from its acquire path —
        // transport/sync fields are unused for already-registered surfaces.
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
                    // Forget the WriteGuard so its Drop doesn't fire — we
                    // call `end_write_access` manually on release.
                    std::mem::forget(g);
                    t
                }
                Err(e) => {
                    tracing::error!(
                        "slpn_opengl_acquire_write: adapter.acquire_write: {:?}",
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
                        "slpn_opengl_acquire_read: adapter.acquire_read: {:?}",
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
                    "slpn_opengl_release_*: no acquire held for surface_id {}",
                    surface_id
                );
                return -1;
            }
        };
        if !matches!((&held.kind, &expected), (HeldKind::Read, HeldKind::Read) | (HeldKind::Write, HeldKind::Write)) {
            tracing::error!(
                "slpn_opengl_release_*: surface_id {} held in different mode than \
                 release call expected — releasing it anyway",
                surface_id
            );
        }
        // CRITICAL ordering: drop the make-current guard FIRST so the
        // adapter's `end_*_access` can re-lock the EGL mutex internally.
        // `parking_lot::Mutex` is not reentrant.
        drop(held.make_current);
        match held.kind {
            HeldKind::Read => rt.adapter.end_read_access(surface_id),
            HeldKind::Write => rt.adapter.end_write_access(surface_id),
        }
        let _ = held.texture_id; // capture so the field isn't read-warned
        0
    }

    /// Map a host-side `TextureFormat` debug string to the matching DRM
    /// fourcc. Mirrors the Vulkan→DRM byte-order convention: `Bgra8Unorm`
    /// is bytes `[B, G, R, A]` in memory, which is `DRM_FORMAT_ARGB8888`.
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
    pub unsafe extern "C" fn slpn_opengl_runtime_new() -> *mut c_void {
        tracing::error!("slpn_opengl_*: OpenGL adapter runtime is Linux-only");
        std::ptr::null_mut()
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_opengl_runtime_free(_rt: *mut c_void) {}

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_opengl_register_surface(
        _rt: *mut c_void,
        _surface_id: u64,
        _gpu_handle: *const c_void,
    ) -> i32 {
        -1
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_opengl_unregister_surface(
        _rt: *mut c_void,
        _surface_id: u64,
    ) -> i32 {
        -1
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_opengl_acquire_write(
        _rt: *mut c_void,
        _surface_id: u64,
    ) -> u32 {
        0
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_opengl_release_write(
        _rt: *mut c_void,
        _surface_id: u64,
    ) -> i32 {
        -1
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_opengl_acquire_read(
        _rt: *mut c_void,
        _surface_id: u64,
    ) -> u32 {
        0
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_opengl_release_read(
        _rt: *mut c_void,
        _surface_id: u64,
    ) -> i32 {
        -1
    }
}

// ============================================================================
// C ABI — Vulkan adapter runtime (#531, Linux)
//
// Subprocess-side runtime for the Vulkan-native surface adapter. Reuses the
// host adapter crate's `VulkanSurfaceAdapter` against a subprocess-local
// `VulkanDevice` from the RHI: same timeline-wait, same layout-transition,
// same per-surface state machine. The cdylib never re-implements layout
// transitions, command-pool lifetimes, fence handling, or queue-mutex
// coordination — every line of that lives in `streamlib-adapter-vulkan`.
//
// Acquire returns a `SlpnVulkanView` (raw `VkImage` handle + layout) so the
// Python / Deno SDK can dispatch its own raw vulkanalia / Deno-FFI work
// against the imported image. Future tickets (subprocess `ComputeKernel`
// parity, see #525) will close the remaining gap by escalating compute
// dispatches to the host's `GpuContext::create_compute_kernel` path.
// ============================================================================

#[cfg(target_os = "linux")]
mod vulkan {
    use std::collections::HashMap;
    use std::os::unix::io::RawFd;
    use std::sync::{Arc, Mutex};

    use streamlib::adapter_support::{
        VulkanDevice, VulkanTexture, VulkanTimelineSemaphore,
    };
    use streamlib::core::rhi::TextureFormat;
    use streamlib_adapter_abi::{
        StreamlibSurface, SurfaceAdapter as _, SurfaceFormat, SurfaceSyncState,
        SurfaceTransportHandle, SurfaceUsage,
    };
    use streamlib_adapter_vulkan::{
        raw_handles, HostSurfaceRegistration, VulkanLayout, VulkanSurfaceAdapter,
    };
    use vulkanalia::vk;

    use super::gpu_surface::SurfaceHandle;

    /// Process-scoped Vulkan adapter runtime. One `VkDevice` + one
    /// `VulkanSurfaceAdapter` per subprocess; held for the cdylib's life.
    pub struct VulkanRuntimeHandle {
        device: Arc<VulkanDevice>,
        adapter: Arc<VulkanSurfaceAdapter>,
        /// Per-surface book-keeping. The actual texture + timeline are
        /// owned by the adapter (transferred into `HostSurfaceRegistration`);
        /// we keep only the raw `vk::Image` handle so
        /// `slpn_vulkan_dispatch_compute` can look it up without a
        /// round-trip through the adapter's lock + `try_begin_write`.
        registered: Mutex<HashMap<u64, RegisteredSurface>>,
    }

    struct RegisteredSurface {
        /// Cached `vk::Image` handle. The adapter owns the underlying
        /// `VulkanTexture` (and therefore the `VkImage` lifetime); we
        /// just snapshot the handle for fast lookup. Valid until
        /// `unregister_host_surface` drops the adapter's record.
        vk_image: vk::Image,
    }

    /// Returned to Python / Deno via out-pointer on `slpn_vulkan_acquire_*`.
    /// Mirrors `streamlib_adapter_vulkan::VulkanWriteView` but flattened
    /// into a `#[repr(C)]` struct so customers can mmap it via ctypes /
    /// Deno.UnsafePointer.
    #[repr(C)]
    pub struct SlpnVulkanView {
        pub vk_image: u64,
        pub vk_image_layout: i32,
    }

    /// Mirrors `streamlib_adapter_vulkan::RawVulkanHandles`. Returned by
    /// `slpn_vulkan_raw_handles` so power-user callers can drive Vulkan
    /// directly from the same `VkDevice` the adapter uses.
    #[repr(C)]
    pub struct SlpnVulkanRawHandles {
        pub vk_instance: u64,
        pub vk_physical_device: u64,
        pub vk_device: u64,
        pub vk_queue: u64,
        pub vk_queue_family_index: u32,
        pub api_version: u32,
    }

    /// Bring up `VulkanDevice` + `VulkanSurfaceAdapter`. Returns NULL on
    /// failure (typically because the driver doesn't support the required
    /// DMA-BUF / external-semaphore extensions).
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_vulkan_runtime_new() -> *mut VulkanRuntimeHandle {
        let device = match VulkanDevice::new() {
            Ok(d) => Arc::new(d),
            Err(e) => {
                tracing::error!(
                    "slpn_vulkan_runtime_new: VulkanDevice::new failed: {}",
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
    pub unsafe extern "C" fn slpn_vulkan_runtime_free(rt: *mut VulkanRuntimeHandle) {
        if !rt.is_null() {
            let _ = unsafe { Box::from_raw(rt) };
        }
    }

    /// Map the surface-share format string onto the RHI [`TextureFormat`]
    /// the host's allocator picked. `None` means an unsupported format —
    /// the v1 Vulkan adapter only handles 8-bit-per-channel BGRA / RGBA
    /// render targets (every other path currently goes through the
    /// CPU-readback / OpenGL adapters).
    fn texture_format_from_str(format: &str) -> Option<TextureFormat> {
        match format {
            "Bgra8Unorm" => Some(TextureFormat::Bgra8Unorm),
            "Bgra8UnormSrgb" => Some(TextureFormat::Bgra8UnormSrgb),
            "Rgba8Unorm" => Some(TextureFormat::Rgba8Unorm),
            "Rgba8UnormSrgb" => Some(TextureFormat::Rgba8UnormSrgb),
            _ => None,
        }
    }

    /// Register a host surface with the Vulkan adapter — imports the
    /// DMA-BUF FDs as a `VkImage` on the subprocess `VkDevice`, imports
    /// the host's exportable timeline semaphore via OPAQUE_FD, and hands
    /// the `HostSurfaceRegistration` to the adapter.
    ///
    /// On success, the FDs on `gpu_handle` are consumed: the adapter
    /// owns the imported texture / semaphore for the surface's lifetime.
    /// On failure the FDs remain owned by the SurfaceHandle (caller
    /// continues to manage them through `slpn_gpu_surface_release`).
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_vulkan_register_surface(
        rt: *mut VulkanRuntimeHandle,
        surface_id: u64,
        gpu_handle: *mut SurfaceHandle,
    ) -> i32 {
        let rt = match unsafe { rt.as_ref() } {
            Some(r) => r,
            None => {
                tracing::error!("slpn_vulkan_register_surface: null runtime");
                return -1;
            }
        };
        let gpu = match unsafe { gpu_handle.as_mut() } {
            Some(g) => g,
            None => {
                tracing::error!("slpn_vulkan_register_surface: null gpu_handle");
                return -1;
            }
        };
        if gpu.fds.is_empty() {
            tracing::error!(
                "slpn_vulkan_register_surface: surface has no DMA-BUF fds"
            );
            return -1;
        }
        if gpu.drm_format_modifier == 0 {
            tracing::error!(
                "slpn_vulkan_register_surface: surface has DRM_FORMAT_MOD_LINEAR \
                 (zero modifier) — render-target Vulkan import requires a tiled \
                 modifier; see docs/learnings/nvidia-egl-dmabuf-render-target.md"
            );
            return -1;
        }
        let texture_format = match texture_format_from_str(&gpu.format) {
            Some(f) => f,
            None => {
                tracing::error!(
                    "slpn_vulkan_register_surface: unsupported format '{}' \
                     (v1 supports Bgra8Unorm, Bgra8UnormSrgb, Rgba8Unorm, Rgba8UnormSrgb)",
                    gpu.format
                );
                return -1;
            }
        };
        let allocation_size = gpu.size;

        // Import each DMA-BUF FD into the cdylib's VkDevice as a VkImage.
        // `import_render_target_dma_buf` `dup`s every FD internally — the
        // SurfaceHandle keeps its originals so callers can re-import for
        // a second adapter (e.g. CPU-readback alongside Vulkan).
        let texture = match VulkanTexture::import_render_target_dma_buf(
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
                    "slpn_vulkan_register_surface: import_render_target_dma_buf: {}",
                    e
                );
                return -1;
            }
        };
        // Snapshot the `vk::Image` handle BEFORE transferring `texture`
        // into the registration. `VulkanTexture::clone` is a hollow
        // metadata-only clone (`image: None`) by design, so we cannot
        // duplicate the texture itself; only the underlying VkImage
        // handle survives across the move.
        let vk_image = match texture.image() {
            Some(img) => img,
            None => {
                tracing::error!(
                    "slpn_vulkan_register_surface: imported texture has no VkImage handle"
                );
                return -1;
            }
        };

        // Import the host's timeline semaphore. The OPAQUE_FD on the
        // SurfaceHandle is `take`n: Vulkan owns it on success.
        let raw_sync_fd: RawFd = match gpu.sync_fd.take() {
            Some(fd) => fd,
            None => {
                tracing::error!(
                    "slpn_vulkan_register_surface: surface '{}' has no sync_fd — \
                     the host must register the texture with an exportable \
                     `VulkanTimelineSemaphore` (see SurfaceStore::register_texture's \
                     `timeline` argument).",
                    surface_id
                );
                return -1;
            }
        };
        let timeline = match VulkanTimelineSemaphore::from_imported_opaque_fd(
            rt.device.device(),
            raw_sync_fd,
        ) {
            Ok(s) => Arc::new(s),
            Err(e) => {
                // Vulkan retained ownership only on success; restore the
                // SurfaceHandle's slot so the caller can still close it.
                gpu.sync_fd = Some(raw_sync_fd);
                tracing::error!(
                    "slpn_vulkan_register_surface: from_imported_opaque_fd: {}",
                    e
                );
                return -1;
            }
        };

        // The host's `acquire_render_target_dma_buf_image` leaves the
        // image in UNDEFINED initially; subsequent acquires transition
        // through GENERAL / SHADER_READ_ONLY_OPTIMAL. Move `texture`
        // (not `texture.clone()` — Clone is a hollow no-image stub) into
        // the registration so the adapter owns the imported VkImage's
        // lifetime end-to-end.
        let registration = HostSurfaceRegistration {
            texture: streamlib::core::rhi::StreamTexture::from_vulkan(texture),
            timeline,
            initial_layout: VulkanLayout::UNDEFINED,
        };

        if let Err(e) = rt
            .adapter
            .register_host_surface(surface_id, registration)
        {
            tracing::error!(
                "slpn_vulkan_register_surface: register_host_surface({}): {:?}",
                surface_id, e
            );
            return -1;
        }

        rt.registered
            .lock()
            .expect("slpn_vulkan registered: poisoned")
            .insert(surface_id, RegisteredSurface { vk_image });
        0
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_vulkan_unregister_surface(
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
            .expect("slpn_vulkan registered: poisoned")
            .remove(&surface_id);
        if removed.is_none() {
            return -1;
        }
        if rt
            .adapter
            .unregister_host_surface(surface_id)
        {
            0
        } else {
            -1
        }
    }

    /// Acquire write access. Populates `*out_view` with the imported
    /// `VkImage` handle and the layout the adapter transitioned to
    /// (`GENERAL`). Returns 0 on success, -1 on contention / failure.
    /// The acquire is held inside the adapter; pair every successful
    /// acquire with `slpn_vulkan_release_write` from the SAME thread.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_vulkan_acquire_write(
        rt: *mut VulkanRuntimeHandle,
        surface_id: u64,
        out_view: *mut SlpnVulkanView,
    ) -> i32 {
        acquire_inner(rt, surface_id, out_view, AcquireKind::Write)
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_vulkan_release_write(
        rt: *mut VulkanRuntimeHandle,
        surface_id: u64,
    ) -> i32 {
        release_inner(rt, surface_id, AcquireKind::Write)
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_vulkan_acquire_read(
        rt: *mut VulkanRuntimeHandle,
        surface_id: u64,
        out_view: *mut SlpnVulkanView,
    ) -> i32 {
        acquire_inner(rt, surface_id, out_view, AcquireKind::Read)
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_vulkan_release_read(
        rt: *mut VulkanRuntimeHandle,
        surface_id: u64,
    ) -> i32 {
        release_inner(rt, surface_id, AcquireKind::Read)
    }

    /// Power-user surface — the same handles `streamlib_adapter_vulkan::raw_handles`
    /// returns to in-process Rust callers. Subprocess customers wrap them
    /// with their own Vulkan binding for compute / blit work that the
    /// adapter doesn't model. Returns 0 on success, -1 if `out` is null.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_vulkan_raw_handles(
        rt: *mut VulkanRuntimeHandle,
        out: *mut SlpnVulkanRawHandles,
    ) -> i32 {
        let rt = match unsafe { rt.as_ref() } {
            Some(r) => r,
            None => return -1,
        };
        let out = match unsafe { out.as_mut() } {
            Some(o) => o,
            None => return -1,
        };
        let handles = raw_handles(&rt.device);
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
        out_view: *mut SlpnVulkanView,
        kind: AcquireKind,
    ) -> i32 {
        let rt = match unsafe { rt.as_ref() } {
            Some(r) => r,
            None => {
                tracing::error!("slpn_vulkan_acquire_*: null runtime");
                return -1;
            }
        };
        let out_view = match unsafe { out_view.as_mut() } {
            Some(v) => v,
            None => {
                tracing::error!("slpn_vulkan_acquire_*: null out_view");
                return -1;
            }
        };
        // The adapter only reads `surface.id` from its acquire path —
        // transport / sync / format fields are unused for already-
        // registered surfaces, so we synthesize a minimal descriptor.
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
                        // Forget the WriteGuard so its Drop doesn't fire — we
                        // call `end_write_access` manually on release.
                        std::mem::forget(g);
                        0
                    }
                    Err(e) => {
                        tracing::error!(
                            "slpn_vulkan_acquire_write({}): {:?}",
                            surface_id, e
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
                            "slpn_vulkan_acquire_read({}): {:?}",
                            surface_id, e
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
            AcquireKind::Read => {
                rt.adapter
                    .end_read_access(surface_id)
            }
            AcquireKind::Write => {
                rt.adapter
                    .end_write_access(surface_id)
            }
        }
        0
    }

    /// Dispatch a single-binding compute shader against the surface's
    /// imported `VkImage`. The image must be currently held in WRITE
    /// mode (use `slpn_vulkan_acquire_write` first). The shader binds
    /// the image as a `binding=0` storage image and may use up to
    /// `push_constants_size` bytes of push constants.
    ///
    /// **v1 limitation (#525 follow-up).** This function builds the
    /// compute pipeline + descriptor set + command buffer + fence
    /// inline using raw vulkanalia, instead of escalating to the host's
    /// `GpuContext::create_compute_kernel` (which has SPIR-V reflection
    /// + descriptor-set lifetime + pipeline cache). It's a quarantined
    /// bypass — every line lives in this one function, and the
    /// follow-up ticket replaces it with an escalate-IPC
    /// `RunComputeKernel` op once #525's RHI-parity decision lands.
    ///
    /// Submits to the same `VkQueue` the adapter uses for layout
    /// transitions, so this runs serially with adapter activity on
    /// that queue. Blocks until the dispatch + a `vkCmdPipelineBarrier`
    /// (storage-write → memory-read across all stages) have completed,
    /// so the next host-side readback observes the writes.
    ///
    /// Returns 0 on success, negative error code on failure.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_vulkan_dispatch_compute(
        rt: *mut VulkanRuntimeHandle,
        surface_id: u64,
        spv_ptr: *const u8,
        spv_len: usize,
        push_constants_ptr: *const u8,
        push_constants_size: u32,
        group_count_x: u32,
        group_count_y: u32,
        group_count_z: u32,
    ) -> i32 {
        let rt = match unsafe { rt.as_ref() } {
            Some(r) => r,
            None => return -1,
        };
        if spv_ptr.is_null() || spv_len == 0 {
            tracing::error!("slpn_vulkan_dispatch_compute: null/empty spv");
            return -2;
        }
        if spv_len % 4 != 0 {
            tracing::error!(
                "slpn_vulkan_dispatch_compute: spv_len {} not a multiple of 4",
                spv_len
            );
            return -3;
        }
        // Look up the surface's cached VkImage handle. The adapter owns
        // the underlying texture; we cached the handle at register time
        // so dispatch is a single hash-lookup with no adapter-lock.
        let vk_image = {
            let registered = rt
                .registered
                .lock()
                .expect("slpn_vulkan registered: poisoned");
            match registered.get(&surface_id) {
                Some(e) => e.vk_image,
                None => {
                    tracing::error!(
                        "slpn_vulkan_dispatch_compute: surface_id {} not registered",
                        surface_id
                    );
                    return -4;
                }
            }
        };

        let spv: &[u8] = unsafe { std::slice::from_raw_parts(spv_ptr, spv_len) };
        let push_constants: &[u8] = if push_constants_size == 0 {
            &[]
        } else {
            unsafe {
                std::slice::from_raw_parts(
                    push_constants_ptr,
                    push_constants_size as usize,
                )
            }
        };

        match super::vulkan_compute_dispatch::dispatch_storage_image_compute(
            &rt.device,
            vk_image,
            spv,
            push_constants,
            group_count_x,
            group_count_y,
            group_count_z,
        ) {
            Ok(()) => 0,
            Err(e) => {
                tracing::error!("slpn_vulkan_dispatch_compute: {}", e);
                -10
            }
        }
    }
}

#[cfg(target_os = "linux")]
mod vulkan_compute_dispatch {
    //! Quarantined v1 compute-dispatch helper for `slpn_vulkan_dispatch_compute`
    //! / `sldn_vulkan_dispatch_compute`. Builds a single-binding compute
    //! pipeline + descriptor set + command buffer + fence inline using
    //! raw vulkanalia. **Replace with escalate-IPC `RunComputeKernel`
    //! once #525 lands** — the host's `GpuContext::create_compute_kernel`
    //! is the canonical path and has SPIR-V reflection + descriptor-set
    //! lifetime + pipeline cache the cdylib can't replicate.
    //!
    //! Layout assumed by the shader:
    //!   layout(set = 0, binding = 0, rgba8) uniform image2D outputImage;
    //!   layout(push_constant) uniform PushConstants { … } pc;

    use std::sync::Arc;

    use streamlib::adapter_support::VulkanDevice;
    use vulkanalia::prelude::v1_4::*;
    use vulkanalia::vk;

    pub fn dispatch_storage_image_compute(
        device: &Arc<VulkanDevice>,
        image: vk::Image,
        spv: &[u8],
        push_constants: &[u8],
        group_x: u32,
        group_y: u32,
        group_z: u32,
    ) -> Result<(), String> {
        let dev = device.device();
        let queue = device.queue();
        let qf = device.queue_family_index();

        // Re-cast the SPIR-V byte slice as `&[u32]`. vulkanalia's builder
        // wants the u32 word view; `spv_len % 4 == 0` is enforced upstream.
        let spv_words: &[u32] = unsafe {
            std::slice::from_raw_parts(spv.as_ptr() as *const u32, spv.len() / 4)
        };

        // Image view over the imported VkImage so the descriptor binds
        // a 2D storage image. Mirrors the host adapter's `make_image_info`
        // — single mip, single layer, COLOR aspect.
        let view_info = vk::ImageViewCreateInfo::builder()
            .image(image)
            .view_type(vk::ImageViewType::_2D)
            .format(vk::Format::R8G8B8A8_UNORM)
            .components(vk::ComponentMapping::default())
            .subresource_range(
                vk::ImageSubresourceRange::builder()
                    .aspect_mask(vk::ImageAspectFlags::COLOR)
                    .level_count(1)
                    .layer_count(1)
                    .build(),
            )
            .build();
        let image_view = unsafe { dev.create_image_view(&view_info, None) }
            .map_err(|e| format!("create_image_view: {e}"))?;

        let cleanup_view = || unsafe { dev.destroy_image_view(image_view, None) };

        // Descriptor set layout: 1 storage image at binding 0.
        let bindings = [vk::DescriptorSetLayoutBinding::builder()
            .binding(0)
            .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
            .descriptor_count(1)
            .stage_flags(vk::ShaderStageFlags::COMPUTE)
            .build()];
        let dsl_info = vk::DescriptorSetLayoutCreateInfo::builder()
            .bindings(&bindings)
            .build();
        let dsl = unsafe { dev.create_descriptor_set_layout(&dsl_info, None) }
            .map_err(|e| {
                cleanup_view();
                format!("create_descriptor_set_layout: {e}")
            })?;
        let cleanup_dsl = || unsafe { dev.destroy_descriptor_set_layout(dsl, None) };

        // Descriptor pool — single set with one storage image.
        let pool_sizes = [vk::DescriptorPoolSize::builder()
            .type_(vk::DescriptorType::STORAGE_IMAGE)
            .descriptor_count(1)
            .build()];
        let pool_info = vk::DescriptorPoolCreateInfo::builder()
            .pool_sizes(&pool_sizes)
            .max_sets(1)
            .build();
        let dpool = unsafe { dev.create_descriptor_pool(&pool_info, None) }
            .map_err(|e| {
                cleanup_dsl();
                cleanup_view();
                format!("create_descriptor_pool: {e}")
            })?;
        let cleanup_dpool = || unsafe { dev.destroy_descriptor_pool(dpool, None) };

        let layouts = [dsl];
        let alloc_info = vk::DescriptorSetAllocateInfo::builder()
            .descriptor_pool(dpool)
            .set_layouts(&layouts)
            .build();
        let descriptor_set = unsafe { dev.allocate_descriptor_sets(&alloc_info) }
            .map_err(|e| {
                cleanup_dpool();
                cleanup_dsl();
                cleanup_view();
                format!("allocate_descriptor_sets: {e}")
            })?[0];

        let image_info = [vk::DescriptorImageInfo::builder()
            .image_view(image_view)
            .image_layout(vk::ImageLayout::GENERAL)
            .build()];
        let writes = [vk::WriteDescriptorSet::builder()
            .dst_set(descriptor_set)
            .dst_binding(0)
            .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
            .image_info(&image_info)
            .build()];
        unsafe {
            dev.update_descriptor_sets(
                &writes,
                &[] as &[vk::CopyDescriptorSet],
            )
        };

        // Pipeline layout — descriptor set + optional push constants.
        let push_const_ranges: Vec<vk::PushConstantRange> = if push_constants.is_empty() {
            Vec::new()
        } else {
            vec![vk::PushConstantRange::builder()
                .stage_flags(vk::ShaderStageFlags::COMPUTE)
                .offset(0)
                .size(push_constants.len() as u32)
                .build()]
        };
        let mut pl_builder = vk::PipelineLayoutCreateInfo::builder().set_layouts(&layouts);
        if !push_const_ranges.is_empty() {
            pl_builder = pl_builder.push_constant_ranges(&push_const_ranges);
        }
        let pipeline_layout =
            unsafe { dev.create_pipeline_layout(&pl_builder.build(), None) }.map_err(|e| {
                cleanup_dpool();
                cleanup_dsl();
                cleanup_view();
                format!("create_pipeline_layout: {e}")
            })?;
        let cleanup_pl = || unsafe { dev.destroy_pipeline_layout(pipeline_layout, None) };

        // Shader module from SPIR-V.
        let shader_info = vk::ShaderModuleCreateInfo::builder()
            .code(spv_words)
            .build();
        let shader = unsafe { dev.create_shader_module(&shader_info, None) }.map_err(|e| {
            cleanup_pl();
            cleanup_dpool();
            cleanup_dsl();
            cleanup_view();
            format!("create_shader_module: {e}")
        })?;
        let cleanup_shader = || unsafe { dev.destroy_shader_module(shader, None) };

        // Compute pipeline.
        let entry_name = b"main\0";
        let stage = vk::PipelineShaderStageCreateInfo::builder()
            .stage(vk::ShaderStageFlags::COMPUTE)
            .module(shader)
            .name(entry_name)
            .build();
        let pipeline_info = vk::ComputePipelineCreateInfo::builder()
            .stage(stage)
            .layout(pipeline_layout)
            .build();
        let (pipelines, _result_code) = unsafe {
            dev.create_compute_pipelines(
                vk::PipelineCache::null(),
                &[pipeline_info],
                None,
            )
        }
        .map_err(|e| {
            cleanup_shader();
            cleanup_pl();
            cleanup_dpool();
            cleanup_dsl();
            cleanup_view();
            format!("create_compute_pipelines: {:?}", e)
        })?;
        let pipeline = pipelines[0];
        let cleanup_pipeline = || unsafe { dev.destroy_pipeline(pipeline, None) };

        // Command pool + buffer.
        let pool_info = vk::CommandPoolCreateInfo::builder()
            .queue_family_index(qf)
            .flags(vk::CommandPoolCreateFlags::TRANSIENT)
            .build();
        let cmd_pool = unsafe { dev.create_command_pool(&pool_info, None) }.map_err(|e| {
            cleanup_pipeline();
            cleanup_shader();
            cleanup_pl();
            cleanup_dpool();
            cleanup_dsl();
            cleanup_view();
            format!("create_command_pool: {e}")
        })?;
        let cleanup_cmd_pool = || unsafe { dev.destroy_command_pool(cmd_pool, None) };

        let cmd_alloc = vk::CommandBufferAllocateInfo::builder()
            .command_pool(cmd_pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1)
            .build();
        let cmd = unsafe { dev.allocate_command_buffers(&cmd_alloc) }.map_err(|e| {
            cleanup_cmd_pool();
            cleanup_pipeline();
            cleanup_shader();
            cleanup_pl();
            cleanup_dpool();
            cleanup_dsl();
            cleanup_view();
            format!("allocate_command_buffers: {e}")
        })?[0];

        let begin = vk::CommandBufferBeginInfo::builder()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT)
            .build();
        unsafe { dev.begin_command_buffer(cmd, &begin) }
            .map_err(|e| format!("begin_command_buffer: {e}"))?;

        unsafe {
            dev.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, pipeline);
            dev.cmd_bind_descriptor_sets(
                cmd,
                vk::PipelineBindPoint::COMPUTE,
                pipeline_layout,
                0,
                &[descriptor_set],
                &[],
            );
            if !push_constants.is_empty() {
                dev.cmd_push_constants(
                    cmd,
                    pipeline_layout,
                    vk::ShaderStageFlags::COMPUTE,
                    0,
                    push_constants,
                );
            }
            dev.cmd_dispatch(cmd, group_x, group_y, group_z);
        }

        // Barrier so the host's subsequent transfer / readback sees the
        // dispatched writes.
        let barrier = vk::ImageMemoryBarrier2::builder()
            .src_stage_mask(vk::PipelineStageFlags2::COMPUTE_SHADER)
            .src_access_mask(vk::AccessFlags2::SHADER_STORAGE_WRITE)
            .dst_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
            .dst_access_mask(vk::AccessFlags2::MEMORY_READ)
            .old_layout(vk::ImageLayout::GENERAL)
            .new_layout(vk::ImageLayout::GENERAL)
            .src_queue_family_index(qf)
            .dst_queue_family_index(qf)
            .image(image)
            .subresource_range(
                vk::ImageSubresourceRange::builder()
                    .aspect_mask(vk::ImageAspectFlags::COLOR)
                    .level_count(1)
                    .layer_count(1)
                    .build(),
            )
            .build();
        let barriers = [barrier];
        let dep = vk::DependencyInfo::builder()
            .image_memory_barriers(&barriers)
            .build();
        unsafe { dev.cmd_pipeline_barrier2(cmd, &dep) };

        unsafe { dev.end_command_buffer(cmd) }
            .map_err(|e| format!("end_command_buffer: {e}"))?;

        let cmd_infos = [vk::CommandBufferSubmitInfo::builder()
            .command_buffer(cmd)
            .build()];
        let submit = vk::SubmitInfo2::builder()
            .command_buffer_infos(&cmd_infos)
            .build();
        unsafe { device.submit_to_queue(queue, &[submit], vk::Fence::null()) }
            .map_err(|e| format!("submit_to_queue: {e}"))?;
        unsafe { dev.queue_wait_idle(queue) }
            .map_err(|e| format!("queue_wait_idle: {e}"))?;

        cleanup_cmd_pool();
        cleanup_pipeline();
        cleanup_shader();
        cleanup_pl();
        cleanup_dpool();
        cleanup_dsl();
        cleanup_view();
        Ok(())
    }
}

#[cfg(not(target_os = "linux"))]
mod vulkan {
    use std::ffi::c_void;

    #[repr(C)]
    pub struct SlpnVulkanView {
        pub vk_image: u64,
        pub vk_image_layout: i32,
    }

    #[repr(C)]
    pub struct SlpnVulkanRawHandles {
        pub vk_instance: u64,
        pub vk_physical_device: u64,
        pub vk_device: u64,
        pub vk_queue: u64,
        pub vk_queue_family_index: u32,
        pub api_version: u32,
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_vulkan_runtime_new() -> *mut c_void {
        tracing::error!("slpn_vulkan_*: Vulkan adapter runtime is Linux-only");
        std::ptr::null_mut()
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_vulkan_runtime_free(_rt: *mut c_void) {}

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_vulkan_register_surface(
        _rt: *mut c_void,
        _surface_id: u64,
        _gpu_handle: *mut c_void,
    ) -> i32 {
        -1
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_vulkan_unregister_surface(
        _rt: *mut c_void,
        _surface_id: u64,
    ) -> i32 {
        -1
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_vulkan_acquire_write(
        _rt: *mut c_void,
        _surface_id: u64,
        _out_view: *mut SlpnVulkanView,
    ) -> i32 {
        -1
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_vulkan_release_write(
        _rt: *mut c_void,
        _surface_id: u64,
    ) -> i32 {
        -1
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_vulkan_acquire_read(
        _rt: *mut c_void,
        _surface_id: u64,
        _out_view: *mut SlpnVulkanView,
    ) -> i32 {
        -1
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_vulkan_release_read(
        _rt: *mut c_void,
        _surface_id: u64,
    ) -> i32 {
        -1
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_vulkan_raw_handles(
        _rt: *mut c_void,
        _out: *mut SlpnVulkanRawHandles,
    ) -> i32 {
        -1
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_vulkan_dispatch_compute(
        _rt: *mut c_void,
        _surface_id: u64,
        _spv_ptr: *const u8,
        _spv_len: usize,
        _push_constants_ptr: *const u8,
        _push_constants_size: u32,
        _group_count_x: u32,
        _group_count_y: u32,
        _group_count_z: u32,
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

