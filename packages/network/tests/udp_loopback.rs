// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Loopback round-trip integration test for `streamlib-network`.
//!
//! Wires `UdpSource` → `UdpSink` in a real `Runner`, then drives traffic
//! across `127.0.0.1`: a test-side socket sends a datagram to the
//! source's bind port; the source emits a `NetworkPacket` with
//! `peer_addr = <test socket>` onto the link; the sink picks it up and
//! `send_to`s the payload back to that exact peer_addr. The test socket
//! then receives its own bytes echoed back. This exercises:
//!
//! 1. UdpSource binds, recv loop publishes one NetworkPacket per
//!    datagram.
//! 2. iceoryx2 byte-shape mailbox round-trips a NetworkPacket between
//!    two in-process processors.
//! 3. UdpSink reads NetworkPacket from its input port and writes the
//!    payload back to the kernel via send_to.
//! 4. `peer_addr` propagates end-to-end without any router processor
//!    in the middle — the echo-server shape #831 needs is implicit.

use std::net::SocketAddr;
use std::time::Duration;

use serial_test::serial;
use streamlib::sdk::graph::{InputLinkPortRef, OutputLinkPortRef};
use streamlib::sdk::processors::{ProcessorSpec, PROCESSOR_REGISTRY};
use streamlib::sdk::runtime::Runner;
use streamlib::sdk::schema_ident;

/// Bind an ephemeral UDP port, capture its address, drop the socket so
/// the port is free for the processor to bind. The brief window
/// between drop and rebind is racy in principle but in practice
/// nothing else binds the port in the test process inside a few
/// milliseconds; SO_REUSEADDR makes the rebind tolerate TIME_WAIT.
fn pick_free_udp_port() -> SocketAddr {
    let probe = std::net::UdpSocket::bind("127.0.0.1:0").expect("probe bind");
    let addr = probe.local_addr().expect("probe local_addr");
    drop(probe);
    addr
}

#[test]
#[serial]
fn loopback_round_trip_propagates_peer_addr() {
    // Pre-bind the test socket so the sink has somewhere to send to
    // *before* the source has a chance to relay the first packet.
    let test_socket =
        std::net::UdpSocket::bind("127.0.0.1:0").expect("bind test socket");
    test_socket
        .set_read_timeout(Some(Duration::from_secs(3)))
        .expect("set test socket read timeout");

    let source_bind = pick_free_udp_port();

    let runtime = Runner::new().expect("Runner::new");

    // Explicit typed registration replaces the legacy
    // `use foo::Bar as _;` inventory force-link pattern. The typed
    // reference pulls the rlib into the link line (without which
    // rustc's dead-code elimination would drop it); the
    // `register::<P>()` call makes registration intent explicit
    // (idempotent — dedup'd against the rlib's inventory submission).
    PROCESSOR_REGISTRY.register::<streamlib_network::UdpSourceProcessor::Processor>();
    PROCESSOR_REGISTRY.register::<streamlib_network::UdpSinkProcessor::Processor>();

    let source_id = runtime
        .add_processor(ProcessorSpec::new(
            schema_ident!("tatolab", "network", "UdpSource", "1.0.0"),
            serde_json::json!({
                "bind_addr": source_bind.to_string(),
            }),
        ))
        .expect("add UdpSource");

    let sink_id = runtime
        .add_processor(ProcessorSpec::new(
            schema_ident!("tatolab", "network", "UdpSink", "1.0.0"),
            serde_json::json!({}),
        ))
        .expect("add UdpSink");

    runtime
        .connect(
            OutputLinkPortRef::new(source_id.as_str(), "packets"),
            InputLinkPortRef::new(sink_id.as_str(), "packets"),
        )
        .expect("connect UdpSource → UdpSink");

    runtime.start().expect("runtime.start");

    // Give the recv loop a moment to bind + iceoryx2 subscribers to warm
    // up. Without this the first send can race the bind and the kernel
    // drops the datagram silently. 250ms is the documented PUBSUB
    // warm-up ballpark in docs/learnings/pubsub-lazy-init-silent-noop.md
    // and is comfortably above iceoryx2 service-open latency.
    std::thread::sleep(Duration::from_millis(250));

    let payload: &[u8] = b"hello-udp-loopback";
    test_socket
        .send_to(payload, source_bind)
        .expect("test socket send_to source");

    let mut buf = [0u8; 64];
    let (n, recv_from) = test_socket
        .recv_from(&mut buf)
        .expect("test socket recv_from echo");

    assert_eq!(&buf[..n], payload, "payload bytes round-tripped intact");

    // The sink's source-port on its outbound socket is ephemeral and
    // not the source's bind_addr — what matters is the peer_addr was
    // honored end-to-end. The recv succeeding at all proves the sink
    // sent to our exact test_socket's bound port (kernel routes by
    // dst-port match); the recv from 127.0.0.1 proves the loopback
    // path; together they lock peer_addr propagation through the
    // source's emit → iceoryx2 link → sink's send_to chain. If
    // peer_addr were dropped or misrouted, recv_from would have
    // timed out (3-second deadline above).
    assert_eq!(
        recv_from.ip().to_string(),
        "127.0.0.1",
        "echo arrived on loopback, not some other interface",
    );
    assert_ne!(
        recv_from.port(),
        0,
        "echo sender used a real port",
    );

    runtime.stop().expect("runtime.stop");
}
