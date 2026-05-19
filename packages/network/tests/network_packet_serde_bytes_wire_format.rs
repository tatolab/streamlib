// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Wire-shape lock for `NetworkPacket.payload`. The codegen pipeline emits
//! `#[serde(with = "serde_bytes")]` on the field; `rmp_serde::to_vec_named`
//! must produce a msgpack `bin 8`/`bin 16` tag (0xc4/0xc5) rather than an
//! array tag (0xdc) so each UDP datagram pays 1× wire overhead instead of
//! ~1.5×. This is load-bearing on UdpSource/UdpSink throughput at MTU-sized
//! payloads — see `packages/network/tests/udp_throughput_bench.rs`.

use streamlib_network::_generated_::NetworkPacket;

const MSGPACK_BIN_8: u8 = 0xc4;
const MSGPACK_ARRAY_16: u8 = 0xdc;

#[test]
fn network_packet_payload_serializes_as_msgpack_bin() {
    let packet = NetworkPacket {
        payload: vec![0xff_u8; 100],
        peer_addr: "127.0.0.1:5600".to_string(),
        timestamp_ns: "0".to_string(),
    };
    let wire = rmp_serde::to_vec_named(&packet).expect("rmp_serde::to_vec_named");

    let bin_tag_pos = wire
        .windows(2)
        .position(|w| w[0] == MSGPACK_BIN_8 && w[1] == 100);
    assert!(
        bin_tag_pos.is_some(),
        "NetworkPacket.payload expected as `bin 8` (0xc4) with length 100; \
         wire={:02x?}",
        wire
    );

    let array_tag_present = wire.iter().any(|&b| b == MSGPACK_ARRAY_16);
    assert!(
        !array_tag_present,
        "wire contains `array 16` (0xdc) — codegen attribute regressed on \
         NetworkPacket.payload; wire={:02x?}",
        wire
    );
}

#[test]
fn network_packet_roundtrips_through_rmp_serde() {
    let packet = NetworkPacket {
        payload: (0u8..=255u8).collect(),
        peer_addr: "[::1]:5600".to_string(),
        timestamp_ns: "12345".to_string(),
    };
    let wire = rmp_serde::to_vec_named(&packet).expect("rmp_serde::to_vec_named");
    let decoded: NetworkPacket =
        rmp_serde::from_slice(&wire).expect("rmp_serde::from_slice");
    assert_eq!(decoded, packet, "round-trip must be exact");
}
