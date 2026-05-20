// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Burst-stress for `UdpSource` → `UdpSink` end-to-end. Wires a real
//! `Runner` with the echo-server shape (source publishes
//! `NetworkPacket`s to the sink; sink writes the payload back to
//! `peer_addr`), then fires 6 sender streams at ~200 Hz each for
//! ~1.5 s and asserts every datagram round-trips.
//!
//! What this locks:
//!
//! 1. The Linux `recvmmsg` path in `UdpSource` drains kernel-queued
//!    bursts without dropping when the source's per-call wakeups are
//!    paced by tokio.
//! 2. The default 4 MiB SO_RCVBUF hint (silently clamped to
//!    `net.core.rmem_max` on stock Linux) is large enough to absorb
//!    the burst between wakeups.
//! 3. iceoryx2's per-schema ring depth (the `max_queued_messages:
//!    64` on `NetworkPacket` from #837) doesn't bottleneck the
//!    source→sink mailbox at this rate.
//!
//! Numbers are deliberately conservative for a CI-grade test:
//! 200 Hz × 6 × 1.5 s ≈ 1800 datagrams, well under any single
//! bottleneck on loopback. The stress is the *burst shape*, not
//! the absolute throughput.

use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serial_test::serial;
use streamlib::sdk::graph::{InputLinkPortRef, OutputLinkPortRef};
use streamlib::sdk::processors::{ProcessorSpec, PROCESSOR_REGISTRY};
use streamlib::sdk::runtime::Runner;
use streamlib::sdk::schema_ident;

const SENDER_COUNT: usize = 6;
const PACKETS_PER_SENDER: usize = 300;
const SEND_INTERVAL: Duration = Duration::from_micros(5_000); // 200 Hz per sender
const PAYLOAD_LEN: usize = 64;
const ECHO_DRAIN_AFTER_SEND: Duration = Duration::from_secs(3);

fn pick_free_udp_port() -> SocketAddr {
    let probe = UdpSocket::bind("127.0.0.1:0").expect("probe bind");
    let addr = probe.local_addr().expect("probe local_addr");
    drop(probe);
    addr
}

