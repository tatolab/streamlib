// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! UDP source processor — binds a socket, spawns a tokio recv loop on
//! the engine's shared runtime, and emits each inbound datagram as a
//! `NetworkPacket` on its output port.
//!
//! On Linux the recv loop drains up to `batch_size` datagrams per
//! wakeup via `recvmmsg(2)` (one syscall per batch). Off Linux the
//! per-datagram `recv_from` path runs unchanged, capped by the same
//! `batch_size` so the loop never starves the shutdown branch.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::Instant;

use streamlib::sdk::context::RuntimeContextFullAccess;
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::iceoryx2::OutputWriter;
use streamlib::sdk::processors::ManualProcessor;
use tokio::net::UdpSocket;
use tokio::sync::Notify;

use crate::_generated_::NetworkPacket;

/// Worst-case UDP datagram payload (RFC 768 / RFC 791): 65,535 − 8 (UDP
/// header) − 20 (IPv4 header) = 65,507. Round up to 65,536 for a clean
/// alignment; the extra bytes are unused.
const MAX_DATAGRAM_BYTES: usize = 65_536;

/// Default `recvmmsg` batch / fallback drain cap. Matches Quinn's UDP
/// recv shape — large enough to amortize syscall cost on bursty
/// sources, small enough that a full drain doesn't latch a long stall
/// into the loop before re-checking shutdown.
const DEFAULT_BATCH_SIZE: u32 = 64;

/// Default SO_RCVBUF hint when the consumer doesn't override. The
/// kernel typically clamps to `net.core.rmem_max` (~208 KiB on stock
/// Linux, often 8–16 MiB on tuned hosts); asking for more than the
/// ceiling is silently truncated.
const DEFAULT_RECV_BUFFER_BYTES: u32 = 4 * 1024 * 1024;

/// Process-wide monotonic anchor for `NetworkPacket.timestamp_ns`.
/// Initialized on first use; all `UdpSource` instances in the same
/// process resolve against this anchor so consumers can compare
/// timestamps across sources (e.g. correlating a MAVLink heartbeat
/// recv on one socket with a vision-stream chunk recv on another).
static MONOTONIC_EPOCH: LazyLock<Instant> = LazyLock::new(Instant::now);

fn monotonic_ns() -> i64 {
    MONOTONIC_EPOCH.elapsed().as_nanos() as i64
}

#[streamlib::sdk::processor("UdpSource")]
pub struct UdpSourceProcessor {
    /// Plugin-owned tokio runtime. Constructed in `setup()`; the host's
    /// runtime is not reachable across the plugin ABI per #885.
    /// `tokio::net::UdpSocket` requires this runtime's TLS to be set
    /// while polling its futures.
    tokio_runtime: Option<tokio::runtime::Runtime>,
    tokio_handle: Option<tokio::runtime::Handle>,
    shutdown: Arc<Notify>,
    packets_received: Arc<AtomicU64>,
    recv_task_handle: Option<tokio::task::JoinHandle<()>>,
}

impl ManualProcessor for UdpSourceProcessor::Processor {
    fn setup(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .map_err(|e| {
                Error::Configuration(format!(
                    "UdpSource: failed to build tokio runtime: {e}"
                ))
            })?;
        self.tokio_handle = Some(runtime.handle().clone());
        self.tokio_runtime = Some(runtime);
        // Touch the lazy anchor so the first packet's timestamp is
        // genuinely relative to setup-time, not to first-recv-time.
        let _ = monotonic_ns();
        tracing::info!(
            bind_addr = %self.config.bind_addr,
            recv_buffer_bytes = ?self.config.recv_buffer_bytes,
            batch_size = ?self.config.batch_size,
            "UdpSource: setup",
        );
        Ok(())
    }

    fn start(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        let tokio_handle = self
            .tokio_handle
            .clone()
            .ok_or_else(|| Error::Configuration("tokio handle not stashed in setup()".into()))?;

        let bind_addr: SocketAddr = self.config.bind_addr.parse().map_err(|e| {
            Error::Configuration(format!(
                "UdpSource: invalid bind_addr {:?}: {e}",
                self.config.bind_addr
            ))
        })?;

        let socket = build_udp_socket(bind_addr, self.config.recv_buffer_bytes, &tokio_handle)?;

        let shutdown = Arc::clone(&self.shutdown);
        let packets_received = Arc::clone(&self.packets_received);
        let outputs: Arc<OutputWriter> = self.outputs.clone();
        let batch_size = resolve_batch_size(self.config.batch_size);

        let handle = tokio_handle.spawn(async move {
            recv_loop(socket, shutdown, packets_received, outputs, batch_size).await;
        });
        self.recv_task_handle = Some(handle);

        tracing::info!(
            bind_addr = %bind_addr,
            batch_size = batch_size,
            "UdpSource: bound + recv loop started",
        );
        Ok(())
    }

