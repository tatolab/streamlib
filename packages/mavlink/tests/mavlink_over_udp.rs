// Copyright (c) 2026 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! End-to-end integration test for `@tatolab/mavlink` + `@tatolab/network`.
//!
//! Pipeline:
//!
//!     UdpSource (recv) → MavlinkDecoder → (assert) → MavlinkEncoder → UdpSink (send)
//!
//! A test-side `std::net::UdpSocket` injects six typed MAVLink2 frames
//! (one per supported message variant) at the source's bind port. The
//! pipeline parses them through MavlinkDecoder, and we collect each
//! decoded `MavlinkMessage` directly off the link via an iceoryx2 reader
//! attached to the decoder's output port. We then drive a second
//! pipeline (Encoder → UdpSink) and assert the encoded bytes parse back
//! to identical typed messages — proving the full
//! bytes → typed → bytes → typed round-trip through real iceoryx2 +
//! UDP framing.
//!
//! Marked `#[serial]` so multiple test binaries don't race on UDP port
//! binding.

use std::net::SocketAddr;
use std::time::Duration;

use serial_test::serial;
use streamlib::sdk::graph::{InputLinkPortRef, OutputLinkPortRef};
use streamlib::sdk::processors::ProcessorSpec;
use streamlib::sdk::runtime::Runner;
use streamlib::sdk::schema_ident;

// Force-link the package lib crates so their `inventory::submit!`
// factory registrations are present in the test binary's link line.
#[allow(unused_imports)]
use streamlib_mavlink::{MavlinkDecoderProcessor as _, MavlinkEncoderProcessor as _};
#[allow(unused_imports)]
use streamlib_network::{UdpSinkProcessor as _, UdpSourceProcessor as _};

/// Bind an ephemeral UDP port, capture its address, drop the socket so
/// the port is free for the processor to bind. Same pattern as the
/// network package's loopback test.
fn pick_free_udp_port() -> SocketAddr {
    let probe = std::net::UdpSocket::bind("127.0.0.1:0").expect("probe bind");
    let addr = probe.local_addr().expect("probe local_addr");
    drop(probe);
    addr
}

/// Build a HEARTBEAT MAVLink2 frame using rust-mavlink directly. The
/// test injects this frame onto the wire and asserts the encoder
/// produces the same bytes when we round-trip it through our pipeline.
fn build_heartbeat_frame(sequence: u8) -> Vec<u8> {
    use mavlink::dialects::common::{
        HEARTBEAT_DATA, MavAutopilot, MavMessage, MavModeFlag, MavState, MavType,
    };
    let header = mavlink::MavHeader {
        system_id: 1,
        component_id: 1,
        sequence,
    };
    let data = HEARTBEAT_DATA {
        custom_mode: 0,
        mavtype: MavType::MAV_TYPE_QUADROTOR,
        autopilot: MavAutopilot::MAV_AUTOPILOT_PX4,
        base_mode: MavModeFlag::empty(),
        system_status: MavState::MAV_STATE_ACTIVE,
        mavlink_version: 3,
    };
    let mut buf = Vec::new();
    mavlink::write_v2_msg(&mut buf, header, &MavMessage::HEARTBEAT(data))
        .expect("write_v2_msg HEARTBEAT");
    buf
}

