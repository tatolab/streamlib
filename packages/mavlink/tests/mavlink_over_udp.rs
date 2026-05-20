// Copyright (c) 2026 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! End-to-end integration test for `@tatolab/mavlink` + `@tatolab/network`.
//!
//! Pipeline:
//!
//!     UdpSource (recv) → MavlinkDecoder → MavlinkEncoder → UdpSink (send)
//!
//! A test-side `std::net::UdpSocket` injects one frame of each of the
//! six supported MAVLink2 message variants into the source's bind port.
//! Each frame flows through the decoder (bytes → typed), then the
//! encoder (typed → bytes), then back out the sink to the test socket.
//! The test parses each echoed datagram and asserts that the typed
//! payload survives the full bytes → typed → bytes → typed round-trip
//! through real iceoryx2 + UDP framing.
//!
//! Marked `#[serial]` so multiple test binaries don't race on UDP port
//! binding.

use std::net::SocketAddr;
use std::time::Duration;

use serial_test::serial;
use streamlib::sdk::graph::{InputLinkPortRef, OutputLinkPortRef};
use streamlib::sdk::processors::{ProcessorSpec, PROCESSOR_REGISTRY};
use streamlib::sdk::runtime::Runner;
use streamlib::sdk::schema_ident;

/// Explicit typed registration for the package processors this test
/// drives. Replaces the legacy `use foo::Bar as _;` inventory
/// force-link pattern.
fn register_test_processors() {
    PROCESSOR_REGISTRY.register::<streamlib_network::UdpSourceProcessor::Processor>();
    PROCESSOR_REGISTRY.register::<streamlib_network::UdpSinkProcessor::Processor>();
    PROCESSOR_REGISTRY.register::<streamlib_mavlink::MavlinkDecoderProcessor::Processor>();
    PROCESSOR_REGISTRY.register::<streamlib_mavlink::MavlinkEncoderProcessor::Processor>();
}

use mavlink::MavHeader;
use mavlink::dialects::common::{
    ATTITUDE_DATA, AttitudeTargetTypemask, HEARTBEAT_DATA, HIGHRES_IMU_DATA, HighresImuUpdatedFlags,
    MavAutopilot, MavFrame, MavMessage, MavModeFlag, MavState, MavType, PositionTargetTypemask,
    SET_ATTITUDE_TARGET_DATA, SET_POSITION_TARGET_LOCAL_NED_DATA, TIMESYNC_DATA,
};
use mavlink::peek_reader::PeekReader;

/// Bind an ephemeral UDP port, capture its address, drop the socket so
/// the port is free for the processor to bind. Same pattern as the
/// network package's loopback test.
fn pick_free_udp_port() -> SocketAddr {
    let probe = std::net::UdpSocket::bind("127.0.0.1:0").expect("probe bind");
    let addr = probe.local_addr().expect("probe local_addr");
    drop(probe);
    addr
}

fn build_frame(seq: u8, msg: MavMessage) -> Vec<u8> {
    let header = MavHeader {
        system_id: 1,
        component_id: 1,
        sequence: seq,
    };
    let mut buf = Vec::new();
    mavlink::write_v2_msg(&mut buf, header, &msg).expect("write_v2_msg");
    buf
}