    fn stop(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        // The actual shutdown contract is `handle.abort()` — the
        // tokio runtime guarantees the spawned task is cancelled.
        // `notify_waiters` is a best-effort graceful signal so a loop
        // mid-await-on-recv can break the `select!` cleanly before
        // the abort lands; if the task hasn't yet registered the
        // `notified()` future (race: stop fires between spawn and
        // first poll), notify_waiters is a no-op but abort still
        // does the job.
        self.shutdown.notify_waiters();
        if let Some(handle) = self.recv_task_handle.take() {
            handle.abort();
        }
        let n = self.packets_received.load(Ordering::Relaxed);
        tracing::info!(packets_received = n, "UdpSource: stopped");
        Ok(())
    }

    fn teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        // Drop the plugin-owned tokio runtime — joins worker threads
        // and finalizes any aborted task cleanup.
        self.tokio_handle.take();
        self.tokio_runtime.take();
        Ok(())
    }
}

/// Resolve the configured batch size against the default. Clamped at
/// 1 so a misconfigured 0 doesn't deadlock the loop, and at
/// `u16::MAX` to keep the per-batch buffer allocation below ~4 GiB
/// even at 64 KiB per slot (real callers stay well below this).
fn resolve_batch_size(configured: Option<u32>) -> usize {
    configured
        .unwrap_or(DEFAULT_BATCH_SIZE)
        .clamp(1, u16::MAX as u32) as usize
}

/// Build a tokio `UdpSocket`, applying pre-bind sockopts (SO_RCVBUF when
/// requested, defaulting to `DEFAULT_RECV_BUFFER_BYTES` otherwise) via
/// socket2 then converting to a tokio handle. Bind runs synchronously —
/// `tokio::net::UdpSocket::bind` is async, but the underlying
/// `socket2::Socket::bind` is sync and equivalent for UDP.
/// `UdpSocket::from_std` requires being inside a tokio runtime context;
/// the `handle.enter()` guard provides it for the duration of the call.
pub(crate) fn build_udp_socket(
    addr: SocketAddr,
    recv_buffer_bytes: Option<u32>,
    tokio_handle: &tokio::runtime::Handle,
) -> Result<UdpSocket> {
    let domain = match addr {
        SocketAddr::V4(_) => socket2::Domain::IPV4,
        SocketAddr::V6(_) => socket2::Domain::IPV6,
    };
    let socket = socket2::Socket::new(domain, socket2::Type::DGRAM, Some(socket2::Protocol::UDP))
        .map_err(|e| Error::Configuration(format!("UdpSource: socket() failed: {e}")))?;
    socket.set_reuse_address(true).map_err(|e| {
        Error::Configuration(format!("UdpSource: SO_REUSEADDR failed: {e}"))
    })?;
    socket.set_nonblocking(true).map_err(|e| {
        Error::Configuration(format!("UdpSource: set_nonblocking failed: {e}"))
    })?;
    let requested_bytes = recv_buffer_bytes.unwrap_or(DEFAULT_RECV_BUFFER_BYTES);
    // The kernel may clamp to net.core.rmem_max; that's an OK silent
    // truncation — we report what we asked for, the kernel logs the
    // ceiling separately.
    socket
        .set_recv_buffer_size(requested_bytes as usize)
        .map_err(|e| {
            Error::Configuration(format!("UdpSource: SO_RCVBUF={requested_bytes} failed: {e}"))
        })?;
    socket
        .bind(&addr.into())
        .map_err(|e| Error::Configuration(format!("UdpSource: bind {addr} failed: {e}")))?;

    let std_socket: std::net::UdpSocket = socket.into();
    let _guard = tokio_handle.enter();
    UdpSocket::from_std(std_socket)
        .map_err(|e| Error::Configuration(format!("UdpSource: from_std failed: {e}")))
}

async fn recv_loop(
    socket: UdpSocket,
    shutdown: Arc<Notify>,
    packets_received: Arc<AtomicU64>,
    outputs: Arc<OutputWriter>,
    batch_size: usize,
) {
    #[cfg(target_os = "linux")]
    {
        recv_loop_linux(socket, shutdown, packets_received, outputs, batch_size).await;
    }
    #[cfg(not(target_os = "linux"))]
    {
        recv_loop_fallback(socket, shutdown, packets_received, outputs, batch_size).await;
    }
}

