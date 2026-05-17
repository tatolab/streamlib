// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! UDP source processor — binds a socket, spawns a tokio recv loop on
//! the engine's shared runtime, and emits each inbound datagram as a
//! `NetworkPacket` on its output port.

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
    tokio_handle: Option<tokio::runtime::Handle>,
    shutdown: Arc<Notify>,
    packets_received: Arc<AtomicU64>,
    recv_task_handle: Option<tokio::task::JoinHandle<()>>,
}

impl ManualProcessor for UdpSourceProcessor::Processor {
    fn setup(
        &mut self,
        ctx: &RuntimeContextFullAccess<'_>,
    ) -> impl std::future::Future<Output = Result<()>> + Send {
        self.tokio_handle = Some(ctx.tokio_handle().clone());
        // Touch the lazy anchor so the first packet's timestamp is
        // genuinely relative to setup-time, not to first-recv-time.
        let _ = monotonic_ns();
        tracing::info!(
            bind_addr = %self.config.bind_addr,
            recv_buffer_bytes = ?self.config.recv_buffer_bytes,
            "UdpSource: setup",
        );
        std::future::ready(Ok(()))
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

        let handle = tokio_handle.spawn(async move {
            recv_loop(socket, shutdown, packets_received, outputs).await;
        });
        self.recv_task_handle = Some(handle);

        tracing::info!(bind_addr = %bind_addr, "UdpSource: bound + recv loop started");
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
}

/// Build a tokio `UdpSocket`, applying pre-bind sockopts (SO_RCVBUF when
/// requested) via socket2 then converting to a tokio handle. Bind runs
/// synchronously — `tokio::net::UdpSocket::bind` is async, but the
/// underlying `socket2::Socket::bind` is sync and equivalent for UDP.
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
    if let Some(bytes) = recv_buffer_bytes {
        // The kernel may clamp to net.core.rmem_max; that's an OK silent
        // truncation — we report what we asked for, the kernel logs the
        // ceiling separately.
        socket.set_recv_buffer_size(bytes as usize).map_err(|e| {
            Error::Configuration(format!("UdpSource: SO_RCVBUF={bytes} failed: {e}"))
        })?;
    }
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
) {
    let mut buf = vec![0u8; MAX_DATAGRAM_BYTES];

    loop {
        tokio::select! {
            biased;
            // Shutdown branch first — `biased` makes the select check
            // it before polling the long-running recv, so a notify
            // delivered while we were already mid-recv still gets
            // observed on the next loop iteration without latency.
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

                let timestamp_ns = monotonic_ns();
                let packet = NetworkPacket {
                    payload: buf[..n].to_vec(),
                    peer_addr: peer.to_string(),
                    timestamp_ns: timestamp_ns.to_string(),
                };

                if let Err(e) = outputs.write("packets", &packet) {
                    tracing::error!(error = %e, peer = %peer, "UdpSource: output write failed");
                    continue;
                }

                let count = packets_received.fetch_add(1, Ordering::Relaxed) + 1;
                if count == 1 {
                    tracing::info!(peer = %peer, bytes = n, "UdpSource: first packet received");
                }
            }
        }
    }

    tracing::debug!(
        packets_received = packets_received.load(Ordering::Relaxed),
        "UdpSource: recv loop exiting",
    );
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
}