fn reference_frames() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        (
            "heartbeat",
            build_frame(
                0,
                MavMessage::HEARTBEAT(HEARTBEAT_DATA {
                    custom_mode: 0,
                    mavtype: MavType::MAV_TYPE_QUADROTOR,
                    autopilot: MavAutopilot::MAV_AUTOPILOT_PX4,
                    base_mode: MavModeFlag::empty(),
                    system_status: MavState::MAV_STATE_ACTIVE,
                    mavlink_version: 3,
                }),
            ),
        ),
        (
            "attitude",
            build_frame(
                1,
                MavMessage::ATTITUDE(ATTITUDE_DATA {
                    time_boot_ms: 12345,
                    roll: 0.1,
                    pitch: -0.2,
                    yaw: 1.5,
                    rollspeed: 0.01,
                    pitchspeed: -0.02,
                    yawspeed: 0.15,
                }),
            ),
        ),
        (
            "highres_imu",
            build_frame(
                2,
                MavMessage::HIGHRES_IMU(HIGHRES_IMU_DATA {
                    time_usec: 1_234_567_890_123_456,
                    xacc: 0.1,
                    yacc: 0.2,
                    zacc: -9.81,
                    xgyro: 0.01,
                    ygyro: 0.02,
                    zgyro: -0.03,
                    xmag: 0.5,
                    ymag: -0.4,
                    zmag: 0.9,
                    abs_pressure: 1013.25,
                    diff_pressure: 0.5,
                    pressure_alt: 100.0,
                    temperature: 22.5,
                    fields_updated: HighresImuUpdatedFlags::empty(),
                    id: 0,
                }),
            ),
        ),
        (
            "set_position_target_local_ned",
            build_frame(
                3,
                MavMessage::SET_POSITION_TARGET_LOCAL_NED(SET_POSITION_TARGET_LOCAL_NED_DATA {
                    time_boot_ms: 12345,
                    x: 1.0,
                    y: 2.0,
                    z: -3.0,
                    vx: 0.1,
                    vy: 0.2,
                    vz: -0.3,
                    afx: 0.0,
                    afy: 0.0,
                    afz: 0.0,
                    yaw: 0.5,
                    yaw_rate: 0.05,
                    type_mask: PositionTargetTypemask::empty(),
                    target_system: 1,
                    target_component: 1,
                    coordinate_frame: MavFrame::MAV_FRAME_LOCAL_NED,
                }),
            ),
        ),
        (
            "set_attitude_target",
            build_frame(
                4,
                MavMessage::SET_ATTITUDE_TARGET(SET_ATTITUDE_TARGET_DATA {
                    time_boot_ms: 12345,
                    q: [1.0, 0.0, 0.0, 0.0],
                    body_roll_rate: 0.0,
                    body_pitch_rate: 0.0,
                    body_yaw_rate: 0.0,
                    thrust: 0.5,
                    target_system: 1,
                    target_component: 1,
                    type_mask: AttitudeTargetTypemask::empty(),
                    thrust_body: [0.0, 0.0, 0.0],
                }),
            ),
        ),
        (
            "timesync",
            build_frame(
                5,
                MavMessage::TIMESYNC(TIMESYNC_DATA {
                    tc1: 1_234_567_890,
                    ts1: 9_876_543_210,
                    target_system: 0,
                    target_component: 0,
                }),
            ),
        ),
    ]
}

fn parse_kind(bytes: &[u8]) -> &'static str {
    let mut reader = PeekReader::new(std::io::Cursor::new(bytes));
    let (_hdr, msg) = mavlink::read_v2_msg::<MavMessage, _>(&mut reader)
        .expect("decode echoed frame");
    match msg {
        MavMessage::HEARTBEAT(_) => "heartbeat",
        MavMessage::ATTITUDE(_) => "attitude",
        MavMessage::HIGHRES_IMU(_) => "highres_imu",
        MavMessage::SET_POSITION_TARGET_LOCAL_NED(_) => "set_position_target_local_ned",
        MavMessage::SET_ATTITUDE_TARGET(_) => "set_attitude_target",
        MavMessage::TIMESYNC(_) => "timesync",
        other => panic!("unexpected MavMessage variant in echo: {other:?}"),
    }
}