#[cfg(target_os = "linux")]
async fn recv_loop_linux(
    socket: UdpSocket,
    shutdown: Arc<Notify>,
    packets_received: Arc<AtomicU64>,
    outputs: Arc<OutputWriter>,
    batch_size: usize,
) {
    use std::os::fd::AsRawFd;
    use tokio::io::Interest;

    let mut batch = recvmmsg_linux::RecvBatch::new(batch_size);
    let fd = socket.as_raw_fd();

    loop {
        tokio::select! {
            biased;
            // Shutdown branch first — `biased` makes the select check
            // it before polling the long-running recv, so a notify
            // delivered while we were already mid-recv still gets
            // observed on the next loop iteration without latency.
            _ = shutdown.notified() => break,

            // SAFETY: drain captures `&mut batch` which lives only as
            // long as the async_io future (dropped at end of arm).
            drained = socket.async_io(Interest::READABLE, || batch.drain(fd)) => {
                let n = match drained {
                    Ok(n) => n,
                    Err(e) => {
                        tracing::error!(error = %e, "UdpSource: recvmmsg failed");
                        continue;
                    }
                };
                let timestamp_ns = monotonic_ns();
                let timestamp_str = timestamp_ns.to_string();
                for i in 0..n {
                    let Some((payload, peer)) = batch.slot(i) else {
                        // Malformed peer addr from kernel (shouldn't
                        // happen for AF_INET/AF_INET6 dgrams); skip.
                        continue;
                    };
                    let packet = NetworkPacket {
                        payload: payload.to_vec(),
                        peer_addr: peer.to_string(),
                        timestamp_ns: timestamp_str.clone(),
                    };
                    if let Err(e) = outputs.write("packets", &packet) {
                        tracing::error!(error = %e, peer = %peer, "UdpSource: output write failed");
                        continue;
                    }
                    let count = packets_received.fetch_add(1, Ordering::Relaxed) + 1;
                    if count == 1 {
                        tracing::info!(peer = %peer, bytes = payload.len(), "UdpSource: first packet received");
                    }
                }
            }
        }
    }

    tracing::debug!(
        packets_received = packets_received.load(Ordering::Relaxed),
        "UdpSource: recv loop exiting",
    );
}

#[cfg(not(target_os = "linux"))]
async fn recv_loop_fallback(
    socket: UdpSocket,
    shutdown: Arc<Notify>,
    packets_received: Arc<AtomicU64>,
    outputs: Arc<OutputWriter>,
    batch_size: usize,
) {
    let mut buf = vec![0u8; MAX_DATAGRAM_BYTES];

    loop {
        tokio::select! {
            biased;
            _ = shutdown.notified() => break,

            recv = socket.recv_from(&mut buf) => {
                let (n, peer) = match recv {
                    Ok(pair) => pair,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
                    Err(e) => {
                        tracing::error!(error = %e, "UdpSource: recv_from failed");
                        continue;
                    }
                };
                publish_one(&buf[..n], peer, &packets_received, &outputs);

                // Opportunistically drain up to batch_size-1 more
                // datagrams that may already be sitting in the
                // socket buffer; per-packet recv_from cost on these
                // platforms is the baseline, batching here is a
                // syscall-amortization courtesy.
                for _ in 1..batch_size {
                    match socket.try_recv_from(&mut buf) {
                        Ok((n, peer)) => publish_one(&buf[..n], peer, &packets_received, &outputs),
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                        Err(e) => {
                            tracing::error!(error = %e, "UdpSource: try_recv_from failed");
                            break;
                        }
                    }
                }
            }
        }
    }

    tracing::debug!(
        packets_received = packets_received.load(Ordering::Relaxed),
        "UdpSource: recv loop exiting",
    );
}

#[cfg(not(target_os = "linux"))]
fn publish_one(
    payload: &[u8],
    peer: SocketAddr,
    packets_received: &Arc<AtomicU64>,
    outputs: &Arc<OutputWriter>,
) {
    let packet = NetworkPacket {
        payload: payload.to_vec(),
        peer_addr: peer.to_string(),
        timestamp_ns: monotonic_ns().to_string(),
    };
    if let Err(e) = outputs.write("packets", &packet) {
        tracing::error!(error = %e, peer = %peer, "UdpSource: output write failed");
        return;
    }
    let count = packets_received.fetch_add(1, Ordering::Relaxed) + 1;
    if count == 1 {
        tracing::info!(peer = %peer, bytes = payload.len(), "UdpSource: first packet received");
    }
}