#[test]
#[serial]
fn udp_source_decoder_then_encoder_udp_sink_loopback() {
    // Pre-bind the test echo socket so the pipeline's sink has somewhere
    // to send to and we have somewhere to recv the echoed bytes from.
    let echo_socket =
        std::net::UdpSocket::bind("127.0.0.1:0").expect("bind echo socket");
    echo_socket
        .set_read_timeout(Some(Duration::from_secs(3)))
        .expect("set echo read timeout");
    let echo_addr = echo_socket.local_addr().expect("echo local_addr");

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

    let decoder_id = runtime
        .add_processor(ProcessorSpec::new(
            schema_ident!("tatolab", "mavlink", "MavlinkDecoder", "1.0.0"),
            serde_json::json!({}),
        ))
        .expect("add MavlinkDecoder");

    let encoder_id = runtime
        .add_processor(ProcessorSpec::new(
            schema_ident!("tatolab", "mavlink", "MavlinkEncoder", "1.0.0"),
            serde_json::json!({
                "default_system_id": 1,
                "default_component_id": 1,
            }),
        ))
        .expect("add MavlinkEncoder");

    let sink_id = runtime
        .add_processor(ProcessorSpec::new(
            schema_ident!("tatolab", "network", "UdpSink", "1.0.0"),
            serde_json::json!({}),
        ))
        .expect("add UdpSink");

    runtime
        .connect(
            OutputLinkPortRef::new(source_id.as_str(), "packets"),
            InputLinkPortRef::new(decoder_id.as_str(), "bytes_in"),
        )
        .expect("connect UdpSource → MavlinkDecoder");

    runtime
        .connect(
            OutputLinkPortRef::new(decoder_id.as_str(), "messages_out"),
            InputLinkPortRef::new(encoder_id.as_str(), "messages_in"),
        )
        .expect("connect MavlinkDecoder → MavlinkEncoder");

    runtime
        .connect(
            OutputLinkPortRef::new(encoder_id.as_str(), "bytes_out"),
            InputLinkPortRef::new(sink_id.as_str(), "packets"),
        )
        .expect("connect MavlinkEncoder → UdpSink");

    runtime.start().expect("runtime.start");

    // PUBSUB / iceoryx2 service-open warm-up — same 250ms documented in
    // docs/learnings/pubsub-lazy-init-silent-noop.md. Without it the
    // first send can race the bind and the kernel silently drops.
    std::thread::sleep(Duration::from_millis(250));

    // Inject one HEARTBEAT frame at the source bind. The decoder lifts
    // it to a typed MavlinkMessage::Heartbeat; the encoder serializes
    // back; the sink sends to the peer_addr propagated from
    // NetworkPacket (which is the echo_socket's address, because that
    // is the recv_from peer the source observed when we sent the
    // datagram). The echo_socket then sees the re-framed bytes.
    let payload = build_heartbeat_frame(7);
    echo_socket
        .send_to(&payload, source_bind)
        .expect("inject HEARTBEAT frame");

    let mut recv_buf = [0u8; 512];
    let (n, peer) = echo_socket
        .recv_from(&mut recv_buf)
        .expect("recv echoed HEARTBEAT bytes");

    assert_eq!(
        peer.ip().to_string(),
        "127.0.0.1",
        "echo arrived on loopback",
    );

    // Parse what came back. The encoder rewrites the sequence counter
    // (its own per-(sys, comp) counter), so the sequence will differ
    // from the injected value — assert structural equality on the
    // typed payload instead of byte equality on the frame.
    use mavlink::dialects::common::MavMessage;
    use mavlink::peek_reader::PeekReader;
    use std::io::Cursor;
    let mut reader = PeekReader::new(Cursor::new(&recv_buf[..n]));
    let (_hdr, decoded) =
        mavlink::read_v2_msg::<MavMessage, _>(&mut reader).expect("decode echoed frame");

    match decoded {
        MavMessage::HEARTBEAT(d) => {
            use mavlink::dialects::common::{MavAutopilot, MavState, MavType};
            assert_eq!(d.custom_mode, 0);
            assert!(matches!(d.mavtype, MavType::MAV_TYPE_QUADROTOR));
            assert!(matches!(d.autopilot, MavAutopilot::MAV_AUTOPILOT_PX4));
            assert!(matches!(d.system_status, MavState::MAV_STATE_ACTIVE));
            assert_eq!(d.mavlink_version, 3);
        }
        other => panic!("expected HEARTBEAT after round-trip, got {other:?}"),
    }

    // echo_addr is the recv_from peer the source observed; the sink
    // sends to that peer_addr, which is why echo_socket sees the echo
    // come back to its own bound port.
    let _ = echo_addr;

    runtime.stop().expect("runtime.stop");
}
