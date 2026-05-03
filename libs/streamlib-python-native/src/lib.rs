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
use std::sync::{Mutex, OnceLock};

use iceoryx2::port::listener::Listener;
use iceoryx2::port::notifier::Notifier;
use iceoryx2::port::publisher::Publisher;
use iceoryx2::port::subscriber::Subscriber;
use iceoryx2::prelude::*;
use streamlib_ipc_types::{FrameHeader, FRAME_HEADER_SIZE, MAX_FANIN_PER_DESTINATION};

// ============================================================================
// Tracing subscriber init
// ============================================================================

/// Install a fmt subscriber on first FFI entry so the cdylib's
/// `tracing::error!` / `tracing::warn!` events surface to subprocess
/// stderr, where the host's `spawn_fd_line_reader` picks them up and
/// forwards them under the `[/python]` log namespace via fd2.
///
/// Idempotent — subsequent calls hit the `OnceLock` and no-op. Filter
/// honors `RUST_LOG` if set, otherwise defaults to `info`.
fn init_subprocess_logging() {
    static INIT: OnceLock<()> = OnceLock::new();
    INIT.get_or_init(|| {
        let _ = tracing_subscriber::fmt()
            .with_writer(std::io::stderr)
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
            )
            .try_init();
    });
}

// ============================================================================
// Context
// ============================================================================

/// How frames should be read from an input port's buffer.
/// Mirrors the Rust-side `ReadMode` enum in `streamlib::iceoryx2::read_mode`.
const READ_MODE_SKIP_TO_LATEST: i32 = 0;
const READ_MODE_READ_NEXT_IN_ORDER: i32 = 1;

/// Per-processor native context holding iceoryx2 node and port state.
///
/// Mutable interior state lives behind [`Self::inner`]'s [`Mutex`] so that
/// concurrent FFI calls from different Python threads cannot alias `&mut`
/// against each other (#604). Python's `ctypes` releases the GIL on FFI
/// dispatch by default — without this lock, two threads in
/// `slpn_output_write` against the same context would each materialize a
/// `&mut PythonNativeContext` and instantly violate Rust's aliasing rules.
///
/// `node` is immutable after construction (its `service_builder` only
/// borrows `&self`) so it sits outside the lock to keep the locked region
/// small.
pub struct PythonNativeContext {
    processor_id: String,
    node: Node<ipc::Service>,
    inner: Mutex<PythonNativeContextInner>,
}

/// Mutable iceoryx2 port state guarded by [`PythonNativeContext::inner`].
struct PythonNativeContextInner {
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
            inner: Mutex::new(PythonNativeContextInner {
                subscribers: HashMap::new(),
                publishers: HashMap::new(),
                port_read_modes: HashMap::new(),
                notify_listener: None,
            }),
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
    init_subprocess_logging();
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

/// Current monotonic time in nanoseconds via `clock_gettime(CLOCK_MONOTONIC)`.
///
/// Values are comparable across processes on the same kernel — to host
/// `Instant` reads and to the Deno cdylib's [`sldn_monotonic_now_ns`].
/// The canonical timestamp source for ALL polyglot work; do not use
/// wall-clock APIs (`time.time`, `Date.now`) for cross-process timing.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn slpn_monotonic_now_ns() -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: ts is a valid stack slot. CLOCK_MONOTONIC is supported on
    // every platform this cdylib targets (Linux, macOS); the only failure
    // mode is EFAULT/EINVAL, which our arguments make unreachable.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    (ts.tv_sec as u64)
        .saturating_mul(1_000_000_000)
        .saturating_add(ts.tv_nsec as u64)
}

// ============================================================================
// C ABI — Monotonic interval timer via timerfd (Linux)
// ============================================================================
//
// Mirrors `LinuxTimerFdAudioClock` (libs/streamlib/src/linux/audio_clock.rs):
// `timerfd_create(CLOCK_MONOTONIC)` + `TFD_TIMER_ABSTIME` for drift-free
// periodic firing, plus an internal epoll fd so `slpn_timerfd_wait` can
// honor a caller-supplied timeout — that timeout is what bounds teardown
// latency in the subprocess runner's continuous-mode dispatch.
//
// The handle is opaque to the SDK; never dereferenced in Python ctypes /
// Deno FFI. On non-Linux platforms the symbols still exist (so the
// cdylib loads) but always report failure.

pub struct TimerFdHandle {
    #[cfg(target_os = "linux")]
    timer_fd: i32,
    #[cfg(target_os = "linux")]
    epoll_fd: i32,
}

/// Create a periodic monotonic timer firing every `interval_ns`.
///
/// Returns an opaque handle on success, or null on failure. `interval_ns`
/// must be > 0.
#[cfg(target_os = "linux")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn slpn_timerfd_create(interval_ns: u64) -> *mut TimerFdHandle {
    timerfd::create(interval_ns)
}

/// Wait up to `timeout_ms` for the next tick.
///
/// Returns: positive expiration count if a tick fired, 0 on timeout (no
/// tick yet), -1 on error.
#[cfg(target_os = "linux")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn slpn_timerfd_wait(handle: *mut TimerFdHandle, timeout_ms: i32) -> i64 {
    timerfd::wait(handle, timeout_ms)
}

/// Close the timer and free the handle.
#[cfg(target_os = "linux")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn slpn_timerfd_close(handle: *mut TimerFdHandle) {
    timerfd::close(handle)
}

#[cfg(not(target_os = "linux"))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn slpn_timerfd_create(_interval_ns: u64) -> *mut TimerFdHandle {
    std::ptr::null_mut()
}

#[cfg(not(target_os = "linux"))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn slpn_timerfd_wait(_handle: *mut TimerFdHandle, _timeout_ms: i32) -> i64 {
    -1
}

#[cfg(not(target_os = "linux"))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn slpn_timerfd_close(_handle: *mut TimerFdHandle) {}

#[cfg(target_os = "linux")]
mod timerfd {
    use super::TimerFdHandle;

