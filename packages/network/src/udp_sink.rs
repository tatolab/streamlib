// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! UDP sink processor — consumes `NetworkPacket` and writes the payload
//! to a UDP socket. `process()` is non-blocking: it pushes work onto a
//! bounded mpsc that a background tokio task drains via `send_to`. On
//! mpsc overflow the packet is dropped and a counter increments — back-
//! pressure into the pipeline is deliberately avoided so a slow network
//! can't stall upstream processors.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use streamlib::sdk::context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::processors::ReactiveProcessor;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;

use crate::_generated_::NetworkPacket;

const OUTBOUND_MPSC_CAPACITY: usize = 256;

struct OutboundDatagram {
    peer: SocketAddr,
    payload: Vec<u8>,
}

#[streamlib::sdk::processor("UdpSink")]
pub struct UdpSinkProcessor {
    tokio_handle: Option<tokio::runtime::Handle>,
    /// Resolved at setup time so we don't re-parse on every send.
    default_destination: Option<SocketAddr>,
    /// Outbound queue; producer (`process()`) is non-blocking via
    /// `try_send`; consumer is the background send task.
    outbound_tx: Option<mpsc::Sender<OutboundDatagram>>,
    send_task_handle: Option<tokio::task::JoinHandle<()>>,
    packets_sent: Arc<AtomicU64>,
    packets_dropped_queue_full: Arc<AtomicU64>,
    packets_dropped_no_destination: Arc<AtomicU64>,
}

impl ReactiveProcessor for UdpSinkProcessor::Processor {
    fn setup(
        &mut self,
        ctx: &RuntimeContextFullAccess<'_>,
    ) -> impl std::future::Future<Output = Result<()>> + Send {
        let tokio_handle = ctx.tokio_handle().clone();
        self.tokio_handle = Some(tokio_handle.clone());

        let bind_str = self
            .config
            .bind_addr
            .clone()
            .unwrap_or_else(|| "0.0.0.0:0".to_string());

        // Parse default_destination eagerly so a misconfig surfaces at
        // setup rather than on first packet.
        let default_destination_result = self
            .config
            .default_destination
            .as_deref()
            .map(|s| {
                s.parse::<SocketAddr>().map_err(|e| {
                    Error::Configuration(format!(
                        "UdpSink: invalid default_destination {s:?}: {e}"
                    ))
                })
            })
            .transpose();

        let send_buffer_bytes = self.config.send_buffer_bytes;
        let packets_sent = Arc::clone(&self.packets_sent);
        let outbound_tx_slot = &mut self.outbound_tx;
        let send_task_slot = &mut self.send_task_handle;
        let default_destination_slot = &mut self.default_destination;

        std::future::ready((|| -> Result<()> {
            let default_destination = default_destination_result?;
            *default_destination_slot = default_destination;

            let bind_addr: SocketAddr = bind_str.parse().map_err(|e| {
                Error::Configuration(format!("UdpSink: invalid bind_addr {bind_str:?}: {e}"))
            })?;

            let socket = build_udp_socket(bind_addr, send_buffer_bytes, &tokio_handle)?;

            let (tx, rx) = mpsc::channel::<OutboundDatagram>(OUTBOUND_MPSC_CAPACITY);
            *outbound_tx_slot = Some(tx);

            let handle = tokio_handle.spawn(async move {
                send_loop(socket, rx, packets_sent).await;
            });
            *send_task_slot = Some(handle);

            tracing::info!(
                bind_addr = %bind_addr,
                default_destination = ?default_destination_slot.as_ref().map(|d| d.to_string()),
                send_buffer_bytes = ?send_buffer_bytes,
                "UdpSink: bound + send task spawned",
            );
            Ok(())
        })())
    }

    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        if !self.inputs.has_data("packets") {
            return Ok(());
        }
        let packet: NetworkPacket = self.inputs.read("packets")?;

        let peer = resolve_destination(&packet.peer_addr, self.default_destination);
        let Some(peer) = peer else {
            let n = self
                .packets_dropped_no_destination
                .fetch_add(1, Ordering::Relaxed)
                + 1;
            if n == 1 || n.is_power_of_two() {
                tracing::warn!(
                    dropped_total = n,
                    "UdpSink: dropping packet — empty peer_addr and no default_destination",
                );
            }
            return Ok(());
        };

        let Some(tx) = self.outbound_tx.as_ref() else {
            return Err(Error::Configuration(
                "UdpSink: send task not initialized — setup() did not run".into(),
            ));
        };