#[test]
#[serial]
fn udp_source_decoder_then_encoder_udp_sink_loopback_all_six_variants() {
    // Pre-bind the test echo socket — this is both the injecting peer
    // (sends to source's bind addr) and the receiving peer (sink sends
    // back to recv_from peer, which is this socket).
    let echo_socket =
        std::net::UdpSocket::bind("127.0.0.1:0").expect("bind echo socket");
    echo_socket
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("set echo read timeout");
    let source_bind = pick_free_udp_port();

    let runtime = Runner::new().expect("Runner::new");
    register_test_processors();

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

    let frames = reference_frames();
    for (kind, bytes) in &frames {
        echo_socket
            .send_to(bytes, source_bind)
            .unwrap_or_else(|e| panic!("inject {kind}: {e}"));
    }

    // Collect echoes until we've seen all 6 variants or hit the read
    // timeout. recv_from's `peer` is the sink's outbound source port
    // (an ephemeral port chosen when the sink bound 0.0.0.0:0). What
    // we're asserting via `recv_from` succeeding at all is that the
    // sink sent the bytes to the echo socket's bind port — the kernel
    // would not route them to us otherwise. peer_addr propagation is
    // implicit in the recv working.
    let mut seen: std::collections::HashMap<String, Vec<u8>> =
        std::collections::HashMap::new();
    let mut recv_buf = [0u8; 512];
    for _ in 0..frames.len() {
        let (n, peer) = match echo_socket.recv_from(&mut recv_buf) {
            Ok(p) => p,
            Err(e) => {
                eprintln!(
                    "[integration] recv timed out after {} variants: {e}; seen={:?}",
                    seen.len(),
                    seen.keys().collect::<Vec<_>>()
                );
                break;
            }
        };
        assert_eq!(peer.ip().to_string(), "127.0.0.1", "echo arrived on loopback");
        assert_ne!(peer.port(), 0, "echo sender used a real port");
        let kind = parse_kind(&recv_buf[..n]);
        seen.entry(kind.to_string())
            .or_insert_with(|| recv_buf[..n].to_vec());
    }

    // Stop the runtime BEFORE asserting so teardown counters land in
    // the logs even if the assertion below fails.
    runtime.stop().expect("runtime.stop");

    let expected: std::collections::HashSet<String> = frames
        .iter()
        .map(|(kind, _)| kind.to_string())
        .collect();
    let seen_kinds: std::collections::HashSet<String> = seen.keys().cloned().collect();
    assert_eq!(seen_kinds, expected, "missing variants in echo set");

    // Stronger lock: decode each echoed frame back into a typed
    // MavMessage and assert the typed payload survived the
    // bytes → typed → bytes → typed round-trip through real iceoryx2 +
    // UDP. Sequence and src system/component IDs are rewritten by the
    // encoder (per-(sys, comp) auto-increment, default 1/1), so they
    // don't match the injected values — compare only the typed body.
    for (kind, expected_bytes) in &frames {
        let echoed = seen.get(*kind).unwrap_or_else(|| {
            panic!("missing echoed frame for {kind}")
        });

        let expected_typed = read_typed(expected_bytes);
        let echoed_typed = read_typed(echoed);

        assert_eq!(
            std::mem::discriminant(&expected_typed),
            std::mem::discriminant(&echoed_typed),
            "echo for {kind} round-tripped to a different MavMessage variant",
        );
        assert_typed_body_eq(*kind, &expected_typed, &echoed_typed);
    }
}

fn read_typed(bytes: &[u8]) -> MavMessage {
    let mut reader = PeekReader::new(std::io::Cursor::new(bytes));
    let (_hdr, msg) = mavlink::read_v2_msg::<MavMessage, _>(&mut reader)
        .expect("decode frame for body comparison");
    msg
}