    pub(super) fn create(interval_ns: u64) -> *mut TimerFdHandle {
        if interval_ns == 0 {
            return std::ptr::null_mut();
        }
        let interval_sec = (interval_ns / 1_000_000_000) as libc::time_t;
        let interval_nsec = (interval_ns % 1_000_000_000) as libc::c_long;

        let timer_fd = unsafe { libc::timerfd_create(libc::CLOCK_MONOTONIC, 0) };
        if timer_fd < 0 {
            return std::ptr::null_mut();
        }

        let mut now = libc::timespec { tv_sec: 0, tv_nsec: 0 };
        if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut now) } < 0 {
            unsafe { libc::close(timer_fd) };
            return std::ptr::null_mut();
        }

        let mut first_sec = now.tv_sec;
        let mut first_nsec = now.tv_nsec + interval_nsec;
        if first_nsec >= 1_000_000_000 {
            first_sec += (first_nsec / 1_000_000_000) as libc::time_t;
            first_nsec %= 1_000_000_000;
        }
        first_sec += interval_sec;

        let spec = libc::itimerspec {
            it_interval: libc::timespec { tv_sec: interval_sec, tv_nsec: interval_nsec },
            it_value: libc::timespec { tv_sec: first_sec, tv_nsec: first_nsec },
        };
        let set_ret = unsafe {
            libc::timerfd_settime(timer_fd, libc::TFD_TIMER_ABSTIME, &spec, std::ptr::null_mut())
        };
        if set_ret < 0 {
            unsafe { libc::close(timer_fd) };
            return std::ptr::null_mut();
        }

        let epoll_fd = unsafe { libc::epoll_create1(libc::EPOLL_CLOEXEC) };
        if epoll_fd < 0 {
            unsafe { libc::close(timer_fd) };
            return std::ptr::null_mut();
        }
        let mut event = libc::epoll_event { events: libc::EPOLLIN as u32, u64: 0 };
        if unsafe { libc::epoll_ctl(epoll_fd, libc::EPOLL_CTL_ADD, timer_fd, &mut event) } < 0 {
            unsafe {
                libc::close(epoll_fd);
                libc::close(timer_fd);
            }
            return std::ptr::null_mut();
        }

        Box::into_raw(Box::new(TimerFdHandle { timer_fd, epoll_fd }))
    }

    pub(super) fn wait(handle: *mut TimerFdHandle, timeout_ms: i32) -> i64 {
        let handle = match unsafe { handle.as_ref() } {
            Some(h) => h,
            None => return -1,
        };
        let mut events = [libc::epoll_event { events: 0, u64: 0 }; 1];
        let nfds = unsafe { libc::epoll_wait(handle.epoll_fd, events.as_mut_ptr(), 1, timeout_ms) };
        if nfds < 0 {
            return if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                0
            } else {
                -1
            };
        }
        if nfds == 0 {
            return 0;
        }
        let mut expirations: u64 = 0;
        let read_ret = unsafe {
            libc::read(
                handle.timer_fd,
                &mut expirations as *mut u64 as *mut libc::c_void,
                std::mem::size_of::<u64>(),
            )
        };
        if read_ret < 0 {
            return if std::io::Error::last_os_error().kind() == std::io::ErrorKind::WouldBlock {
                0
            } else {
                -1
            };
        }
        expirations.min(i64::MAX as u64) as i64
    }

    pub(super) fn close(handle: *mut TimerFdHandle) {
        if handle.is_null() {
            return;
        }
        let h = unsafe { Box::from_raw(handle) };
        unsafe {
            libc::close(h.epoll_fd);
            libc::close(h.timer_fd);
        }
    }
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
    let ctx = match unsafe { ctx.as_ref() } {
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

    let mut inner = match ctx.inner.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    inner.subscribers.insert(
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
    let ctx = match unsafe { ctx.as_ref() } {
        Some(c) => c,
        None => return -1,
    };
    let port_name = match unsafe { c_str_to_str(port_name) } {
        Some(s) => s,
        None => return -1,
    };

    let mut inner = match ctx.inner.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    inner.port_read_modes.insert(port_name.to_string(), mode);
    0
}

/// Poll all subscribed services for new data.
///
/// Returns 1 if any data was received, 0 if none, -1 on error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn slpn_input_poll(ctx: *mut PythonNativeContext) -> i32 {
    let ctx = match unsafe { ctx.as_ref() } {
        Some(c) => c,
        None => return -1,
    };

    let mut inner = match ctx.inner.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };

    let mut has_data = false;

    for (_service_name, state) in inner.subscribers.iter_mut() {
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
    let ctx = match unsafe { ctx.as_ref() } {
        Some(c) => c,
        None => return -1,
    };
    let port_name = match unsafe { c_str_to_str(port_name) } {
        Some(s) => s,
        None => return -1,
    };

    let mut inner = match ctx.inner.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };

    let read_mode = inner
        .port_read_modes
        .get(port_name)
        .copied()
        .unwrap_or(READ_MODE_SKIP_TO_LATEST);

    // Search all subscribers for pending data on this port
    for (_service_name, state) in inner.subscribers.iter_mut() {
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
    let ctx = match unsafe { ctx.as_ref() } {
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

    let mut inner = match ctx.inner.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    inner.publishers.insert(
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
/// Safe to call from any thread, including concurrently with other
/// `slpn_*` ops on the same context — the inner [`Mutex`] serializes
/// access to the iceoryx2 publisher map (#604). The lock is held through
/// `loan_slice_uninit` + `send` + `notify`; iceoryx2's publisher path is
/// lock-free zero-copy so the held duration is short.
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
    let ctx = match unsafe { ctx.as_ref() } {
        Some(c) => c,
        None => return -1,
    };
    let port_name = match unsafe { c_str_to_str(port_name) } {
        Some(s) => s,
        None => return -1,
    };

    let inner = match ctx.inner.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };

    let state = match inner.publishers.get(port_name) {
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
    let ctx = match unsafe { ctx.as_ref() } {
        Some(c) => c,
        None => return -1,
    };
    {
        let inner = match ctx.inner.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        if inner.notify_listener.is_some() {
            return 0;
        }
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
    let mut inner = match ctx.inner.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    // Re-check under the lock — another thread may have raced ahead.
    if inner.notify_listener.is_none() {
        inner.notify_listener = Some(listener);
    }
    0
}

/// Returns the underlying listener fd for `select`/`poll`, or -1 if not subscribed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn slpn_event_listener_fd(ctx: *mut PythonNativeContext) -> i32 {
    let ctx = match unsafe { ctx.as_ref() } {
        Some(c) => c,
        None => return -1,
    };
    let inner = match ctx.inner.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    match inner.notify_listener.as_ref() {
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
    let ctx = match unsafe { ctx.as_ref() } {
        Some(c) => c,
        None => return -1,
    };
    let inner = match ctx.inner.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    let Some(listener) = inner.notify_listener.as_ref() else {
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
    //! same shape as the host's `HostVulkanPixelBuffer::from_dma_buf_fd` so both
    //! ends speak the canonical driver-supported path. The import-side only —
    //! allocation always escalates to the host per the research doc.
    use std::ffi::c_void;
    use std::os::unix::io::RawFd;
    use std::sync::Arc;

    use streamlib_consumer_rhi::{ConsumerVulkanDevice, ConsumerVulkanPixelBuffer, PixelFormat};

    /// Surface backend used for the currently-locked mapping. Reported via
    /// [`slpn_gpu_surface_backend`] so tests can assert the import took the
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
        /// One fd per DMA-BUF plane. Single-plane surfaces carry a
        /// one-element vec; multi-plane DMA-BUFs (e.g. NV12 under DRM format
        /// modifiers with disjoint Y/UV allocations) carry one per plane,
        /// keyed by plane index.
        pub fds: Vec<RawFd>,
        /// Optional OPAQUE_FD timeline-semaphore handle the host attached
        /// when registering the surface (#531). Routed into the Vulkan
        /// adapter's `ConsumerVulkanTimelineSemaphore::from_imported_opaque_fd` so
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
        /// Producer-declared `VkImageLayout` (i32 per Vulkan spec) read
        /// from the surface-share lookup response (#633). The Vulkan /
        /// CUDA / cpu-readback adapters' `register_host_surface` paths
        /// pass this into `HostSurfaceRegistration::initial_layout` so
        /// the adapter's per-surface `current_layout` matches the
        /// producer's claim from the first acquire onward. `0`
        /// (UNDEFINED) for surfaces registered without a declared layout
        /// (back-compat).
        pub current_image_layout: i32,
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
        /// Consumer-side Vulkan device attached by
        /// [`super::surface_client::slpn_surface_resolve_surface`].
        /// `None` means the service could not create a Vulkan device and
        /// lock will fail cleanly.
        pub vulkan_device: Option<Arc<ConsumerVulkanDevice>>,
        /// Imported pixel buffer — `Some` only while `is_locked`. Drop
        /// runs `vkDestroyBuffer` + `vkFreeMemory` via the consumer-rhi
        /// teardown path; `slpn_gpu_surface_unlock` takes() to tear down
        /// without dropping the surface handle.
        pub imported_pixel_buffer: Option<ConsumerVulkanPixelBuffer>,
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
            // Tear down any outstanding Vulkan import (lock without
            // unlock). `lock` imports a dup of `self.fds[0]`; Vulkan
            // owns that dup. Dropping the `ConsumerVulkanPixelBuffer`
            // runs `vkDestroyBuffer` + `vkFreeMemory`, which releases
            // the dup — not our fds.
            let _ = self.imported_pixel_buffer.take();
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

    /// Producer-declared `VkImageLayout` (raw i32 per Vulkan spec) from
    /// the surface-share lookup response (#633). `0` (UNDEFINED) on
    /// null handle or for surfaces registered without a declared layout.
    /// Used by adapter `register_host_surface` paths to seed
    /// `HostSurfaceRegistration::initial_layout` so the adapter's
    /// per-surface `current_layout` matches the producer's claim from
    /// the first acquire onward.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_gpu_surface_initial_image_layout(
        handle: *const SurfaceHandle,
    ) -> i32 {
        unsafe { handle.as_ref() }
            .map(|h| h.current_image_layout)
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

    use streamlib_consumer_rhi::ConsumerVulkanDevice;

    use super::gpu_surface::{SurfaceHandle, SURFACE_BACKEND_NONE};

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
        /// Producer-declared `VkImageLayout` mirrored from the
        /// surface-share lookup response (#633). Cached here so cache
        /// hits can re-emit the same value on the returned
        /// `SurfaceHandle` without another wire roundtrip.
        current_image_layout: i32,
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
        /// Lazily-created per-handle consumer-side Vulkan device for
        /// DMA-BUF import. Populated on first
        /// [`slpn_surface_resolve_surface`] call; dropped with the handle.
        vulkan_device: Mutex<Option<Arc<ConsumerVulkanDevice>>>,
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
        // a SurfaceHandle. Every handle carries an Arc<ConsumerVulkanDevice> so
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
                    current_image_layout: cached.current_image_layout,
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
        // Producer-declared `VkImageLayout` from the surface-share
        // lookup response (#633). `0` (UNDEFINED) is the back-compat
        // default for surfaces registered before the field landed.
        let current_image_layout = response
            .get("current_image_layout")
            .and_then(|v| v.as_i64())
            .map(|v| v as i32)
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
                    current_image_layout,
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
            current_image_layout,
            format: format_str.to_string(),
            mapped_ptr: std::ptr::null_mut(),
            plane_mapped_ptrs: vec![std::ptr::null_mut(); n_planes],
            is_locked: false,
            vulkan_device: Some(vulkan_device),
            imported_pixel_buffer: None,
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
        super::init_subprocess_logging();
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

    /// Register a host surface as a sampler-only `GL_TEXTURE_EXTERNAL_OES`.
    ///
    /// Same DMA-BUF + plane metadata extraction as
    /// [`slpn_opengl_register_surface`], but routes through
    /// `OpenGlSurfaceAdapter::register_external_oes_host_surface` so the
    /// resulting GL texture is bound under `GL_TEXTURE_EXTERNAL_OES`.
    /// Use this for surfaces whose modifier is reported `external_only=TRUE`
    /// by `eglQueryDmaBufModifiersEXT` (NVIDIA + linear DMA-BUFs — see
    /// `docs/learnings/nvidia-egl-dmabuf-render-target.md`); typical
    /// consumer is a per-frame camera ring texture.
    ///
    /// The customer's GLSL must enable `samplerExternalOES`. On the
    /// adapter's desktop-GL context, declare
    /// `#extension GL_OES_EGL_image_external : require` and sample via
    /// `texture2D(samplerExternalOES, vec2)` — NVIDIA's desktop-GL
    /// driver does NOT register the unified `texture(...)` overload
    /// for `samplerExternalOES` in `#version 330 core`; that overload
    /// comes from `GL_OES_EGL_image_external_essl3`, which requires a
    /// GLES context (not what this adapter creates). Acquire/release
    /// uses the same [`slpn_opengl_acquire_read`] /
    /// [`slpn_opengl_release_read`] pair as the 2D path; only
    /// `acquire_write` is rejected (the EXTERNAL_OES binding is
    /// sample-only by GL spec).
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_opengl_register_external_oes_surface(
        rt: *mut OpenGlRuntimeHandle,
        surface_id: u64,
        gpu_handle: *const SurfaceHandle,
    ) -> i32 {
        let rt = match unsafe { rt.as_ref() } {
            Some(r) => r,
            None => {
                tracing::error!(
                    "slpn_opengl_register_external_oes_surface: null runtime"
                );
                return -1;
            }
        };
        let gpu = match unsafe { gpu_handle.as_ref() } {
            Some(g) => g,
            None => {
                tracing::error!(
                    "slpn_opengl_register_external_oes_surface: null gpu_handle"
                );
                return -1;
            }
        };
        let fd = match gpu.fds.first().copied() {
            Some(f) if f >= 0 => f,
            _ => {
                tracing::error!(
                    fds = ?gpu.fds,
                    format = %gpu.format,
                    width = gpu.width,
                    height = gpu.height,
                    modifier = format_args!("0x{:016x}", gpu.drm_format_modifier),
                    "slpn_opengl_register_external_oes_surface: surface has no DMA-BUF fd"
                );
                return -1;
            }
        };
        let drm_fourcc = match drm_fourcc_for_format(&gpu.format) {
            Some(c) => c,
            None => {
                tracing::error!(
                    format = %gpu.format,
                    "slpn_opengl_register_external_oes_surface: unsupported format"
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
        match rt
            .adapter
            .register_external_oes_host_surface(surface_id, registration)
        {
            Ok(()) => 0,
            Err(e) => {
                tracing::error!(
                    surface_id,
                    fourcc = format_args!("0x{:08x}", drm_fourcc),
                    modifier = format_args!("0x{:016x}", gpu.drm_format_modifier),
                    width = gpu.width,
                    height = gpu.height,
                    fd,
                    stride,
                    offset,
                    error = ?e,
                    "slpn_opengl_register_external_oes_surface: register_external_oes_host_surface failed"
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
            // TextureFormat-derived strings (host-allocated render-target
            // surfaces emit these in the surface-share wire format).
            "Bgra8Unorm" | "Bgra8UnormSrgb" => Some(DRM_FORMAT_ARGB8888),
            "Rgba8Unorm" | "Rgba8UnormSrgb" => Some(DRM_FORMAT_ABGR8888),
            // PixelFormat-derived strings (host-allocated HOST_VISIBLE
            // pixel buffers emit these — camera ring `acquire_pixel_buffer`,
            // CPU-readback adapter, etc.).
            "Bgra32" | "Argb32" => Some(DRM_FORMAT_ARGB8888),
            "Rgba32" => Some(DRM_FORMAT_ABGR8888),
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
    pub unsafe extern "C" fn slpn_opengl_register_external_oes_surface(
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
// `ConsumerVulkanDevice` from the RHI: same timeline-wait, same layout-transition,
// same per-surface state machine. The cdylib never re-implements layout
// transitions, command-pool lifetimes, fence handling, or queue-mutex
// coordination — every line of that lives in `streamlib-adapter-vulkan`.
//
// Acquire returns a `SlpnVulkanView` (raw `VkImage` handle + layout) so the
// Python SDK can dispatch its own work against the imported image. Compute
// dispatches escalate to the host's `GpuContext::create_compute_kernel` via
// the `register_compute_kernel` / `run_compute_kernel` IPC ops (#550) — no
// raw-vulkanalia compute lives in this cdylib.
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

    /// Process-scoped Vulkan adapter runtime. One `VkDevice` + one
    /// `VulkanSurfaceAdapter` per subprocess; held for the cdylib's life.
    pub struct VulkanRuntimeHandle {
        device: Arc<ConsumerVulkanDevice>,
        adapter: Arc<VulkanSurfaceAdapter<ConsumerVulkanDevice>>,
        /// Per-surface book-keeping. The actual texture + timeline are
        /// owned by the adapter (transferred into
        /// `HostSurfaceRegistration`); this map only tracks which
        /// surface_ids have been registered so the FFI boundary can
        /// reject double-registers / double-unregisters cleanly.
        registered: Mutex<HashMap<u64, RegisteredSurface>>,
    }

    /// Tracks which surface_ids have been registered. The adapter owns
    /// the imported VkImage + timeline; this registry only exists to
    /// reject double-registers / double-unregisters at the FFI boundary.
    struct RegisteredSurface;

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

    /// Per-image VkImageInfo descriptor for a registered surface.
    /// Mirrors `streamlib_adapter_abi::VkImageInfo` field-for-field —
    /// kept as a separate `#[repr(C)]` here so the cdylib's ABI
    /// surface stays self-contained (the SDK doesn't need to mirror
    /// `streamlib-adapter-abi` to read this).
    ///
    /// Per-image (fixed at registration), NOT per-acquire. Polyglot
    /// Skia wrappers call `slpn_vulkan_get_image_info` once per
    /// registered surface to populate their backend-context state;
    /// per-acquire `vk_image_layout` still flows through
    /// [`SlpnVulkanView`].
    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct SlpnVulkanImageInfo {
        pub format: i32,
        pub tiling: i32,
        pub usage_flags: u32,
        pub sample_count: u32,
        pub level_count: u32,
        pub queue_family: u32,
        pub memory_handle: u64,
        pub memory_offset: u64,
        pub memory_size: u64,
        pub memory_property_flags: u32,
        pub protected: u32,
        pub ycbcr_conversion: u64,
        pub _reserved: [u8; 16],
    }

    /// Bring up `ConsumerVulkanDevice` + `VulkanSurfaceAdapter`. Returns NULL on
    /// failure (typically because the driver doesn't support the required
    /// DMA-BUF / external-semaphore extensions).
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_vulkan_runtime_new() -> *mut VulkanRuntimeHandle {
        super::init_subprocess_logging();
        let device = match ConsumerVulkanDevice::new() {
            Ok(d) => Arc::new(d),
            Err(e) => {
                tracing::error!(
                    "slpn_vulkan_runtime_new: ConsumerVulkanDevice::new failed: {}",
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
                    "slpn_vulkan_register_surface: import_render_target_dma_buf: {}",
                    e
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
                     `ConsumerVulkanTimelineSemaphore` (see SurfaceStore::register_texture's \
                     `timeline` argument).",
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

        // Seed the adapter's per-surface `current_layout` from the
        // producer's declared `VkImageLayout` carried in the
        // surface-share lookup response (#633). The host's
        // `acquire_render_target_dma_buf_image` registers fresh images
        // as UNDEFINED; subsequent acquires transition through
        // GENERAL / SHADER_READ_ONLY_OPTIMAL and the producer
        // re-declares its post-publish layout via
        // `surface_store::register_texture(..., layout)`. Reading
        // `gpu.current_image_layout` here keeps the consumer-side
        // adapter's first barrier source layout aligned with the
        // producer's claim. Move `texture` (not `texture.clone()` —
        // Clone is a hollow no-image stub) into the registration so
        // the adapter owns the imported VkImage's lifetime end-to-end.
        let registration = HostSurfaceRegistration::<ConsumerMarker> {
            texture: Arc::new(texture),
            timeline,
            initial_layout: VulkanLayout(gpu.current_image_layout),
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
            .insert(surface_id, RegisteredSurface);
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
        let handles = raw_handles(rt.device.as_ref());
        out.vk_instance = handles.vk_instance;
        out.vk_physical_device = handles.vk_physical_device;
        out.vk_device = handles.vk_device;
        out.vk_queue = handles.vk_queue;
        out.vk_queue_family_index = handles.vk_queue_family_index;
        out.api_version = handles.api_version;
        0
    }

    /// Fetch the per-image VkImageInfo descriptor for a registered
    /// surface. Returns 0 on success, -1 on null pointer or
    /// unregistered surface.
    ///
    /// Per-image (fixed at registration time). Polyglot wrappers that
    /// build a framework-native handle from the underlying VkImage
    /// (Skia's `GrBackendRenderTarget`, vulkano's `Image`, etc.)
    /// call this once per registration to populate their backend
    /// state; the per-acquire `vk_image_layout` still flows through
    /// [`SlpnVulkanView`] on every acquire.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_vulkan_get_image_info(
        rt: *mut VulkanRuntimeHandle,
        surface_id: u64,
        out: *mut SlpnVulkanImageInfo,
    ) -> i32 {
        let rt = match unsafe { rt.as_ref() } {
            Some(r) => r,
            None => return -1,
        };
        let out = match unsafe { out.as_mut() } {
            Some(o) => o,
            None => return -1,
        };
        let info = match rt.adapter.surface_image_info(surface_id) {
            Some(i) => i,
            None => {
                tracing::error!(
                    "slpn_vulkan_get_image_info: surface_id {} not registered",
                    surface_id
                );
                return -1;
            }
        };
        out.format = info.format;
        out.tiling = info.tiling;
        out.usage_flags = info.usage_flags;
        out.sample_count = info.sample_count;
        out.level_count = info.level_count;
        out.queue_family = info.queue_family;
        out.memory_handle = info.memory_handle;
        out.memory_offset = info.memory_offset;
        out.memory_size = info.memory_size;
        out.memory_property_flags = info.memory_property_flags;
        out.protected = info.protected;
        out.ycbcr_conversion = info.ycbcr_conversion;
        out._reserved = [0; 16];
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
}

// ============================================================================
// C ABI — cpu-readback adapter runtime (#562, Linux)
//
// Subprocess-side runtime for the cpu-readback adapter. Mirrors the
// vulkan / opengl runtimes one level above: the adapter is generic over
// device flavor, this cdylib instantiates it against `ConsumerVulkanDevice`
// from the carve-out, and per-acquire `vkCmdCopyImageToBuffer` runs
// host-side via a thin `run_cpu_readback_copy` escalate IPC. The
// subprocess waits on the imported timeline through the carve-out.
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
        HostSurfaceRegistration, VulkanLayout,
    };
    use streamlib_consumer_rhi::{
        ConsumerMarker, ConsumerVulkanDevice, ConsumerVulkanPixelBuffer,
        ConsumerVulkanTimelineSemaphore, PixelFormat,
    };

    use super::gpu_surface::SurfaceHandle;

    /// Maximum planes a cpu-readback view exposes. NV12 is the widest
    /// supported format today (2 planes); 4 leaves headroom for future
    /// formats without breaking the FFI struct layout.
    pub const SLPN_CPU_READBACK_MAX_PLANES: usize = 4;

    /// Direction wire constants matched by the Python / Deno SDKs.
    pub const SLPN_CPU_READBACK_DIRECTION_IMAGE_TO_BUFFER: u32 = 0;
    pub const SLPN_CPU_READBACK_DIRECTION_BUFFER_TO_IMAGE: u32 = 1;

    /// `acquire_*` return values.
    pub const SLPN_CPU_READBACK_OK: i32 = 0;
    pub const SLPN_CPU_READBACK_ERR: i32 = -1;
    pub const SLPN_CPU_READBACK_CONTENDED: i32 = 1;

    /// Per-plane geometry returned to the SDK on acquire. `mapped_ptr`
    /// aliases the imported staging `VkBuffer`'s host-visible mapping
    /// and is valid until the matching `_release_*` call.
    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct SlpnCpuReadbackPlane {
        pub mapped_ptr: *mut u8,
        pub width: u32,
        pub height: u32,
        pub bytes_per_pixel: u32,
        pub byte_size: u64,
    }

    /// View handed back on `_acquire_*`. `planes[0..plane_count]` is
    /// populated; entries beyond `plane_count` are zeroed.
    #[repr(C)]
    pub struct SlpnCpuReadbackView {
        pub width: u32,
        pub height: u32,
        /// Mirrors `streamlib_adapter_abi::SurfaceFormat as u32`
        /// (Bgra8 = 0, Rgba8 = 1, Nv12 = 2).
        pub format: u32,
        pub plane_count: u32,
        pub planes: [SlpnCpuReadbackPlane; SLPN_CPU_READBACK_MAX_PLANES],
    }

    /// Callback Python / Deno register at runtime construction time.
    /// The cdylib invokes it on every acquire (image_to_buffer) and
    /// every write release (buffer_to_image). The callback MUST send
    /// a `run_cpu_readback_copy` escalate IPC with `surface_id` and
    /// `direction`, block until the host responds, and return the
    /// host's `timeline_value`.
    ///
    /// Returns the `timeline_value` (a positive `u64`) on success.
    /// Returns `0` on failure — the host adapter starts every surface's
    /// timeline at 0 and only ever signals values >= 1, so 0 is an
    /// unused-by-construction sentinel. `user_data` is the opaque
    /// pointer the SDK passed at registration.
    pub type SlpnCpuReadbackTriggerCallback = unsafe extern "C" fn(
        user_data: *mut c_void,
        surface_id: u64,
        direction: u32,
    ) -> u64;

    /// Subprocess trigger that delegates `run_copy_*` to a callback the
    /// SDK registered. The callback wraps the SDK's escalate-IPC client.
    /// Construction is deferred — the runtime is created with no
    /// callback (every acquire fails), and the SDK installs one via
    /// `slpn_cpu_readback_set_trigger_callback` once its escalate
    /// channel is open.
    pub struct EscalateCpuReadbackCopyTrigger {
        callback: Mutex<Option<RegisteredCallback>>,
    }

    struct RegisteredCallback {
        callback: SlpnCpuReadbackTriggerCallback,
        /// Opaque pointer threaded back into the callback. The SDK is
        /// responsible for keeping the referent alive for the runtime's
        /// lifetime; the cdylib never dereferences it.
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
            callback: SlpnCpuReadbackTriggerCallback,
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
                     slpn_cpu_readback_set_trigger_callback before any acquire"
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
            self.dispatch(ctx.surface_id, SLPN_CPU_READBACK_DIRECTION_IMAGE_TO_BUFFER)
        }

        fn run_copy_buffer_to_image(
            &self,
            ctx: &CpuReadbackTriggerContext<'_, ConsumerMarker>,
        ) -> Result<u64, AdapterError> {
            self.dispatch(ctx.surface_id, SLPN_CPU_READBACK_DIRECTION_BUFFER_TO_IMAGE)
        }
    }

    pub struct CpuReadbackRuntimeHandle {
        device: Arc<ConsumerVulkanDevice>,
        adapter: Arc<CpuReadbackSurfaceAdapter<ConsumerVulkanDevice>>,
        trigger: Arc<EscalateCpuReadbackCopyTrigger>,
        /// Per-surface plane geometry snapshot. Populated at register
        /// time so `acquire_*` doesn't need to chase per-plane info
        /// through the adapter's view types.
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

    /// Bring up `ConsumerVulkanDevice` + `CpuReadbackSurfaceAdapter`
    /// against an empty trigger. The SDK MUST register a trigger
    /// callback via `slpn_cpu_readback_set_trigger_callback` before
    /// the first acquire; until then every acquire returns -1.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_cpu_readback_runtime_new() -> *mut CpuReadbackRuntimeHandle {
        super::init_subprocess_logging();
        let device = match ConsumerVulkanDevice::new() {
            Ok(d) => Arc::new(d),
            Err(e) => {
                tracing::error!(
                    "slpn_cpu_readback_runtime_new: ConsumerVulkanDevice::new failed: {}",
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
    pub unsafe extern "C" fn slpn_cpu_readback_runtime_free(rt: *mut CpuReadbackRuntimeHandle) {
        if !rt.is_null() {
            let _ = unsafe { Box::from_raw(rt) };
        }
    }

    /// Install the trigger callback. Replaces any prior callback. Pass
    /// a null callback pointer is undefined — callers must always pass
    /// a real C-callable. `user_data` is opaque and may be null; the
    /// cdylib threads it back through every call without dereferencing.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_cpu_readback_set_trigger_callback(
        rt: *mut CpuReadbackRuntimeHandle,
        callback: SlpnCpuReadbackTriggerCallback,
        user_data: *mut c_void,
    ) -> i32 {
        let rt = match unsafe { rt.as_ref() } {
            Some(r) => r,
            None => return SLPN_CPU_READBACK_ERR,
        };
        rt.trigger.install(callback, user_data);
        SLPN_CPU_READBACK_OK
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
            // NV12 plane 0 = Y (single channel), plane 1 = UV
            // (interleaved). The consumer-side import only uses the
            // PixelFormat for buffer metadata; the adapter's per-plane
            // geometry is what drives copies.
            (SurfaceFormat::Nv12, 0) => PixelFormat::Gray8,
            (SurfaceFormat::Nv12, 1) => PixelFormat::Gray8,
            _ => PixelFormat::Unknown,
        }
    }

    /// Register a host cpu-readback surface — imports each per-plane
    /// DMA-BUF FD as its own [`ConsumerVulkanPixelBuffer`], imports the
    /// host's exportable timeline via OPAQUE_FD, and hands the
    /// resulting [`HostSurfaceRegistration`] to the adapter.
    ///
    /// `surface_format` is the [`SurfaceFormat`] u32 wire token (0 =
    /// Bgra8, 1 = Rgba8, 2 = Nv12) — the SDK knows what the host
    /// allocated, so it passes it explicitly rather than parsing the
    /// surface-share format string.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_cpu_readback_register_surface(
        rt: *mut CpuReadbackRuntimeHandle,
        surface_id: u64,
        gpu_handle: *mut SurfaceHandle,
        surface_format: u32,
    ) -> i32 {
        let rt = match unsafe { rt.as_ref() } {
            Some(r) => r,
            None => {
                tracing::error!("slpn_cpu_readback_register_surface: null runtime");
                return SLPN_CPU_READBACK_ERR;
            }
        };
        let gpu = match unsafe { gpu_handle.as_mut() } {
            Some(g) => g,
            None => {
                tracing::error!("slpn_cpu_readback_register_surface: null gpu_handle");
                return SLPN_CPU_READBACK_ERR;
            }
        };
        let format = match surface_format_from_u32(surface_format) {
            Some(f) => f,
            None => {
                tracing::error!(
                    "slpn_cpu_readback_register_surface: unknown surface_format={}",
                    surface_format
                );
                return SLPN_CPU_READBACK_ERR;
            }
        };
        let plane_count = format.plane_count() as usize;
        if gpu.fds.len() != plane_count {
            tracing::error!(
                "slpn_cpu_readback_register_surface: format {:?} requires {} plane(s); gpu_handle has {} fd(s)",
                format,
                plane_count,
                gpu.fds.len()
            );
            return SLPN_CPU_READBACK_ERR;
        }
        let surface_width = gpu.width;
        let surface_height = gpu.height;

        // Import each plane as its own ConsumerVulkanPixelBuffer.
        // `from_dma_buf_fd` `dup`s the fd internally; the SurfaceHandle
        // keeps its originals so the SDK can re-import for a sibling
        // adapter (e.g. peeking the same surface via Vulkan).
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
                        "slpn_cpu_readback_register_surface: import plane {} fd={}: {}",
                        plane_idx,
                        gpu.fds[plane_idx],
                        e
                    );
                    return SLPN_CPU_READBACK_ERR;
                }
            };
            staging_planes.push(pb);
        }

        // Import the timeline semaphore. Required for every cpu-
        // readback surface — the host triggers signal it on every copy.
        let raw_sync_fd: RawFd = match gpu.sync_fd.take() {
            Some(fd) => fd,
            None => {
                tracing::error!(
                    "slpn_cpu_readback_register_surface: surface '{}' has no sync_fd — \
                     the host must register it via SurfaceStore::register_pixel_buffer_with_timeline \
                     with an exportable HostVulkanTimelineSemaphore.",
                    surface_id
                );
                return SLPN_CPU_READBACK_ERR;
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
                    "slpn_cpu_readback_register_surface: from_imported_opaque_fd: {}",
                    e
                );
                return SLPN_CPU_READBACK_ERR;
            }
        };

        // Snapshot per-plane geometry + mapped pointers BEFORE the
        // staging_planes Vec is moved into the registration.
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
            // Consumer-flavor cpu-readback surfaces don't import the
            // host's source VkImage — the host runs the copy on its
            // own VkDevice and signals the shared timeline; the
            // consumer only sees the staging buffers.
            texture: None,
            staging_planes,
            timeline,
            initial_image_layout: VulkanLayout::GENERAL,
            format,
            width: surface_width,
            height: surface_height,
        };

        if let Err(e) = rt.adapter.register_host_surface(surface_id, registration) {
            tracing::error!(
                "slpn_cpu_readback_register_surface: register_host_surface({}): {:?}",
                surface_id,
                e
            );
            return SLPN_CPU_READBACK_ERR;
        }

        rt.registered
            .lock()
            .expect("slpn_cpu_readback registered: poisoned")
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
        SLPN_CPU_READBACK_OK
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_cpu_readback_unregister_surface(
        rt: *mut CpuReadbackRuntimeHandle,
        surface_id: u64,
    ) -> i32 {
        let rt = match unsafe { rt.as_ref() } {
            Some(r) => r,
            None => return SLPN_CPU_READBACK_ERR,
        };
        let removed = rt
            .registered
            .lock()
            .expect("slpn_cpu_readback registered: poisoned")
            .remove(&surface_id);
        if removed.is_none() {
            return SLPN_CPU_READBACK_ERR;
        }
        if rt.adapter.unregister_host_surface(surface_id) {
            SLPN_CPU_READBACK_OK
        } else {
            SLPN_CPU_READBACK_ERR
        }
    }

    fn populate_view(
        rt: &CpuReadbackRuntimeHandle,
        surface_id: u64,
        out: &mut SlpnCpuReadbackView,
    ) -> i32 {
        let registered = rt
            .registered
            .lock()
            .expect("slpn_cpu_readback registered: poisoned");
        let entry = match registered.get(&surface_id) {
            Some(e) => e,
            None => {
                tracing::error!(
                    "slpn_cpu_readback acquire: surface_id {} not registered",
                    surface_id
                );
                return SLPN_CPU_READBACK_ERR;
            }
        };
        out.width = entry.width;
        out.height = entry.height;
        out.format = entry.format as u32;
        out.plane_count = entry.plane_count;
        out.planes = [SlpnCpuReadbackPlane {
            mapped_ptr: std::ptr::null_mut(),
            width: 0,
            height: 0,
            bytes_per_pixel: 0,
            byte_size: 0,
        }; SLPN_CPU_READBACK_MAX_PLANES];
        for idx in 0..(entry.plane_count as usize).min(SLPN_CPU_READBACK_MAX_PLANES) {
            out.planes[idx] = SlpnCpuReadbackPlane {
                mapped_ptr: entry.plane_mapped_ptrs[idx],
                width: entry.plane_widths[idx],
                height: entry.plane_heights[idx],
                bytes_per_pixel: entry.plane_bytes_per_pixel[idx],
                byte_size: entry.plane_byte_sizes[idx],
            };
        }
        SLPN_CPU_READBACK_OK
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
    pub unsafe extern "C" fn slpn_cpu_readback_acquire_read(
        rt: *mut CpuReadbackRuntimeHandle,
        surface_id: u64,
        out_view: *mut SlpnCpuReadbackView,
    ) -> i32 {
        let rt = match unsafe { rt.as_ref() } {
            Some(r) => r,
            None => return SLPN_CPU_READBACK_ERR,
        };
        let out = match unsafe { out_view.as_mut() } {
            Some(v) => v,
            None => return SLPN_CPU_READBACK_ERR,
        };
        let surface = make_descriptor(surface_id);
        match rt.adapter.acquire_read(&surface) {
            Ok(g) => {
                std::mem::forget(g);
                populate_view(rt, surface_id, out)
            }
            Err(e) => {
                tracing::error!(
                    "slpn_cpu_readback_acquire_read({}): {:?}",
                    surface_id,
                    e
                );
                SLPN_CPU_READBACK_ERR
            }
        }
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_cpu_readback_acquire_write(
        rt: *mut CpuReadbackRuntimeHandle,
        surface_id: u64,
        out_view: *mut SlpnCpuReadbackView,
    ) -> i32 {
        let rt = match unsafe { rt.as_ref() } {
            Some(r) => r,
            None => return SLPN_CPU_READBACK_ERR,
        };
        let out = match unsafe { out_view.as_mut() } {
            Some(v) => v,
            None => return SLPN_CPU_READBACK_ERR,
        };
        let surface = make_descriptor(surface_id);
        match rt.adapter.acquire_write(&surface) {
            Ok(g) => {
                std::mem::forget(g);
                populate_view(rt, surface_id, out)
            }
            Err(e) => {
                tracing::error!(
                    "slpn_cpu_readback_acquire_write({}): {:?}",
                    surface_id,
                    e
                );
                SLPN_CPU_READBACK_ERR
            }
        }
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_cpu_readback_try_acquire_read(
        rt: *mut CpuReadbackRuntimeHandle,
        surface_id: u64,
        out_view: *mut SlpnCpuReadbackView,
    ) -> i32 {
        let rt = match unsafe { rt.as_ref() } {
            Some(r) => r,
            None => return SLPN_CPU_READBACK_ERR,
        };
        let out = match unsafe { out_view.as_mut() } {
            Some(v) => v,
            None => return SLPN_CPU_READBACK_ERR,
        };
        let surface = make_descriptor(surface_id);
        match rt.adapter.try_acquire_read(&surface) {
            Ok(Some(g)) => {
                std::mem::forget(g);
                populate_view(rt, surface_id, out)
            }
            Ok(None) => SLPN_CPU_READBACK_CONTENDED,
            Err(e) => {
                tracing::error!(
                    "slpn_cpu_readback_try_acquire_read({}): {:?}",
                    surface_id,
                    e
                );
                SLPN_CPU_READBACK_ERR
            }
        }
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_cpu_readback_try_acquire_write(
        rt: *mut CpuReadbackRuntimeHandle,
        surface_id: u64,
        out_view: *mut SlpnCpuReadbackView,
    ) -> i32 {
        let rt = match unsafe { rt.as_ref() } {
            Some(r) => r,
            None => return SLPN_CPU_READBACK_ERR,
        };
        let out = match unsafe { out_view.as_mut() } {
            Some(v) => v,
            None => return SLPN_CPU_READBACK_ERR,
        };
        let surface = make_descriptor(surface_id);
        match rt.adapter.try_acquire_write(&surface) {
            Ok(Some(g)) => {
                std::mem::forget(g);
                populate_view(rt, surface_id, out)
            }
            Ok(None) => SLPN_CPU_READBACK_CONTENDED,
            Err(e) => {
                tracing::error!(
                    "slpn_cpu_readback_try_acquire_write({}): {:?}",
                    surface_id,
                    e
                );
                SLPN_CPU_READBACK_ERR
            }
        }
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_cpu_readback_release_read(
        rt: *mut CpuReadbackRuntimeHandle,
        surface_id: u64,
    ) -> i32 {
        let rt = match unsafe { rt.as_ref() } {
            Some(r) => r,
            None => return SLPN_CPU_READBACK_ERR,
        };
        rt.adapter.end_read_access(surface_id);
        SLPN_CPU_READBACK_OK
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_cpu_readback_release_write(
        rt: *mut CpuReadbackRuntimeHandle,
        surface_id: u64,
    ) -> i32 {
        let rt = match unsafe { rt.as_ref() } {
            Some(r) => r,
            None => return SLPN_CPU_READBACK_ERR,
        };
        rt.adapter.end_write_access(surface_id);
        SLPN_CPU_READBACK_OK
    }
}

#[cfg(not(target_os = "linux"))]
mod cpu_readback {
    use std::ffi::c_void;

    pub const SLPN_CPU_READBACK_MAX_PLANES: usize = 4;

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct SlpnCpuReadbackPlane {
        pub mapped_ptr: *mut u8,
        pub width: u32,
        pub height: u32,
        pub bytes_per_pixel: u32,
        pub byte_size: u64,
    }

    #[repr(C)]
    pub struct SlpnCpuReadbackView {
        pub width: u32,
        pub height: u32,
        pub format: u32,
        pub plane_count: u32,
        pub planes: [SlpnCpuReadbackPlane; SLPN_CPU_READBACK_MAX_PLANES],
    }

    pub type SlpnCpuReadbackTriggerCallback = unsafe extern "C" fn(
        user_data: *mut c_void,
        surface_id: u64,
        direction: u32,
    ) -> u64;

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_cpu_readback_runtime_new() -> *mut c_void {
        tracing::error!("slpn_cpu_readback_*: cpu-readback adapter runtime is Linux-only");
        std::ptr::null_mut()
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_cpu_readback_runtime_free(_rt: *mut c_void) {}

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_cpu_readback_set_trigger_callback(
        _rt: *mut c_void,
        _callback: SlpnCpuReadbackTriggerCallback,
        _user_data: *mut c_void,
    ) -> i32 {
        -1
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_cpu_readback_register_surface(
        _rt: *mut c_void,
        _surface_id: u64,
        _gpu_handle: *mut c_void,
        _surface_format: u32,
    ) -> i32 {
        -1
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_cpu_readback_unregister_surface(
        _rt: *mut c_void,
        _surface_id: u64,
    ) -> i32 {
        -1
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_cpu_readback_acquire_read(
        _rt: *mut c_void,
        _surface_id: u64,
        _out_view: *mut SlpnCpuReadbackView,
    ) -> i32 {
        -1
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_cpu_readback_acquire_write(
        _rt: *mut c_void,
        _surface_id: u64,
        _out_view: *mut SlpnCpuReadbackView,
    ) -> i32 {
        -1
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_cpu_readback_try_acquire_read(
        _rt: *mut c_void,
        _surface_id: u64,
        _out_view: *mut SlpnCpuReadbackView,
    ) -> i32 {
        -1
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_cpu_readback_try_acquire_write(
        _rt: *mut c_void,
        _surface_id: u64,
        _out_view: *mut SlpnCpuReadbackView,
    ) -> i32 {
        -1
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_cpu_readback_release_read(
        _rt: *mut c_void,
        _surface_id: u64,
    ) -> i32 {
        -1
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_cpu_readback_release_write(
        _rt: *mut c_void,
        _surface_id: u64,
    ) -> i32 {
        -1
    }
}

// ============================================================================
// C ABI — cuda adapter runtime (#589, Linux)
//
// Subprocess-side runtime for the cuda adapter. Same single-pattern shape
// as cpu-readback / vulkan / opengl: the adapter is generic over device
// flavor, this cdylib instantiates it against `ConsumerVulkanDevice`, and
// the OPAQUE_FD `VkBuffer` + timeline semaphore that surface-share hands
// over are imported into Vulkan via `streamlib-consumer-rhi` and into
// CUDA via `cudaImportExternalMemory` + `cudaImportExternalSemaphore`.
// Per-acquire flow has no IPC: the host pipeline writes into the
// OPAQUE_FD buffer and signals the shared timeline ambiently; the
// consumer's `acquire_read` Vulkan-waits via the adapter, then CUDA-waits
// via `cudaWaitExternalSemaphoresAsync` so CUDA driver state is in sync,
// then hands a DLPack capsule back to Python.
// ============================================================================

#[cfg(target_os = "linux")]
mod cuda {
    use std::collections::HashMap;
    use std::ffi::c_void;
    use std::mem::MaybeUninit;
    use std::os::unix::io::RawFd;
    use std::sync::{Arc, Mutex};

    use cudarc::runtime::result::external_memory;
    use cudarc::runtime::sys;
    use streamlib_adapter_abi::{
        StreamlibSurface, SurfaceAdapter as _, SurfaceFormat, SurfaceSyncState,
        SurfaceTransportHandle, SurfaceUsage,
    };
    use streamlib_adapter_cuda::dlpack::{
        self, CapsuleOwner, Device as DlpackDevice, DeviceType as DlpackDeviceType,
        ManagedTensor as DlpackManagedTensor,
    };
    use streamlib_adapter_cuda::{CudaSurfaceAdapter, HostSurfaceRegistration, VulkanLayout};
    use streamlib_consumer_rhi::{
        ConsumerVulkanDevice, ConsumerVulkanPixelBuffer, ConsumerVulkanTimelineSemaphore,
        PixelFormat,
    };

    use super::gpu_surface::SurfaceHandle;

    /// `acquire_*` return values — wire-stable across the cdylib boundary.
    pub const SLPN_CUDA_OK: i32 = 0;
    pub const SLPN_CUDA_ERR: i32 = -1;
    pub const SLPN_CUDA_CONTENDED: i32 = 1;

    /// DLPack `DLDeviceType` discriminants exposed at the FFI surface so
    /// the Python wrapper can interpret [`SlpnCudaView::device_type`]
    /// without re-importing the dlpark spec.
    pub const SLPN_CUDA_DEVICE_TYPE_CUDA: i32 = DlpackDeviceType::Cuda as i32;
    pub const SLPN_CUDA_DEVICE_TYPE_CUDA_HOST: i32 = DlpackDeviceType::CudaHost as i32;

    /// Per-acquire timeline-wait timeout (nanoseconds). Long enough to
    /// cover any realistic GPU queue depth; short enough that a deadlock
    /// surfaces as `SLPN_CUDA_ERR` rather than wedging the consumer.
    /// Mirrors `streamlib_adapter_cuda::CudaSurfaceAdapter`'s default
    /// (5 s).
    const ACQUIRE_TIMEOUT_NS: u64 = 5_000_000_000;

    /// View handed back on every successful acquire. Carries the cached
    /// CUDA device pointer (resolved once per surface at register time)
    /// plus a freshly heap-allocated DLPack `*mut DLManagedTensor` the
    /// caller takes ownership of — Python wraps it as a PyCapsule
    /// consumable by `torch.from_dlpack`. The capsule deleter drops an
    /// `Arc<RegisteredCudaSurface>` clone, so the imported memory stays
    /// alive until the consumer releases the capsule even if the adapter
    /// guard has been released first.
    ///
    /// Note: the underlying `vk::Buffer` handle is deliberately NOT
    /// exposed on this view. Consumers that need raw Vulkan access go
    /// through `streamlib-adapter-vulkan`'s `SlpnVulkanView` instead;
    /// switching to this adapter is the contractual signal for "I want
    /// a DLPack capsule."
    #[repr(C)]
    pub struct SlpnCudaView {
        /// Buffer size in bytes — same value as
        /// [`Self::dlpack_managed_tensor`]'s 1-D `u8` shape.
        pub size: u64,
        /// CUDA device pointer (`CUdeviceptr` cast to `u64`) returned by
        /// `cudaExternalMemoryGetMappedBuffer`. Stable for the surface's
        /// lifetime; cached at register time, not re-resolved on every
        /// acquire.
        pub device_ptr: u64,
        /// DLPack `DLDeviceType` discriminant. `2` (`kDLCUDA`) for true
        /// device memory; `3` (`kDLCUDAHost`) if `cudaPointerGetAttributes`
        /// classifies the imported pointer as pinned-host (a regression
        /// flagged by the carve-out test, currently driver-impossible on
        /// our test rig but checked anyway). Mirrors
        /// [`SLPN_CUDA_DEVICE_TYPE_CUDA`] / `_CUDA_HOST`.
        pub device_type: i32,
        /// CUDA device ordinal. Single-GPU rigs always see `0`; multi-GPU
        /// UUID matching is a follow-up.
        pub device_id: i32,
        /// `*mut DLManagedTensor` — heap-allocated, ownership transfers
        /// to the caller. The caller MUST eventually call the capsule's
        /// `deleter` (typically by handing the pointer to Python's
        /// `PyCapsule` constructor with a name `"dltensor"` per the
        /// DLPack v0.8 spec, and `deleter` set to the spec-mandated
        /// trampoline that calls `(*mt).deleter(mt)`).
        pub dlpack_managed_tensor: *mut c_void,
    }

    /// Subprocess CUDA runtime — single-process global per cdylib load.
    pub struct CudaRuntimeHandle {
        device: Arc<ConsumerVulkanDevice>,
        adapter: Arc<CudaSurfaceAdapter<ConsumerVulkanDevice>>,
        cuda_device_ordinal: i32,
        registered: Mutex<HashMap<u64, Arc<RegisteredCudaSurface>>>,
    }

    /// Per-surface registered state. The adapter's registry holds
    /// references to the same Vulkan-side `Arc`s; this struct adds the
    /// CUDA-side handles + the per-surface stream the cdylib uses for
    /// `cudaWaitExternalSemaphoresAsync`. Stored behind an `Arc` so a
    /// DLPack capsule's `manager_ctx` can clone it cheaply and keep
    /// every CUDA import alive until the consumer releases the capsule.
    struct RegisteredCudaSurface {
        // Vulkan-side handles — held for Drop ordering. The adapter's
        // registry holds its own Arc clones; ours ensure the CUDA
        // imports below are torn down BEFORE the underlying Vulkan
        // memory goes away (Vulkan teardown closes the FDs CUDA still
        // references). Drop order: this struct's fields drop in
        // declaration order, so put CUDA imports first.
        ext_mem: sys::cudaExternalMemory_t,
        ext_sem: sys::cudaExternalSemaphore_t,
        stream: sys::cudaStream_t,
        device_ptr: u64,
        size: u64,
        device_type: i32,
        cuda_device_ordinal: i32,
        // Vulkan-side imports follow — they outlive `ext_mem` / `ext_sem`
        // because Drop runs in declaration order, but logically the
        // adapter's registry already owns them. Holding our own clones
        // is belt-and-suspenders.
        #[allow(dead_code)] // Arc held for lifetime; never read after register.
        pixel_buffer: Arc<ConsumerVulkanPixelBuffer>,
        timeline: Arc<ConsumerVulkanTimelineSemaphore>,
    }

    // SAFETY: `cudaExternalMemory_t`, `cudaExternalSemaphore_t`, and
    // `cudaStream_t` are opaque pointer-shaped handles. The CUDA Runtime
    // API's threading contract permits use of these handles from any
    // thread once created; resource teardown (`cudaDestroyExternalMemory`,
    // `cudaDestroyExternalSemaphore`, `cudaStreamDestroy`) is similarly
    // thread-safe. Our `Mutex<HashMap>` serializes registration /
    // unregistration; per-acquire wait + sync calls are reentrant.
    unsafe impl Send for RegisteredCudaSurface {}
    unsafe impl Sync for RegisteredCudaSurface {}

    impl Drop for RegisteredCudaSurface {
        fn drop(&mut self) {
            // CUDA-side teardown FIRST so the imports release their
            // hold on the OPAQUE_FD kernel objects before Vulkan-side
            // Arcs drop and teardown the underlying VkDeviceMemory /
            // VkSemaphore.
            unsafe {
                if !self.stream.is_null() {
                    let _ = sys::cudaStreamDestroy(self.stream).result();
                }
                if !self.ext_sem.is_null() {
                    let _ = sys::cudaDestroyExternalSemaphore(self.ext_sem).result();
                }
                if !self.ext_mem.is_null() {
                    let _ = external_memory::destroy_external_memory(self.ext_mem);
                }
            }
        }
    }

    /// Bring up `ConsumerVulkanDevice` + `CudaSurfaceAdapter` against
    /// CUDA device 0. Returns NULL on Vulkan or CUDA bring-up failure;
    /// see the subprocess log for the underlying cause. Idempotent
    /// callers can rebuild after a failure once the cause is fixed.
    #[unsafe(no_mangle)]
    #[tracing::instrument(level = "info")]
    pub unsafe extern "C" fn slpn_cuda_runtime_new() -> *mut CudaRuntimeHandle {
        super::init_subprocess_logging();
        let device = match ConsumerVulkanDevice::new() {
            Ok(d) => Arc::new(d),
            Err(e) => {
                tracing::error!(
                    "slpn_cuda_runtime_new: ConsumerVulkanDevice::new failed: {}",
                    e
                );
                return std::ptr::null_mut();
            }
        };

        // CUDA runtime presence: dlopens libcudart + libcuda lazily on
        // the first CUDA call. `is_culib_present()` probes without
        // initializing — used here to surface the no-CUDA case as a
        // clean NULL return rather than a panic on the first import.
        if !unsafe { sys::is_culib_present() } {
            tracing::error!(
                "slpn_cuda_runtime_new: libcudart not present — CUDA toolkit \
                 absent on this machine; cuda adapter is unavailable"
            );
            return std::ptr::null_mut();
        }

        let cuda_device_ordinal: i32 = 0;
        if let Err(e) = unsafe { sys::cudaSetDevice(cuda_device_ordinal) }.result() {
            tracing::error!(
                "slpn_cuda_runtime_new: cudaSetDevice({}) failed: {:?}",
                cuda_device_ordinal,
                e
            );
            return std::ptr::null_mut();
        }

        let adapter = Arc::new(CudaSurfaceAdapter::new(Arc::clone(&device)));
        Box::into_raw(Box::new(CudaRuntimeHandle {
            device,
            adapter,
            cuda_device_ordinal,
            registered: Mutex::new(HashMap::new()),
        }))
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_cuda_runtime_free(rt: *mut CudaRuntimeHandle) {
        if !rt.is_null() {
            let _ = unsafe { Box::from_raw(rt) };
        }
    }

    /// Register a host cuda surface — imports the OPAQUE_FD `VkBuffer`
    /// and timeline semaphore via [`streamlib_consumer_rhi`], then
    /// re-imports the same FDs into CUDA via
    /// `cudaImportExternalMemory` + `cudaImportExternalSemaphore`.
    ///
    /// Caller (the SDK) supplies `surface_id` (subprocess-local u64;
    /// caller picks the namespace) and `gpu_handle` (a `SurfaceHandle*`
    /// returned by `slpn_surface_resolve_surface` after a `check_out` of
    /// the host's pre-registered cuda surface).
    ///
    /// Single-FD only: the host registers cuda surfaces as a
    /// flat OPAQUE_FD `VkBuffer` per `HostVulkanPixelBuffer::new_opaque_fd_export`;
    /// CUDA's `cudaExternalMemoryGetMappedBuffer` requires a flat memory
    /// region (multi-plane variants need
    /// `cudaExternalMemoryGetMappedMipmappedArray`, which doesn't have a
    /// DLPack flavor — see the issue body for the OPAQUE_FD vs DMA-BUF
    /// rationale).
    #[unsafe(no_mangle)]
    #[tracing::instrument(level = "info", skip(rt, gpu_handle))]
    pub unsafe extern "C" fn slpn_cuda_register_surface(
        rt: *mut CudaRuntimeHandle,
        surface_id: u64,
        gpu_handle: *mut SurfaceHandle,
    ) -> i32 {
        let rt = match unsafe { rt.as_ref() } {
            Some(r) => r,
            None => {
                tracing::error!("slpn_cuda_register_surface: null runtime");
                return SLPN_CUDA_ERR;
            }
        };
        let gpu = match unsafe { gpu_handle.as_mut() } {
            Some(g) => g,
            None => {
                tracing::error!("slpn_cuda_register_surface: null gpu_handle");
                return SLPN_CUDA_ERR;
            }
        };
        if gpu.fds.len() != 1 {
            tracing::error!(
                "slpn_cuda_register_surface: cuda requires exactly 1 OPAQUE_FD plane; \
                 gpu_handle has {} fd(s)",
                gpu.fds.len()
            );
            return SLPN_CUDA_ERR;
        }

        // ── Step 1: import the OPAQUE_FD `VkBuffer` into Vulkan ─────────
        // The fd is duplicated before the Vulkan import takes ownership
        // because we re-import the same fd into CUDA below. Vulkan and
        // CUDA each get their own dup; both close their dups on
        // teardown (Vulkan via `vkFreeMemory` → driver, CUDA via
        // `cudaDestroyExternalMemory` → driver).
        let vk_fd = unsafe { libc::dup(gpu.fds[0]) };
        if vk_fd < 0 {
            tracing::error!(
                "slpn_cuda_register_surface: dup vk_fd failed: {}",
                std::io::Error::last_os_error()
            );
            return SLPN_CUDA_ERR;
        }
        let buffer_size = gpu
            .plane_sizes
            .first()
            .copied()
            .filter(|s| *s > 0)
            .unwrap_or(gpu.size);
        if buffer_size == 0 {
            tracing::error!(
                "slpn_cuda_register_surface: surface '{}' has zero size",
                surface_id
            );
            unsafe { libc::close(vk_fd) };
            return SLPN_CUDA_ERR;
        }
        let pixel_buffer = match ConsumerVulkanPixelBuffer::from_opaque_fd(
            &rt.device,
            vk_fd,
            gpu.width,
            gpu.height,
            gpu.bytes_per_row.div_ceil(gpu.width.max(1)),
            PixelFormat::Bgra32,
            buffer_size,
        ) {
            Ok(b) => Arc::new(b),
            Err(e) => {
                tracing::error!(
                    "slpn_cuda_register_surface: from_opaque_fd failed: {} — \
                     verify the host registered this surface with handle_type=opaque_fd",
                    e
                );
                // Vulkan import takes the fd ownership only on success;
                // on error we still own the dup.
                unsafe { libc::close(vk_fd) };
                return SLPN_CUDA_ERR;
            }
        };

        // ── Step 2: import the timeline semaphore into Vulkan ───────────
        let raw_sync_fd: RawFd = match gpu.sync_fd.take() {
            Some(fd) => fd,
            None => {
                tracing::error!(
                    "slpn_cuda_register_surface: surface '{}' has no sync_fd — \
                     the host must register it with an exportable timeline semaphore \
                     so cross-API sync (Vulkan ↔ CUDA) is well-defined",
                    surface_id
                );
                return SLPN_CUDA_ERR;
            }
        };
        // Same dup story: timeline imports both into Vulkan and into CUDA.
        let vk_sync_fd = unsafe { libc::dup(raw_sync_fd) };
        if vk_sync_fd < 0 {
            tracing::error!(
                "slpn_cuda_register_surface: dup sync_fd failed: {}",
                std::io::Error::last_os_error()
            );
            // Restore the original fd onto the handle so its Drop closes it.
            gpu.sync_fd = Some(raw_sync_fd);
            return SLPN_CUDA_ERR;
        }
        let timeline = match ConsumerVulkanTimelineSemaphore::from_imported_opaque_fd(
            &rt.device,
            vk_sync_fd,
        ) {
            Ok(s) => Arc::new(s),
            Err(e) => {
                tracing::error!(
                    "slpn_cuda_register_surface: timeline from_imported_opaque_fd: {}",
                    e
                );
                unsafe { libc::close(vk_sync_fd) };
                gpu.sync_fd = Some(raw_sync_fd);
                return SLPN_CUDA_ERR;
            }
        };

        // ── Step 3: import the OPAQUE_FD memory into CUDA ───────────────
        // CUDA takes ownership of the cuda-side dup on successful import.
        let cuda_mem_fd = unsafe { libc::dup(gpu.fds[0]) };
        if cuda_mem_fd < 0 {
            tracing::error!(
                "slpn_cuda_register_surface: dup cuda_mem_fd failed: {}",
                std::io::Error::last_os_error()
            );
            // Vulkan-side imports drop here via Arc — they own their own
            // dups already. Restore sync_fd onto the handle so its Drop
            // closes it (Vulkan timeline import owns its dup independently).
            gpu.sync_fd = Some(raw_sync_fd);
            return SLPN_CUDA_ERR;
        }
        // SAFETY: `cudaImportExternalMemory` per CUDA docs:
        // - On UNIX, ownership of `cuda_mem_fd` transfers to the CUDA
        //   driver on successful import. We MUST NOT close it after.
        // - On error, ownership stays with us — close the fd here.
        let ext_mem = unsafe {
            match external_memory::import_external_memory_opaque_fd(cuda_mem_fd, buffer_size) {
                Ok(m) => m,
                Err(e) => {
                    tracing::error!(
                        "slpn_cuda_register_surface: cudaImportExternalMemory failed: {:?}",
                        e
                    );
                    libc::close(cuda_mem_fd);
                    gpu.sync_fd = Some(raw_sync_fd);
                    return SLPN_CUDA_ERR;
                }
            }
        };

        // SAFETY: `cudaExternalMemoryGetMappedBuffer` is the flat-pointer
        // mapping helper. The returned pointer aliases the same kernel
        // memory the OPAQUE_FD VkBuffer was bound to. Lifetime: valid
        // until `cudaDestroyExternalMemory` (handled by Drop).
        let dev_ptr = unsafe {
            match external_memory::get_mapped_buffer(ext_mem, 0, buffer_size) {
                Ok(p) => p as u64,
                Err(e) => {
                    tracing::error!(
                        "slpn_cuda_register_surface: cudaExternalMemoryGetMappedBuffer: {:?}",
                        e
                    );
                    let _ = external_memory::destroy_external_memory(ext_mem);
                    gpu.sync_fd = Some(raw_sync_fd);
                    return SLPN_CUDA_ERR;
                }
            }
        };

        // ── Step 4: classify the device pointer (kDLCUDA vs kDLCUDAHost) ─
        // The carve-out test (#588 Stage 8) flagged this as a load-bearing
        // probe — a future driver could downgrade the import to pinned-host
        // memory; the DLPack capsule must advertise the right device type
        // or `torch.from_dlpack` will copy unnecessarily (or refuse).
        let mut ptr_attrs = MaybeUninit::<sys::cudaPointerAttributes>::uninit();
        let ptr_attrs = unsafe {
            match sys::cudaPointerGetAttributes(ptr_attrs.as_mut_ptr(), dev_ptr as *const c_void)
                .result()
            {
                Ok(()) => ptr_attrs.assume_init(),
                Err(e) => {
                    tracing::error!(
                        "slpn_cuda_register_surface: cudaPointerGetAttributes failed: {:?}",
                        e
                    );
                    let _ = external_memory::destroy_external_memory(ext_mem);
                    gpu.sync_fd = Some(raw_sync_fd);
                    return SLPN_CUDA_ERR;
                }
            }
        };
        let device_type = match ptr_attrs.type_ {
            sys::cudaMemoryType::cudaMemoryTypeDevice => SLPN_CUDA_DEVICE_TYPE_CUDA,
            sys::cudaMemoryType::cudaMemoryTypeHost => SLPN_CUDA_DEVICE_TYPE_CUDA_HOST,
            other => {
                tracing::error!(
                    "slpn_cuda_register_surface: imported OPAQUE_FD device pointer is \
                     {:?} — neither cudaMemoryTypeDevice nor cudaMemoryTypeHost. \
                     DLPack consumers cannot accept this; investigate driver before proceeding",
                    other
                );
                let _ = unsafe { external_memory::destroy_external_memory(ext_mem) };
                gpu.sync_fd = Some(raw_sync_fd);
                return SLPN_CUDA_ERR;
            }
        };

        // ── Step 5: import the timeline semaphore into CUDA ─────────────
        let cuda_sync_fd = unsafe { libc::dup(raw_sync_fd) };
        if cuda_sync_fd < 0 {
            tracing::error!(
                "slpn_cuda_register_surface: dup cuda_sync_fd failed: {}",
                std::io::Error::last_os_error()
            );
            let _ = unsafe { external_memory::destroy_external_memory(ext_mem) };
            gpu.sync_fd = Some(raw_sync_fd);
            return SLPN_CUDA_ERR;
        }
        // Same descriptor-construction story as the carve-out test:
        // `cudaExternalSemaphoreHandleType`'s zero discriminant is invalid
        // under modern Rust validity rules, so use `MaybeUninit::zeroed()`
        // and write fields through raw pointers before `assume_init()`.
        // `reserved` (cuda-13xxx only) inherits the zero pre-fill, which
        // matches the spec contract.
        let mut sem_desc = MaybeUninit::<sys::cudaExternalSemaphoreHandleDesc>::zeroed();
        let sem_desc = unsafe {
            let p = sem_desc.as_mut_ptr();
            (&raw mut (*p).type_).write(
                sys::cudaExternalSemaphoreHandleType::cudaExternalSemaphoreHandleTypeTimelineSemaphoreFd,
            );
            (&raw mut (*p).handle).write(
                sys::cudaExternalSemaphoreHandleDesc__bindgen_ty_1 { fd: cuda_sync_fd },
            );
            (&raw mut (*p).flags).write(0);
            sem_desc.assume_init()
        };
        let mut ext_sem = MaybeUninit::<sys::cudaExternalSemaphore_t>::uninit();
        let ext_sem = unsafe {
            match sys::cudaImportExternalSemaphore(ext_sem.as_mut_ptr(), &sem_desc).result() {
                Ok(()) => ext_sem.assume_init(),
                Err(e) => {
                    tracing::error!(
                        "slpn_cuda_register_surface: cudaImportExternalSemaphore: {:?}",
                        e
                    );
                    libc::close(cuda_sync_fd);
                    let _ = external_memory::destroy_external_memory(ext_mem);
                    gpu.sync_fd = Some(raw_sync_fd);
                    return SLPN_CUDA_ERR;
                }
            }
        };

        // ── Step 6: per-surface CUDA stream for the per-acquire wait ────
        let mut stream = MaybeUninit::<sys::cudaStream_t>::uninit();
        let stream = unsafe {
            match sys::cudaStreamCreate(stream.as_mut_ptr()).result() {
                Ok(()) => stream.assume_init(),
                Err(e) => {
                    tracing::error!(
                        "slpn_cuda_register_surface: cudaStreamCreate: {:?}",
                        e
                    );
                    let _ = sys::cudaDestroyExternalSemaphore(ext_sem).result();
                    let _ = external_memory::destroy_external_memory(ext_mem);
                    gpu.sync_fd = Some(raw_sync_fd);
                    return SLPN_CUDA_ERR;
                }
            }
        };

        // ── Step 7: hand the imports to the adapter's registry ──────────
        // CUDA imports use `cudaImportExternalMemory(OPAQUE_FD)`, not
        // VkImage layout transitions; the producer-declared layout
        // from #633 is irrelevant to this adapter's acquire/release
        // path. Field is kept on the struct for shape parity with
        // `streamlib-adapter-vulkan` (see `HostSurfaceRegistration`
        // doc comment in `streamlib-adapter-cuda::state`); always
        // pass `UNDEFINED`.
        let registration = HostSurfaceRegistration {
            pixel_buffer: Arc::clone(&pixel_buffer),
            timeline: Arc::clone(&timeline),
            initial_layout: VulkanLayout::UNDEFINED,
        };
        if let Err(e) = rt.adapter.register_host_surface(surface_id, registration) {
            tracing::error!(
                "slpn_cuda_register_surface: adapter.register_host_surface({}): {:?}",
                surface_id,
                e
            );
            unsafe {
                let _ = sys::cudaStreamDestroy(stream).result();
                let _ = sys::cudaDestroyExternalSemaphore(ext_sem).result();
                let _ = external_memory::destroy_external_memory(ext_mem);
            };
            gpu.sync_fd = Some(raw_sync_fd);
            return SLPN_CUDA_ERR;
        }

        // The original `raw_sync_fd` is owned by the SurfaceHandle's drop
        // path. We've already dup'd it twice (Vulkan + CUDA timeline), so
        // returning it to the handle's `sync_fd` slot is the canonical
        // ownership recovery — same pattern as cpu-readback's
        // register_surface error paths.
        gpu.sync_fd = Some(raw_sync_fd);

        let entry = Arc::new(RegisteredCudaSurface {
            ext_mem,
            ext_sem,
            stream,
            device_ptr: dev_ptr,
            size: buffer_size,
            device_type,
            cuda_device_ordinal: rt.cuda_device_ordinal,
            pixel_buffer,
            timeline,
        });
        rt.registered
            .lock()
            .expect("slpn_cuda registered: poisoned")
            .insert(surface_id, entry);
        SLPN_CUDA_OK
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_cuda_unregister_surface(
        rt: *mut CudaRuntimeHandle,
        surface_id: u64,
    ) -> i32 {
        let rt = match unsafe { rt.as_ref() } {
            Some(r) => r,
            None => return SLPN_CUDA_ERR,
        };
        let removed = rt
            .registered
            .lock()
            .expect("slpn_cuda registered: poisoned")
            .remove(&surface_id);
        if removed.is_none() {
            return SLPN_CUDA_ERR;
        }
        if rt.adapter.unregister_host_surface(surface_id) {
            SLPN_CUDA_OK
        } else {
            SLPN_CUDA_ERR
        }
    }

    fn make_descriptor(surface_id: u64) -> StreamlibSurface {
        StreamlibSurface::new(
            surface_id,
            0,
            0,
            SurfaceFormat::Bgra8,
            SurfaceUsage::SAMPLED,
            SurfaceTransportHandle::empty(),
            SurfaceSyncState::default(),
        )
    }

    /// Build a fresh DLPack `*mut DLManagedTensor` over the cached
    /// device pointer; capsule owner is an `Arc<RegisteredCudaSurface>`
    /// clone so the imported memory stays alive across the FFI handoff.
    fn build_capsule(entry: &Arc<RegisteredCudaSurface>) -> *mut DlpackManagedTensor {
        // dlpark's `Device::cuda(usize)` helper covers `kDLCUDA` only;
        // construct manually so the `kDLCUDAHost` branch (regression
        // path flagged by the carve-out test) can ride the same code.
        let device = DlpackDevice {
            device_type: if entry.device_type == SLPN_CUDA_DEVICE_TYPE_CUDA_HOST {
                DlpackDeviceType::CudaHost
            } else {
                DlpackDeviceType::Cuda
            },
            device_id: entry.cuda_device_ordinal,
        };
        let owner: CapsuleOwner = Box::new(Arc::clone(entry));
        dlpack::build_byte_buffer_managed_tensor(entry.device_ptr, entry.size, device, owner)
    }

    /// Cross-API sync: after the adapter's Vulkan-side wait succeeds,
    /// CUDA's view of the same kernel timeline must also reach the
    /// signaled value before consumer kernels read the buffer. Issues a
    /// `cudaWaitExternalSemaphoresAsync` against the surface's stream at
    /// the timeline's current Vulkan-observed value, then synchronizes.
    /// Returns `Ok(())` on success.
    fn cuda_sync_after_acquire(entry: &RegisteredCudaSurface) -> Result<(), String> {
        let wait_value = entry
            .timeline
            .current_value()
            .map_err(|e| format!("get_semaphore_counter_value: {e}"))?;

        let mut wait_params = MaybeUninit::<sys::cudaExternalSemaphoreWaitParams>::zeroed();
        let wait_params = unsafe {
            let p = wait_params.as_mut_ptr();
            (&raw mut (*p).params.fence.value).write(wait_value);
            (&raw mut (*p).flags).write(0);
            wait_params.assume_init()
        };

        let wait_result = unsafe {
            sys::cudaWaitExternalSemaphoresAsync_v2(
                &entry.ext_sem,
                &wait_params,
                1,
                entry.stream,
            )
            .result()
        };
        if let Err(e) = wait_result {
            return Err(format!("cudaWaitExternalSemaphoresAsync: {e:?}"));
        }
        if let Err(e) = unsafe { sys::cudaStreamSynchronize(entry.stream) }.result() {
            return Err(format!("cudaStreamSynchronize: {e:?}"));
        }
        // Race note: the timeline can advance further between
        // `current_value()` (above) and `cudaWaitExternalSemaphoresAsync_v2`
        // returning. CUDA's wait-at-or-above semantics make a stale
        // `wait_value` still valid (we just wait less). Underflow isn't
        // possible: `current_value()` returns the present counter, never
        // zero on a post-acquire path because the host adapter only
        // signals values >= 1 on every release.
        Ok(())
    }

    fn populate_view(
        entry: &Arc<RegisteredCudaSurface>,
        out: &mut SlpnCudaView,
    ) -> i32 {
        let capsule = build_capsule(entry);
        if capsule.is_null() {
            tracing::error!("slpn_cuda: build_capsule returned null");
            return SLPN_CUDA_ERR;
        }
        out.size = entry.size;
        out.device_ptr = entry.device_ptr;
        out.device_type = entry.device_type;
        out.device_id = entry.cuda_device_ordinal;
        out.dlpack_managed_tensor = capsule as *mut c_void;
        SLPN_CUDA_OK
    }

    fn lookup_entry(
        rt: &CudaRuntimeHandle,
        surface_id: u64,
    ) -> Option<Arc<RegisteredCudaSurface>> {
        rt.registered
            .lock()
            .expect("slpn_cuda registered: poisoned")
            .get(&surface_id)
            .cloned()
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_cuda_acquire_read(
        rt: *mut CudaRuntimeHandle,
        surface_id: u64,
        out_view: *mut SlpnCudaView,
    ) -> i32 {
        let rt = match unsafe { rt.as_ref() } {
            Some(r) => r,
            None => return SLPN_CUDA_ERR,
        };
        let out = match unsafe { out_view.as_mut() } {
            Some(v) => v,
            None => return SLPN_CUDA_ERR,
        };
        let entry = match lookup_entry(rt, surface_id) {
            Some(e) => e,
            None => {
                tracing::error!(
                    "slpn_cuda_acquire_read: surface_id {} not registered",
                    surface_id
                );
                return SLPN_CUDA_ERR;
            }
        };
        let surface = make_descriptor(surface_id);
        let _ = ACQUIRE_TIMEOUT_NS; // wired through the adapter's default — fixed for v1
        match rt.adapter.acquire_read(&surface) {
            Ok(g) => {
                std::mem::forget(g);
                if let Err(e) = cuda_sync_after_acquire(&entry) {
                    tracing::error!(
                        "slpn_cuda_acquire_read({}): cuda sync failed: {} — \
                         releasing the adapter guard so the timeline can \
                         advance",
                        surface_id,
                        e
                    );
                    rt.adapter.end_read_access(surface_id);
                    return SLPN_CUDA_ERR;
                }
                populate_view(&entry, out)
            }
            Err(e) => {
                tracing::error!("slpn_cuda_acquire_read({}): {:?}", surface_id, e);
                SLPN_CUDA_ERR
            }
        }
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_cuda_acquire_write(
        rt: *mut CudaRuntimeHandle,
        surface_id: u64,
        out_view: *mut SlpnCudaView,
    ) -> i32 {
        let rt = match unsafe { rt.as_ref() } {
            Some(r) => r,
            None => return SLPN_CUDA_ERR,
        };
        let out = match unsafe { out_view.as_mut() } {
            Some(v) => v,
            None => return SLPN_CUDA_ERR,
        };
        let entry = match lookup_entry(rt, surface_id) {
            Some(e) => e,
            None => {
                tracing::error!(
                    "slpn_cuda_acquire_write: surface_id {} not registered",
                    surface_id
                );
                return SLPN_CUDA_ERR;
            }
        };
        let surface = make_descriptor(surface_id);
        match rt.adapter.acquire_write(&surface) {
            Ok(g) => {
                std::mem::forget(g);
                if let Err(e) = cuda_sync_after_acquire(&entry) {
                    tracing::error!(
                        "slpn_cuda_acquire_write({}): cuda sync failed: {}",
                        surface_id,
                        e
                    );
                    rt.adapter.end_write_access(surface_id);
                    return SLPN_CUDA_ERR;
                }
                populate_view(&entry, out)
            }
            Err(e) => {
                tracing::error!("slpn_cuda_acquire_write({}): {:?}", surface_id, e);
                SLPN_CUDA_ERR
            }
        }
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_cuda_try_acquire_read(
        rt: *mut CudaRuntimeHandle,
        surface_id: u64,
        out_view: *mut SlpnCudaView,
    ) -> i32 {
        let rt = match unsafe { rt.as_ref() } {
            Some(r) => r,
            None => return SLPN_CUDA_ERR,
        };
        let out = match unsafe { out_view.as_mut() } {
            Some(v) => v,
            None => return SLPN_CUDA_ERR,
        };
        let entry = match lookup_entry(rt, surface_id) {
            Some(e) => e,
            None => return SLPN_CUDA_ERR,
        };
        let surface = make_descriptor(surface_id);
        match rt.adapter.try_acquire_read(&surface) {
            Ok(Some(g)) => {
                std::mem::forget(g);
                if let Err(e) = cuda_sync_after_acquire(&entry) {
                    tracing::error!(
                        "slpn_cuda_try_acquire_read({}): cuda sync: {}",
                        surface_id,
                        e
                    );
                    rt.adapter.end_read_access(surface_id);
                    return SLPN_CUDA_ERR;
                }
                populate_view(&entry, out)
            }
            Ok(None) => SLPN_CUDA_CONTENDED,
            Err(e) => {
                tracing::error!("slpn_cuda_try_acquire_read({}): {:?}", surface_id, e);
                SLPN_CUDA_ERR
            }
        }
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_cuda_try_acquire_write(
        rt: *mut CudaRuntimeHandle,
        surface_id: u64,
        out_view: *mut SlpnCudaView,
    ) -> i32 {
        let rt = match unsafe { rt.as_ref() } {
            Some(r) => r,
            None => return SLPN_CUDA_ERR,
        };
        let out = match unsafe { out_view.as_mut() } {
            Some(v) => v,
            None => return SLPN_CUDA_ERR,
        };
        let entry = match lookup_entry(rt, surface_id) {
            Some(e) => e,
            None => return SLPN_CUDA_ERR,
        };
        let surface = make_descriptor(surface_id);
        match rt.adapter.try_acquire_write(&surface) {
            Ok(Some(g)) => {
                std::mem::forget(g);
                if let Err(e) = cuda_sync_after_acquire(&entry) {
                    tracing::error!(
                        "slpn_cuda_try_acquire_write({}): cuda sync: {}",
                        surface_id,
                        e
                    );
                    rt.adapter.end_write_access(surface_id);
                    return SLPN_CUDA_ERR;
                }
                populate_view(&entry, out)
            }
            Ok(None) => SLPN_CUDA_CONTENDED,
            Err(e) => {
                tracing::error!("slpn_cuda_try_acquire_write({}): {:?}", surface_id, e);
                SLPN_CUDA_ERR
            }
        }
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_cuda_release_read(
        rt: *mut CudaRuntimeHandle,
        surface_id: u64,
    ) -> i32 {
        let rt = match unsafe { rt.as_ref() } {
            Some(r) => r,
            None => return SLPN_CUDA_ERR,
        };
        rt.adapter.end_read_access(surface_id);
        SLPN_CUDA_OK
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_cuda_release_write(
        rt: *mut CudaRuntimeHandle,
        surface_id: u64,
    ) -> i32 {
        let rt = match unsafe { rt.as_ref() } {
            Some(r) => r,
            None => return SLPN_CUDA_ERR,
        };
        rt.adapter.end_write_access(surface_id);
        SLPN_CUDA_OK
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::mem::{offset_of, size_of};

        // Layout regression — pins the FFI struct shape across the
        // cdylib boundary. Python's ctypes / Deno's DataView readers
        // depend on these offsets matching exactly.
        #[test]
        fn slpn_cuda_view_layout_matches_spec_64bit() {
            // SlpnCudaView fields in declaration order:
            //   size                 : u64    @ 0
            //   device_ptr           : u64    @ 8
            //   device_type          : i32    @ 16
            //   device_id            : i32    @ 20
            //   dlpack_managed_tensor: ptr    @ 24 (8 bytes on 64-bit)
            assert_eq!(size_of::<SlpnCudaView>(), 32);
            assert_eq!(offset_of!(SlpnCudaView, size), 0);
            assert_eq!(offset_of!(SlpnCudaView, device_ptr), 8);
            assert_eq!(offset_of!(SlpnCudaView, device_type), 16);
            assert_eq!(offset_of!(SlpnCudaView, device_id), 20);
            assert_eq!(offset_of!(SlpnCudaView, dlpack_managed_tensor), 24);
        }

        #[test]
        fn slpn_cuda_constants_match_dlpack_spec() {
            // DLPack v0.8 — these discriminants are wire ABI; a future
            // dlpark renumbering would land here as a compile failure.
            assert_eq!(SLPN_CUDA_DEVICE_TYPE_CUDA, 2);
            assert_eq!(SLPN_CUDA_DEVICE_TYPE_CUDA_HOST, 3);
            assert_eq!(SLPN_CUDA_OK, 0);
            assert_eq!(SLPN_CUDA_ERR, -1);
            assert_eq!(SLPN_CUDA_CONTENDED, 1);
        }

        // Runtime-construction smoke test: bring up + tear down the
        // cuda runtime without panicking. The runtime returns NULL
        // when Vulkan or CUDA is absent (the typical CI env), and
        // succeeds when both are present (a developer's box). Both
        // outcomes must be panic-free; this asserts neither path
        // unwinds across the FFI boundary.
        //
        // Not gated on CUDA presence — the no-CUDA branch is the
        // important one to exercise everywhere because that's what
        // most CI runners hit. On a CUDA-equipped runner the runtime
        // is constructed and immediately freed; that exercises the
        // happy path's allocation + cleanup symmetry.
        #[test]
        fn runtime_new_is_panic_safe_without_cuda() {
            let rt = unsafe { slpn_cuda_runtime_new() };
            if !rt.is_null() {
                unsafe { slpn_cuda_runtime_free(rt) };
            }
        }
    }
}

#[cfg(not(target_os = "linux"))]
mod cuda {
    use std::ffi::c_void;

    pub const SLPN_CUDA_OK: i32 = 0;
    pub const SLPN_CUDA_ERR: i32 = -1;
    pub const SLPN_CUDA_CONTENDED: i32 = 1;
    pub const SLPN_CUDA_DEVICE_TYPE_CUDA: i32 = 2;
    pub const SLPN_CUDA_DEVICE_TYPE_CUDA_HOST: i32 = 3;

    #[repr(C)]
    pub struct SlpnCudaView {
        pub size: u64,
        pub device_ptr: u64,
        pub device_type: i32,
        pub device_id: i32,
        pub dlpack_managed_tensor: *mut c_void,
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_cuda_runtime_new() -> *mut c_void {
        tracing::error!("slpn_cuda_*: cuda adapter runtime is Linux-only");
        std::ptr::null_mut()
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_cuda_runtime_free(_rt: *mut c_void) {}

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_cuda_register_surface(
        _rt: *mut c_void,
        _surface_id: u64,
        _gpu_handle: *mut c_void,
    ) -> i32 {
        SLPN_CUDA_ERR
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_cuda_unregister_surface(
        _rt: *mut c_void,
        _surface_id: u64,
    ) -> i32 {
        SLPN_CUDA_ERR
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_cuda_acquire_read(
        _rt: *mut c_void,
        _surface_id: u64,
        _out_view: *mut SlpnCudaView,
    ) -> i32 {
        SLPN_CUDA_ERR
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_cuda_acquire_write(
        _rt: *mut c_void,
        _surface_id: u64,
        _out_view: *mut SlpnCudaView,
    ) -> i32 {
        SLPN_CUDA_ERR
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_cuda_try_acquire_read(
        _rt: *mut c_void,
        _surface_id: u64,
        _out_view: *mut SlpnCudaView,
    ) -> i32 {
        SLPN_CUDA_ERR
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_cuda_try_acquire_write(
        _rt: *mut c_void,
        _surface_id: u64,
        _out_view: *mut SlpnCudaView,
    ) -> i32 {
        SLPN_CUDA_ERR
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_cuda_release_read(
        _rt: *mut c_void,
        _surface_id: u64,
    ) -> i32 {
        SLPN_CUDA_ERR
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn slpn_cuda_release_write(
        _rt: *mut c_void,
        _surface_id: u64,
    ) -> i32 {
        SLPN_CUDA_ERR
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

    #[repr(C)]
    pub struct SlpnVulkanImageInfo {
        pub format: i32,
        pub tiling: i32,
        pub usage_flags: u32,
        pub sample_count: u32,
        pub level_count: u32,
        pub queue_family: u32,
        pub memory_handle: u64,
        pub memory_offset: u64,
        pub memory_size: u64,
        pub memory_property_flags: u32,
        pub protected: u32,
        pub ycbcr_conversion: u64,
        pub _reserved: [u8; 16],
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
    pub unsafe extern "C" fn slpn_vulkan_get_image_info(
        _rt: *mut c_void,
        _surface_id: u64,
        _out: *mut SlpnVulkanImageInfo,
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