        match tx.try_send(OutboundDatagram {
            peer,
            payload: packet.payload,
        }) {
            Ok(()) => Ok(()),
            Err(mpsc::error::TrySendError::Full(_)) => {
                let n = self
                    .packets_dropped_queue_full
                    .fetch_add(1, Ordering::Relaxed)
                    + 1;
                if n == 1 || n.is_power_of_two() {
                    tracing::warn!(
                        dropped_total = n,
                        capacity = OUTBOUND_MPSC_CAPACITY,
                        "UdpSink: outbound queue full — dropping packet",
                    );
                }
                Ok(())
            }
            Err(mpsc::error::TrySendError::Closed(_)) => Err(Error::Runtime(
                "UdpSink: outbound channel closed — send task died".into(),
            )),
        }
    }

    fn teardown(
        &mut self,
        _ctx: &RuntimeContextFullAccess<'_>,
    ) -> impl std::future::Future<Output = Result<()>> + Send {
        // Drop the sender so the send loop sees `Closed` and exits.
        self.outbound_tx.take();
        if let Some(handle) = self.send_task_handle.take() {
            handle.abort();
        }
        let sent = self.packets_sent.load(Ordering::Relaxed);
        let dropped_full = self.packets_dropped_queue_full.load(Ordering::Relaxed);
        let dropped_nodst = self.packets_dropped_no_destination.load(Ordering::Relaxed);
        tracing::info!(
            packets_sent = sent,
            packets_dropped_queue_full = dropped_full,
            packets_dropped_no_destination = dropped_nodst,
            "UdpSink: teardown",
        );
        std::future::ready(Ok(()))
    }
}

fn resolve_destination(packet_peer: &str, default: Option<SocketAddr>) -> Option<SocketAddr> {
    if packet_peer.is_empty() {
        return default;
    }
    match packet_peer.parse::<SocketAddr>() {
        Ok(addr) => Some(addr),
        Err(_) => {
            // Malformed addresses arrive from network input; we don't want
            // a bad upstream to kill the sink, so log+fall-back to default
            // (which may also be None, in which case the caller drops).
            tracing::warn!(peer_addr = packet_peer, "UdpSink: malformed peer_addr — falling back to default_destination");
            default
        }
    }
}

fn build_udp_socket(
    addr: SocketAddr,
    send_buffer_bytes: Option<u32>,
    tokio_handle: &tokio::runtime::Handle,
) -> Result<UdpSocket> {
    let domain = match addr {
        SocketAddr::V4(_) => socket2::Domain::IPV4,
        SocketAddr::V6(_) => socket2::Domain::IPV6,
    };
    let socket = socket2::Socket::new(domain, socket2::Type::DGRAM, Some(socket2::Protocol::UDP))
        .map_err(|e| Error::Configuration(format!("UdpSink: socket() failed: {e}")))?;
    socket
        .set_reuse_address(true)
        .map_err(|e| Error::Configuration(format!("UdpSink: SO_REUSEADDR failed: {e}")))?;
    socket
        .set_nonblocking(true)
        .map_err(|e| Error::Configuration(format!("UdpSink: set_nonblocking failed: {e}")))?;
    if let Some(bytes) = send_buffer_bytes {
        socket.set_send_buffer_size(bytes as usize).map_err(|e| {
            Error::Configuration(format!("UdpSink: SO_SNDBUF={bytes} failed: {e}"))
        })?;
    }
    socket
        .bind(&addr.into())
        .map_err(|e| Error::Configuration(format!("UdpSink: bind {addr} failed: {e}")))?;
    let std_socket: std::net::UdpSocket = socket.into();
    let _guard = tokio_handle.enter();
    UdpSocket::from_std(std_socket)
        .map_err(|e| Error::Configuration(format!("UdpSink: from_std failed: {e}")))
}

async fn send_loop(
    socket: UdpSocket,
    mut rx: mpsc::Receiver<OutboundDatagram>,
    packets_sent: Arc<AtomicU64>,
) {
    while let Some(datagram) = rx.recv().await {
        match socket.send_to(&datagram.payload, datagram.peer).await {
            Ok(_) => {
                let n = packets_sent.fetch_add(1, Ordering::Relaxed) + 1;
                if n == 1 {
                    tracing::info!(peer = %datagram.peer, bytes = datagram.payload.len(), "UdpSink: first packet sent");
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, peer = %datagram.peer, "UdpSink: send_to failed");
            }
        }
    }
    tracing::debug!(
        packets_sent = packets_sent.load(Ordering::Relaxed),
        "UdpSink: send loop exiting (channel closed)",
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::SocketAddr;

    #[test]
    fn resolve_uses_packet_peer_when_set() {
        let default: Option<SocketAddr> = Some("1.2.3.4:1000".parse().unwrap());
        let resolved = resolve_destination("5.6.7.8:2000", default);
        assert_eq!(resolved, Some("5.6.7.8:2000".parse().unwrap()));
    }

    #[test]
    fn resolve_falls_back_to_default_when_packet_empty() {
        let default: Option<SocketAddr> = Some("1.2.3.4:1000".parse().unwrap());
        let resolved = resolve_destination("", default);
        assert_eq!(resolved, default);
    }

    #[test]
    fn resolve_returns_none_when_packet_empty_and_no_default() {
        let resolved = resolve_destination("", None);
        assert_eq!(resolved, None);
    }

    #[test]
    fn resolve_falls_back_to_default_on_malformed_peer() {
        let default: Option<SocketAddr> = Some("1.2.3.4:1000".parse().unwrap());
        let resolved = resolve_destination("not-an-addr", default);
        assert_eq!(resolved, default);
    }
}