fn assert_typed_body_eq(kind: &str, expected: &MavMessage, got: &MavMessage) {
    match (expected, got) {
        (MavMessage::HEARTBEAT(a), MavMessage::HEARTBEAT(b)) => {
            assert_eq!(a.custom_mode, b.custom_mode, "{kind}.custom_mode");
            assert_eq!(a.mavtype as u32, b.mavtype as u32, "{kind}.mavtype");
            assert_eq!(a.autopilot as u32, b.autopilot as u32, "{kind}.autopilot");
            assert_eq!(a.base_mode.bits(), b.base_mode.bits(), "{kind}.base_mode");
            assert_eq!(
                a.system_status as u32, b.system_status as u32,
                "{kind}.system_status"
            );
            assert_eq!(a.mavlink_version, b.mavlink_version, "{kind}.mavlink_version");
        }
        (MavMessage::ATTITUDE(a), MavMessage::ATTITUDE(b)) => {
            assert_eq!(a.time_boot_ms, b.time_boot_ms, "{kind}.time_boot_ms");
            assert_eq!(a.roll.to_bits(), b.roll.to_bits(), "{kind}.roll");
            assert_eq!(a.pitch.to_bits(), b.pitch.to_bits(), "{kind}.pitch");
            assert_eq!(a.yaw.to_bits(), b.yaw.to_bits(), "{kind}.yaw");
            assert_eq!(a.rollspeed.to_bits(), b.rollspeed.to_bits(), "{kind}.rollspeed");
            assert_eq!(a.pitchspeed.to_bits(), b.pitchspeed.to_bits(), "{kind}.pitchspeed");
            assert_eq!(a.yawspeed.to_bits(), b.yawspeed.to_bits(), "{kind}.yawspeed");
        }
        (MavMessage::HIGHRES_IMU(a), MavMessage::HIGHRES_IMU(b)) => {
            assert_eq!(a.time_usec, b.time_usec, "{kind}.time_usec");
            assert_eq!(a.xacc.to_bits(), b.xacc.to_bits(), "{kind}.xacc");
            assert_eq!(a.yacc.to_bits(), b.yacc.to_bits(), "{kind}.yacc");
            assert_eq!(a.zacc.to_bits(), b.zacc.to_bits(), "{kind}.zacc");
            assert_eq!(
                a.fields_updated.bits(),
                b.fields_updated.bits(),
                "{kind}.fields_updated"
            );
            assert_eq!(a.id, b.id, "{kind}.id");
        }
        (MavMessage::SET_POSITION_TARGET_LOCAL_NED(a), MavMessage::SET_POSITION_TARGET_LOCAL_NED(b)) => {
            assert_eq!(a.time_boot_ms, b.time_boot_ms);
            assert_eq!(a.x.to_bits(), b.x.to_bits());
            assert_eq!(a.y.to_bits(), b.y.to_bits());
            assert_eq!(a.z.to_bits(), b.z.to_bits());
            assert_eq!(a.target_system, b.target_system);
            assert_eq!(a.target_component, b.target_component);
            assert_eq!(a.type_mask.bits(), b.type_mask.bits());
            assert_eq!(a.coordinate_frame as u32, b.coordinate_frame as u32);
        }
        (MavMessage::SET_ATTITUDE_TARGET(a), MavMessage::SET_ATTITUDE_TARGET(b)) => {
            assert_eq!(a.time_boot_ms, b.time_boot_ms);
            assert_eq!(a.q, b.q, "{kind}.q");
            assert_eq!(a.target_system, b.target_system);
            assert_eq!(a.target_component, b.target_component);
            assert_eq!(a.type_mask.bits(), b.type_mask.bits());
            assert_eq!(a.thrust.to_bits(), b.thrust.to_bits());
            assert_eq!(a.thrust_body, b.thrust_body, "{kind}.thrust_body");
        }
        (MavMessage::TIMESYNC(a), MavMessage::TIMESYNC(b)) => {
            assert_eq!(a.tc1, b.tc1, "{kind}.tc1");
            assert_eq!(a.ts1, b.ts1, "{kind}.ts1");
            assert_eq!(a.target_system, b.target_system);
            assert_eq!(a.target_component, b.target_component);
        }
        (e, g) => panic!("variant mismatch for {kind}: expected {e:?}, got {g:?}"),
    }
}