#[test]
#[serial]
fn burst_six_streams_at_200hz_round_trips_without_loss() {
    let source_bind = pick_free_udp_port();

    let runtime = Runner::new().expect("Runner::new");

    // Explicit typed registration replaces the legacy
    // `use foo::Bar as _;` inventory force-link. The typed
    // reference pulls the rlib into the link line; the
    // `register::<P>()` call makes registration intent explicit
    // (idempotent — dedup'd against the rlib's inventory submission).
    PROCESSOR_REGISTRY.register::<streamlib_network::UdpSourceProcessor::Processor>();
    PROCESSOR_REGISTRY.register::<streamlib_network::UdpSinkProcessor::Processor>();

    let source_id = runtime
        .add_processor(ProcessorSpec::new(
            schema_ident!("tatolab", "network", "UdpSource", "1.0.0"),
            serde_json::json!({
                "bind_addr": source_bind.to_string(),
                // batch_size left at default (64) — the whole point of
                // this test is the default shape, not a tuned override.
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

    // Warm-up so the recv loop is bound and the iceoryx2 subscriber
    // is open before the first burst lands (per
    // docs/learnings/pubsub-lazy-init-silent-noop.md, iceoryx2
    // service open is best-effort and a publish before subscribe is
    // observed disappears silently).
    std::thread::sleep(Duration::from_millis(250));

    // Per-sender shape: one sender thread paces 200 Hz for
    // PACKETS_PER_SENDER datagrams, one receiver thread drains
    // echoes on the same socket with a deadline. Pairing the
    // receiver into a separate thread keeps the sender's pacing
    // free of recv latency.
    let total_sent = Arc::new(AtomicUsize::new(0));
    let total_received = Arc::new(AtomicUsize::new(0));

    let burst_start = Instant::now();
    let mut sender_handles = Vec::with_capacity(SENDER_COUNT);
    let mut receiver_handles = Vec::with_capacity(SENDER_COUNT);

    for sender_idx in 0..SENDER_COUNT {
        // Each sender owns its socket — sink echoes back to peer_addr,
        // and peer_addr is this socket's bound port.
        let sock = Arc::new(UdpSocket::bind("127.0.0.1:0").expect("sender bind"));
        // 50ms read timeout on the receiver thread; long enough that
        // the loop doesn't spin tightly, short enough that the
        // deadline check fires within a few iterations.
        sock.set_read_timeout(Some(Duration::from_millis(50)))
            .expect("set read timeout");

        let sender_sock = Arc::clone(&sock);
        let recv_sock = Arc::clone(&sock);
        let sent_counter = Arc::clone(&total_sent);
        let recv_counter = Arc::clone(&total_received);

        let send_handle = std::thread::Builder::new()
            .name(format!("udp-burst-sender-{sender_idx}"))
            .spawn(move || {
                let mut clock = Instant::now();
                for packet_idx in 0..PACKETS_PER_SENDER {
                    let mut payload = [0u8; PAYLOAD_LEN];
                    payload[0] = sender_idx as u8;
                    payload[1..5].copy_from_slice(&(packet_idx as u32).to_le_bytes());
                    sender_sock
                        .send_to(&payload, source_bind)
                        .expect("send_to");
                    sent_counter.fetch_add(1, Ordering::Relaxed);

                    clock += SEND_INTERVAL;
                    let now = Instant::now();
                    if clock > now {
                        std::thread::sleep(clock - now);
                    } else {
                        clock = now;
                    }
                }
            })
            .expect("spawn sender");
        sender_handles.push(send_handle);

        let recv_handle = std::thread::Builder::new()
            .name(format!("udp-burst-receiver-{sender_idx}"))
            .spawn(move || {
                // Generous deadline: send takes ~1.5s + echo tail.
                let deadline = Instant::now()
                    + Duration::from_secs(2)
                    + ECHO_DRAIN_AFTER_SEND;
                let mut local_received = 0usize;
                let mut buf = [0u8; PAYLOAD_LEN];
                while Instant::now() < deadline && local_received < PACKETS_PER_SENDER {
                    match recv_sock.recv_from(&mut buf) {
                        Ok((n, _peer)) if n == PAYLOAD_LEN => {
                            local_received += 1;
                        }
                        Ok(_) => {} // unexpected short read
                        Err(e)
                            if e.kind() == std::io::ErrorKind::WouldBlock
                                || e.kind() == std::io::ErrorKind::TimedOut => {}
                        Err(e) => panic!("recv_from failed: {e}"),
                    }
                }
                recv_counter.fetch_add(local_received, Ordering::Relaxed);
            })
            .expect("spawn receiver");
        receiver_handles.push(recv_handle);
    }

    for handle in sender_handles {
        handle.join().expect("sender thread");
    }
    let send_elapsed = burst_start.elapsed();

    for handle in receiver_handles {
        handle.join().expect("receiver thread");
    }
    let total_elapsed = burst_start.elapsed();

    runtime.stop().expect("runtime.stop");

    let sent = total_sent.load(Ordering::Relaxed);
    let received = total_received.load(Ordering::Relaxed);
    let expected = SENDER_COUNT * PACKETS_PER_SENDER;

    tracing::info!(
        sent,
        expected,
        received,
        ?send_elapsed,
        ?total_elapsed,
        "burst-stress completed",
    );

    assert_eq!(sent, expected, "every sender completed its full burst");

    // Zero loss is the exit criterion from #838. recvmmsg + the
    // 4 MiB SO_RCVBUF default + iceoryx2's 64-entry NetworkPacket
    // ring + UdpSink's 256-entry mpsc all stack up such that 1800
    // datagrams over ~1.5 s on loopback round-trip cleanly. A
    // regression to per-datagram recv_from or to the old ~208 KiB
    // SO_RCVBUF default would drop a meaningful fraction on this
    // burst shape, which the strict equality assertion catches.
    // If this ever flakes on a contended CI box the failure
    // message includes the wall times so the loss can be
    // re-characterized — but the goal is the exit criterion, not
    // an arbitrary tolerance.
    assert_eq!(
        received, expected,
        "expected zero loss round-trip with recvmmsg + 4 MiB SO_RCVBUF default; \
         got {received}/{expected}. send took {send_elapsed:?}, total {total_elapsed:?}.",
    );
}