#[cfg(target_os = "linux")]
mod recvmmsg_linux {
    //! Linux-specific batched recv via `recvmmsg(2)`. Pre-allocates a
    //! fixed array of (buffer, sockaddr_storage, iovec, mmsghdr) slots
    //! at construction and reuses them per syscall — zero allocation
    //! on the hot path.

    use std::io;
    use std::net::SocketAddr;
    use std::os::fd::RawFd;

    use super::MAX_DATAGRAM_BYTES;

    /// Per-slot storage: one datagram buffer, one peer-addr storage,
    /// one iovec pointing into the buffer. Boxed so the address is
    /// stable independent of the enclosing Vec's reallocation.
    struct Slot {
        buffer: Box<[u8; MAX_DATAGRAM_BYTES]>,
        peer_storage: libc::sockaddr_storage,
        iovec: libc::iovec,
    }

    pub(super) struct RecvBatch {
        slots: Box<[Slot]>,
        /// Parallel array of `mmsghdr`s whose `msg_hdr` fields point
        /// into the matching `slots[i]`. Stable as long as `slots` is
        /// not resized; we never resize it.
        mmsghdrs: Box<[libc::mmsghdr]>,
    }

    // SAFETY: `mmsghdr` contains raw `*mut` pointers that point into
    // the same `RecvBatch`'s `slots` (heap-allocated; addresses
    // stable across moves of `RecvBatch` itself). The pointers never
    // escape the `RecvBatch`, so no other thread can observe them
    // unless the whole batch moves with them. The recv task that
    // owns the batch is the only reader and writer.
    unsafe impl Send for RecvBatch {}

    impl RecvBatch {
        pub(super) fn new(batch_size: usize) -> Self {
            let mut slots: Vec<Slot> = (0..batch_size)
                .map(|_| Slot {
                    buffer: Box::new([0u8; MAX_DATAGRAM_BYTES]),
                    peer_storage: unsafe { std::mem::zeroed() },
                    iovec: libc::iovec {
                        iov_base: std::ptr::null_mut(),
                        iov_len: 0,
                    },
                })
                .collect();

            // Wire iovec.iov_base into each buffer. Do this BEFORE
            // building mmsghdrs so the iovec pointers are stable.
            for slot in slots.iter_mut() {
                slot.iovec.iov_base = slot.buffer.as_mut_ptr() as *mut libc::c_void;
                slot.iovec.iov_len = MAX_DATAGRAM_BYTES;
            }
            let slots = slots.into_boxed_slice();

            let mut mmsghdrs: Vec<libc::mmsghdr> = Vec::with_capacity(batch_size);
            for slot in slots.iter() {
                let hdr = libc::msghdr {
                    msg_name: &slot.peer_storage as *const _ as *mut libc::c_void,
                    msg_namelen: std::mem::size_of::<libc::sockaddr_storage>() as libc::socklen_t,
                    msg_iov: &slot.iovec as *const _ as *mut libc::iovec,
                    msg_iovlen: 1,
                    msg_control: std::ptr::null_mut(),
                    msg_controllen: 0,
                    msg_flags: 0,
                };
                mmsghdrs.push(libc::mmsghdr {
                    msg_hdr: hdr,
                    msg_len: 0,
                });
            }
            let mmsghdrs = mmsghdrs.into_boxed_slice();

            Self { slots, mmsghdrs }
        }

        /// Drain up to `batch_size` datagrams via one `recvmmsg`
        /// syscall. Returns the count on success or `WouldBlock` when
        /// no datagrams are ready — `async_io` treats `WouldBlock` as
        /// "re-await readiness," which matches kernel semantics for a
        /// spurious wake.
        pub(super) fn drain(&mut self, fd: RawFd) -> io::Result<usize> {
            // Reset per-call mutable fields. msg_namelen is rewritten
            // by the kernel to the actual peer-addr length, and
            // msg_len reports the datagram length per slot.
            let slot_count = self.slots.len();
            for i in 0..slot_count {
                self.mmsghdrs[i].msg_hdr.msg_namelen =
                    std::mem::size_of::<libc::sockaddr_storage>() as libc::socklen_t;
                self.mmsghdrs[i].msg_len = 0;
            }

            // SAFETY: `self.mmsghdrs` is a boxed slice whose backing
            // memory is owned by &mut self. Each mmsghdr.msg_hdr
            // points into the matching `self.slots[i]` (peer_storage,
            // iovec), also owned by &mut self for the duration of the
            // call. `recvmmsg` writes into `msg_len`, into
            // `peer_storage` (length capped by msg_namelen), and into
            // the iovec target buffers. MSG_DONTWAIT ensures the
            // syscall returns -EAGAIN instead of blocking when no
            // data is ready.
            let n = unsafe {
                libc::recvmmsg(
                    fd,
                    self.mmsghdrs.as_mut_ptr(),
                    slot_count as libc::c_uint,
                    libc::MSG_DONTWAIT,
                    std::ptr::null_mut(),
                )
            };
            if n < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(n as usize)
        }

