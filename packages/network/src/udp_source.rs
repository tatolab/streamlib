// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! UDP source processor — binds a socket, spawns a tokio recv loop on
//! the engine's shared runtime, and emits each inbound datagram as a
//! `NetworkPacket` on its output port.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use streamlib::sdk::context::RuntimeContextFullAccess;
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::iceoryx2::OutputWriter;
use streamlib::sdk::processors::ManualProcessor;
use tokio::net::UdpSocket;

use crate::_generated_::NetworkPacket;

/// Worst-case UDP datagram payload (RFC 768 / RFC 791): 65,535 − 8 (UDP
/// header) − 20 (IPv4 header) = 65,507. Round up to 65,536 for a clean
/// alignment; the extra bytes are unused.
const MAX_DATAGRAM_BYTES: usize = 65_536;

#[streamlib::sdk::processor("UdpSource")]
pub struct UdpSourceProcessor {
    tokio_handle: Option<tokio::runtime::Handle>,
    is_running: Arc<AtomicBool>,
    packets_received: Arc<AtomicU64>,
    recv_task_handle: Option<tokio::task::JoinHandle<()>>,
}

impl ManualProcessor for UdpSourceProcessor::Processor {
    fn setup(
        &mut self,
        ctx: &RuntimeContextFullAccess<'_>,
    ) -> impl std::future::Future<Output = Result<()>> + Send {
        self.tokio_handle = Some(ctx.tokio_handle().clone());
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

        self.is_running.store(true, Ordering::Release);
        let is_running = Arc::clone(&self.is_running);
        let packets_received = Arc::clone(&self.packets_received);
        let outputs: Arc<OutputWriter> = self.outputs.clone();

        let handle = tokio_handle.spawn(async move {
            recv_loop(socket, is_running, packets_received, outputs).await;
        });
        self.recv_task_handle = Some(handle);

        tracing::info!(bind_addr = %bind_addr, "UdpSource: bound + recv loop started");
        Ok(())
    }

    fn stop(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        self.is_running.store(false, Ordering::Release);
        let n = self.packets_received.load(Ordering::Relaxed);
        if let Some(handle) = self.recv_task_handle.take() {
            handle.abort();
        }
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
fn build_udp_socket(
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
    is_running: Arc<AtomicBool>,
    packets_received: Arc<AtomicU64>,
    outputs: Arc<OutputWriter>,
) {
    // Monotonic clock anchor — `Instant::elapsed` is the right monotonic
    // primitive (per the `monotonic_clocks_for_timing` rule); we publish
    // absolute nanos since processor start so consumers can compare
    // across packets without needing a shared epoch.
    let clock_start = Instant::now();
    let mut buf = vec![0u8; MAX_DATAGRAM_BYTES];

    loop {
        if !is_running.load(Ordering::Acquire) {
            break;
        }

        let (n, peer) = match socket.recv_from(&mut buf).await {
            Ok(pair) => pair,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
            Err(e) => {
                tracing::error!(error = %e, "UdpSource: recv_from failed");
                continue;
            }
        };

        let timestamp_ns = clock_start.elapsed().as_nanos() as i64;
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

    tracing::debug!(
        packets_received = packets_received.load(Ordering::Relaxed),
        "UdpSource: recv loop exiting",
    );
}
