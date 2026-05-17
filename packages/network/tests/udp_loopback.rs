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
use streamlib::sdk::processors::ProcessorSpec;
use streamlib::sdk::runtime::Runner;
use streamlib::sdk::schema_ident;

// Force-link the package's lib crate so its `inventory::submit!` factory
// registrations (one per `#[streamlib::sdk::processor("...")]`) are
// pulled into the test binary. Without an explicit reference rustc's
// dead-code elimination would drop the lib entirely from the link line.
#[allow(unused_imports)]
use streamlib_network::{UdpSinkProcessor as _, UdpSourceProcessor as _};

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
    let test_addr = test_socket.local_addr().expect("test socket local_addr");

    let source_bind = pick_free_udp_port();

    let runtime = Runner::new().expect("Runner::new");

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
    // honored (data arrived back at test_socket at all). Just confirm
    // the recv came from 127.0.0.1 to lock in the loopback path.
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
    // The destination of the echo was our test_addr (the kernel
    // routed it via 127.0.0.1, which we just confirmed).
    let _ = test_addr;

    runtime.stop().expect("runtime.stop");
}