        /// Return `(payload, peer)` for slot `idx` after a successful
        /// `drain(n)`. Caller must respect `idx < n`; indexing beyond
        /// that yields stale data from a prior call.
        pub(super) fn slot(&self, idx: usize) -> Option<(&[u8], SocketAddr)> {
            let mh = self.mmsghdrs.get(idx)?;
            let len = (mh.msg_len as usize).min(MAX_DATAGRAM_BYTES);
            let slot = &self.slots[idx];
            let payload = &slot.buffer[..len];
            let peer = sockaddr_storage_to_socket(&slot.peer_storage, mh.msg_hdr.msg_namelen)?;
            Some((payload, peer))
        }
    }

    fn sockaddr_storage_to_socket(
        storage: &libc::sockaddr_storage,
        len: libc::socklen_t,
    ) -> Option<SocketAddr> {
        // SAFETY: `socket2::SockAddr::new` requires `storage` to be
        // initialized as a valid sockaddr_storage value of the
        // indicated length. The kernel writes the peer address into
        // `slot.peer_storage` and reports the actual length in
        // `msg_namelen` for AF_INET / AF_INET6 sockets, which is what
        // a UDP socket bound to an IP address produces. socket2 then
        // narrows to `Option<SocketAddr>`, returning `None` for any
        // address family it can't classify (AF_UNIX, AF_NETLINK,
        // etc.) — which never appears here.
        let sock_addr = unsafe { socket2::SockAddr::new(*storage, len) };
        sock_addr.as_socket()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Locks the bind → drop → rebind path through `build_udp_socket`.
    /// Linux UDP has no TIME_WAIT (TCP-only), so the rebind succeeds
    /// regardless of `SO_REUSEADDR` — this test does NOT verify the
    /// REUSEADDR knob (that would require overlapping wildcard /
    /// specific binds, out of scope for the v1 surface). What it
    /// does lock is that the function itself doesn't leak a kernel-
    /// side socket descriptor: a refactor that forgot to close the
    /// fd would make the rebind fail with EADDRINUSE.
    #[tokio::test]
    async fn build_socket_binds_drops_and_rebinds_cleanly() {
        let handle = tokio::runtime::Handle::current();
        let ephemeral: SocketAddr = "127.0.0.1:0".parse().unwrap();

        let socket_a = build_udp_socket(ephemeral, None, &handle).expect("bind 1");
        let bound_addr = socket_a.local_addr().expect("local_addr");
        assert_eq!(bound_addr.ip().to_string(), "127.0.0.1");
        assert_ne!(bound_addr.port(), 0, "kernel assigned a real port");
        drop(socket_a);

        let socket_b = build_udp_socket(bound_addr, None, &handle).expect("rebind");
        assert_eq!(socket_b.local_addr().unwrap(), bound_addr);
    }

    /// Bind error: try to bind to a non-local IPv4 address. The kernel
    /// returns EADDRNOTAVAIL, which surfaces as `Error::Configuration`
    /// — NOT a panic. Locks in the "bind failure surfaces a clean
    /// typed error" exit criterion from #829.
    #[tokio::test]
    async fn build_socket_bind_failure_returns_typed_error() {
        let handle = tokio::runtime::Handle::current();
        // 192.0.2.0/24 is RFC 5737 TEST-NET-1 — guaranteed not to be a
        // local interface address anywhere. bind() against it returns
        // EADDRNOTAVAIL on every Linux kernel.
        let unreachable: SocketAddr = "192.0.2.1:9999".parse().unwrap();

        match build_udp_socket(unreachable, None, &handle) {
            Err(Error::Configuration(msg)) => {
                assert!(
                    msg.contains("bind"),
                    "bind failure message should mention bind, got: {msg}"
                );
            }
            Err(other) => panic!("expected Error::Configuration, got {other:?}"),
            Ok(_) => panic!("expected bind failure on non-local 192.0.2.1"),
        }
    }

    /// Smoke test that passing a `recv_buffer_bytes` hint does not
    /// break the build path. The kernel may silently clamp the value
    /// to `net.core.rmem_max`, and `socket2` does NOT surface a clamp
    /// as an error — so this only locks "the optional code path
    /// doesn't error", not "the hint is honored by the kernel".
    /// Verifying the actual SO_RCVBUF value would require reading it
    /// back via `socket2` getsockopt; that's overkill for v1.
    #[tokio::test]
    async fn build_socket_accepts_recv_buffer_hint() {
        let handle = tokio::runtime::Handle::current();
        let ephemeral: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let socket = build_udp_socket(ephemeral, Some(1 << 20), &handle)
            .expect("bind with 1 MiB SO_RCVBUF hint");
        assert!(socket.local_addr().is_ok());
    }

    /// `resolve_batch_size` clamps a zero override to 1 so a misconfig
    /// can't deadlock the recv loop (a 0-element `recvmmsg` returns 0
    /// without consuming readiness, which would spin). `None` resolves
    /// to the documented default.
    #[test]
    fn batch_size_defaults_and_clamp_invariants() {
        assert_eq!(resolve_batch_size(None), DEFAULT_BATCH_SIZE as usize);
        assert_eq!(resolve_batch_size(Some(0)), 1);
        assert_eq!(resolve_batch_size(Some(32)), 32);
        assert_eq!(resolve_batch_size(Some(64)), 64);
        // Above the clamp ceiling, capped instead of overflowed.
        assert_eq!(resolve_batch_size(Some(1_000_000)), u16::MAX as usize);
    }

    /// Linux-only: send N datagrams into a paired socket, drive one
    /// `recvmmsg` syscall, and assert all N come back in one drain
    /// with the correct peer address and payload bytes. This is the
    /// per-syscall amortization invariant — the whole point of the
    /// batching change. A regression that fell back to per-datagram
    /// recv would still pass this (it would just take N syscalls);
    /// what locks the batching behavior specifically is the count
    /// returned by `drain()` (≥ 2 means more than one msg landed in
    /// one syscall).
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn recvmmsg_drains_multiple_datagrams_in_one_syscall() {
        use std::os::fd::AsRawFd;

        let handle = tokio::runtime::Handle::current();
        let bind: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let recv_socket = build_udp_socket(bind, None, &handle).expect("bind recv");
        let recv_addr = recv_socket.local_addr().expect("local_addr");

        let send_socket = std::net::UdpSocket::bind("127.0.0.1:0").expect("bind sender");
        let send_addr = send_socket.local_addr().expect("send local_addr");

        // Burst N datagrams synchronously — the kernel queues them in
        // the recv socket's buffer; the subsequent recvmmsg picks all
        // up in one syscall.
        const N: usize = 8;
        for i in 0..N {
            let payload = [i as u8; 16];
            send_socket
                .send_to(&payload, recv_addr)
                .expect("send datagram");
        }

        // Yield to let the kernel deliver the datagrams. On loopback
        // delivery is immediate, but the recv readiness needs to
        // propagate to tokio's mio reactor.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let mut batch = recvmmsg_linux::RecvBatch::new(N);
        let fd = recv_socket.as_raw_fd();

        // Drive readiness through tokio so the socket is registered
        // with the reactor; otherwise `recvmmsg` could race the
        // kernel's queue settling.
        recv_socket
            .readable()
            .await
            .expect("readable after burst");

        let n = batch.drain(fd).expect("recvmmsg drains");
        assert!(
            n >= 2,
            "expected ≥2 datagrams in one syscall (batching invariant), got {n}",
        );

        for i in 0..n {
            let (payload, peer) = batch.slot(i).expect("slot decoded");
            assert_eq!(peer, send_addr, "peer addr decoded from sockaddr_storage");
            assert_eq!(payload.len(), 16, "16-byte datagram");
            // Bytes inside a slot are the bytes the kernel wrote for
            // that datagram; recvmmsg orders slots in arrival order,
            // and loopback preserves send order, so slot[i] carries
            // the i-th datagram's payload.
            assert!(
                payload.iter().all(|&b| b == i as u8),
                "slot {i} payload bytes mismatch",
            );
        }
    }
}
