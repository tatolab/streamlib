// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Microbench for the `OutputWriter::write_raw` per-call cost.
//!
//! Two arms compare a direct call on the inner against a call through
//! the public `OutputWriter` handle. A third bench varies payload size
//! so the report shows how the cost scales with the data length:
//!
//! - `baseline_direct_inner` — `Arc<OutputWriterInner>::write_raw`
//!   called directly. Includes the full iceoryx2 publish + notify step.
//! - `vtable_dispatch` — `OutputWriter::write_raw` on the public handle,
//!   which derefs the opaque handle to the same
//!   `OutputWriterInner::write_raw`. The delta vs `baseline_direct_inner`
//!   is the handle-indirection cost. (The `vtable_dispatch` /
//!   `payload_sweep_vtable` criterion ids date from the deleted
//!   vtable-dispatch arm and are kept as-is: criterion ids are
//!   historical comparison keys, and renaming them orphans every
//!   recorded baseline.)
//! - `payload_sweep_vtable` — vtable-dispatch arm at 64 B / 256 B /
//!   1 KiB / 8 KiB / 64 KiB payloads. Tells the reader whether the
//!   hop's per-call cost is dominated by the fixed overhead (call
//!   indirection + msgpack envelope) or scales with payload size
//!   (the single payload copy into the loan).
//! - `channel_round_trip` — `write_raw` through `OutputWriterInner`
//!   then `read_raw` through `InputMailboxesInner` at 256 B and
//!   64 KiB payloads: the engine's full data-plane hop, write and
//!   read timed together (#1822). The destination is a self-driven
//!   sink — no notifier — so the arm is not directly comparable to
//!   the notify-carrying arms above; throughput counts payload bytes
//!   per hop, though each hop moves them twice (into the loan, out
//!   of shared memory).
//! - `fanout_1_to_n` — one channel publisher feeding N ∈ {1,2,4,8}
//!   subscribers. `write_raw` issues a SINGLE zero-copy loan + send
//!   that reaches every subscriber (the transport inversion, #1419);
//!   only the per-destination `notify()` is O(N). Throughput is
//!   reported as frames-delivered (N per call), so the curve stays
//!   near-flat per delivered frame — the signature the retired
//!   per-connection copy loop (one frame build + send PER subscriber,
//!   O(N) copies) could not produce.
//!
//! Run: `cargo bench -p streamlib-engine --bench output_writer_ffi_hop`.
//! The bench writes to a per-run-unique iceoryx2 service name so
//! parallel `cargo bench` invocations don't collide on the
//! machine-global `/dev/shm` namespace.

#![allow(clippy::disallowed_macros)]

use std::sync::Arc;

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use iceoryx2::prelude::*;

use streamlib_engine::core::machine_global_unique_name::mint_machine_global_unique_name_suffix;
use streamlib_engine::iceoryx2::{
    ChannelEgressConfig, ChannelTrustTier, InboundLinkName, InputMailboxesInner, OutputWriter,
    OutputWriterInner, ReadMode, TRUSTED_CHANNEL_PAYLOAD_CEILING_BYTES,
};

/// Per-bench-run unique service-name suffix so parallel benches
/// don't collide on iceoryx2's machine-global `/dev/shm` namespace.
fn unique_suffix(tag: &str) -> String {
    format!(
        "bench/output_writer/{tag}/{}",
        mint_machine_global_unique_name_suffix()
    )
}

type BenchChannelPublisher =
    iceoryx2::port::publisher::Publisher<iceoryx2::service::ipc::Service, [u8], ()>;
type BenchChannelSubscriber =
    iceoryx2::port::subscriber::Subscriber<iceoryx2::service::ipc::Service, [u8], ()>;

/// The one pubsub shape every bench arm publishes through: 2-publisher cap, a
/// deep 8192-sample ring so 100k+ bench iterations don't backpressure ahead of
/// the in-line drainers, and a 128 KiB slice cap covering the 64 KiB sweep arm
/// plus `FRAME_HEADER_SIZE` with margin.
fn open_bench_channel_pubsub(
    node: &Node<ipc::Service>,
    tag: &str,
    subscriber_count: usize,
) -> (BenchChannelPublisher, Vec<BenchChannelSubscriber>) {
    let pubsub = node
        .service_builder(&ServiceName::new(&unique_suffix(&format!("{tag}/pubsub"))).unwrap())
        .publish_subscribe::<[u8]>()
        .max_publishers(2)
        .max_subscribers(subscriber_count + 1)
        .subscriber_max_buffer_size(8192)
        .open_or_create()
        .unwrap();
    let publisher = pubsub
        .publisher_builder()
        .initial_max_slice_len(128 * 1024)
        .create()
        .unwrap();
    let subscribers = (0..subscriber_count)
        .map(|_| pubsub.subscriber_builder().create().unwrap())
        .collect();
    (publisher, subscribers)
}

/// An `OutputWriterInner` primed with the bench's one "out" channel egress.
fn output_writer_inner_publishing_to(publisher: BenchChannelPublisher) -> OutputWriterInner {
    let output_writer_inner = OutputWriterInner::new();
    output_writer_inner.set_channel_publisher(
        "out",
        publisher,
        ChannelEgressConfig {
            service_name: "bench/out".to_string(),
            trust_tier: ChannelTrustTier::Trusted,
            expected_payload_bytes: 4096,
            ceiling_bytes: TRUSTED_CHANNEL_PAYLOAD_CEILING_BYTES,
        },
    );
    output_writer_inner
}

/// Per-bench fixture that owns the iceoryx2 services + the
/// subscriber/listener so the bench's per-iteration loop can drain
/// in-line (iceoryx2's `Subscriber` / `Listener` are not `Send`,
/// hence no background-drainer thread).
struct BenchFixture {
    inner: Arc<OutputWriterInner>,
    subscriber: BenchChannelSubscriber,
    listener: iceoryx2::port::listener::Listener<iceoryx2::service::ipc::Service>,
    // Keep the node + service handles alive for the bench's
    // lifetime so the publisher inside the inner doesn't observe
    // a torn-down service mid-iteration.
    _node: Node<iceoryx2::service::ipc::Service>,
}

/// Build an `OutputWriterInner` with one configured downstream
/// connection. Returns the bench fixture (the bench iter loop
/// drains the subscriber + listener in-line between writes so the
/// publisher's ring doesn't back-pressure).
fn build_inner_with_connection(tag: &str) -> BenchFixture {
    let node = NodeBuilder::new().create::<ipc::Service>().unwrap();
    let (publisher, mut subscribers) = open_bench_channel_pubsub(&node, tag, 1);
    let subscriber = subscribers.pop().unwrap();

    let notify = node
        .service_builder(&ServiceName::new(&unique_suffix(&format!("{tag}/notify"))).unwrap())
        .event()
        .max_notifiers(2)
        .max_listeners(1)
        .open_or_create()
        .unwrap();
    let notifier = notify.notifier_builder().create().unwrap();
    let listener = notify.listener_builder().create().unwrap();

    let inner = Arc::new(output_writer_inner_publishing_to(publisher));
    inner.add_channel_link("out", "L-bench-ffi-hop", Some(notifier));

    BenchFixture {
        inner,
        subscriber,
        listener,
        _node: node,
    }
}

#[inline(always)]
fn drain_in_line(fx: &BenchFixture) {
    // Single non-blocking receive per write — keeps the
    // publisher's ring drained at a steady-state rate without
    // adding measurable per-iteration cost when the ring is mostly
    // empty. The publish-then-receive sequence is what the
    // engine's real consumer does on every frame.
    let _ = fx.subscriber.receive();
    let _ = fx.listener.try_wait_all(|_| {});
}

fn bench_baseline_direct_inner(c: &mut Criterion) {
    let fx = build_inner_with_connection("baseline");
    // Typical payload: 256 bytes mirrors a small msgpack-encoded
    // VideoFrame / control message — close to the steady-state
    // payload size on the drone-racing control loop.
    let payload = vec![0u8; 256];
    c.bench_function("output_writer_write_raw/baseline_direct_inner_256B", |b| {
        b.iter(|| {
            fx.inner
                .write_raw(black_box("out"), black_box(&payload), black_box(0))
                .unwrap();
            drain_in_line(&fx);
        });
    });
}

fn bench_vtable_dispatch(c: &mut Criterion) {
    let fx = build_inner_with_connection("vtable");
    let writer = OutputWriter::from_inner_arc(fx.inner.clone());
    let payload = vec![0u8; 256];
    c.bench_function("output_writer_write_raw/vtable_dispatch_256B", |b| {
        b.iter(|| {
            writer
                .write_raw(black_box("out"), black_box(&payload), black_box(0))
                .unwrap();
            drain_in_line(&fx);
        });
    });
}

/// Vary payload size to characterize how the handle-indirection cost scales
/// with the data length. Useful for the drone-racing JPEG path
/// (typical 30-100 KB JPEG payloads per frame) vs the control-path
/// (sub-100 byte messages).
fn bench_payload_size_sweep(c: &mut Criterion) {
    let fx = build_inner_with_connection("sweep");
    let writer = OutputWriter::from_inner_arc(fx.inner.clone());
    let mut group = c.benchmark_group("output_writer_write_raw/payload_sweep_vtable");
    for size in [64usize, 256, 1024, 8 * 1024, 64 * 1024] {
        let payload = vec![0u8; size];
        group.bench_with_input(
            criterion::BenchmarkId::from_parameter(size),
            &payload,
            |b, p| {
                b.iter(|| {
                    writer
                        .write_raw(black_box("out"), black_box(p), black_box(0))
                        .unwrap();
                    drain_in_line(&fx);
                });
            },
        );
    }
    group.finish();
}

/// Fan-out fixture: one channel publisher feeding N subscribers, each with its
/// own destination notifier + listener. The iter loop drains all N subscribers
/// and listeners in-line so the publisher's ring doesn't back-pressure.
struct FanoutFixture {
    inner: Arc<OutputWriterInner>,
    subscribers: Vec<BenchChannelSubscriber>,
    listeners: Vec<iceoryx2::port::listener::Listener<iceoryx2::service::ipc::Service>>,
    _node: Node<iceoryx2::service::ipc::Service>,
}

/// Build an `OutputWriterInner` whose single "out" channel feeds
/// `subscriber_count` subscribers, mirroring the compiler op's 1→N wiring: ONE
/// `set_channel_publisher` + N `add_channel_link`, N subscribers on the one
/// pubsub service.
fn build_inner_with_fanout(tag: &str, subscriber_count: usize) -> FanoutFixture {
    let node = NodeBuilder::new().create::<ipc::Service>().unwrap();
    let (publisher, subscribers) = open_bench_channel_pubsub(&node, tag, subscriber_count);
    let inner = Arc::new(output_writer_inner_publishing_to(publisher));

    let mut listeners = Vec::with_capacity(subscriber_count);
    for i in 0..subscriber_count {
        let notify_name = unique_suffix(&format!("{tag}/notify/{i}"));
        let notify = node
            .service_builder(&ServiceName::new(&notify_name).unwrap())
            .event()
            .max_notifiers(2)
            .max_listeners(1)
            .open_or_create()
            .unwrap();
        let notifier = notify.notifier_builder().create().unwrap();
        let listener = notify.listener_builder().create().unwrap();
        inner.add_channel_link("out", &format!("L-bench-fanout-{i}"), Some(notifier));
        listeners.push(listener);
    }

    FanoutFixture {
        inner,
        subscribers,
        listeners,
        _node: node,
    }
}

#[inline(always)]
fn drain_fanout_in_line(fx: &FanoutFixture) {
    for subscriber in &fx.subscribers {
        let _ = subscriber.receive();
    }
    for listener in &fx.listeners {
        let _ = listener.try_wait_all(|_| {});
    }
}

/// One publisher fanning out to N ∈ {1,2,4,8} subscribers. Throughput is
/// reported as frames delivered (N per `write_raw` call), so a flat
/// per-delivered-frame cost is the single-loan signature; the retired
/// per-connection copy loop would show cost climbing linearly with N.
fn bench_write_raw_fanout(c: &mut Criterion) {
    let payload = vec![0u8; 256];
    let mut group = c.benchmark_group("output_writer_write_raw/fanout_1_to_n");
    for subscriber_count in [1usize, 2, 4, 8] {
        let fx = build_inner_with_fanout("fanout", subscriber_count);
        let writer = OutputWriter::from_inner_arc(fx.inner.clone());
        group.throughput(criterion::Throughput::Elements(subscriber_count as u64));
        group.bench_with_input(
            criterion::BenchmarkId::from_parameter(subscriber_count),
            &subscriber_count,
            |b, _| {
                b.iter(|| {
                    writer
                        .write_raw(black_box("out"), black_box(&payload), black_box(0))
                        .unwrap();
                    drain_fanout_in_line(&fx);
                });
            },
        );
    }
    group.finish();
}

/// Round-trip fixture: an `OutputWriterInner` publishing into the channel an
/// `InputMailboxesInner` subscribes, mirroring the engine's full data-plane
/// hop. The destination is wired as a self-driven sink (no notifier) — the
/// bench loop reads every write, so a wakeup fd would only add noise.
struct RoundTripFixture {
    output_writer_inner: OutputWriterInner,
    input_mailboxes_inner: InputMailboxesInner,
    _node: Node<iceoryx2::service::ipc::Service>,
}

fn build_round_trip(tag: &str) -> RoundTripFixture {
    let node = NodeBuilder::new().create::<ipc::Service>().unwrap();
    let (publisher, mut subscribers) = open_bench_channel_pubsub(&node, tag, 1);
    let subscriber = subscribers.pop().unwrap();

    let output_writer_inner = output_writer_inner_publishing_to(publisher);
    output_writer_inner.add_channel_link("out", "L-bench-round-trip", None);

    let input_mailboxes_inner = InputMailboxesInner::new();
    input_mailboxes_inner.add_port("in", 8, ReadMode::ReadNextInOrder);
    input_mailboxes_inner.add_channel_subscriber(
        "in",
        "L-bench-round-trip",
        &InboundLinkName::from("pbench/out"),
        subscriber,
    );

    RoundTripFixture {
        output_writer_inner,
        input_mailboxes_inner,
        _node: node,
    }
}

/// The full data-plane hop: publish through `OutputWriterInner::write_raw`,
/// receive + read through `InputMailboxesInner::read_raw`. 256 B mirrors a
/// control message, 64 KiB a camera-class frame — the size where the read
/// side's per-frame copy cost dominates.
fn bench_channel_round_trip(c: &mut Criterion) {
    let mut group = c.benchmark_group("output_writer_write_raw/channel_round_trip");
    for size in [256usize, 64 * 1024] {
        let fx = build_round_trip(&format!("round_trip/{size}"));
        let payload = vec![0u8; size];
        group.throughput(criterion::Throughput::Bytes(size as u64));
        group.bench_with_input(
            criterion::BenchmarkId::from_parameter(size),
            &payload,
            |b, p| {
                b.iter(|| {
                    fx.output_writer_inner
                        .write_raw(black_box("out"), black_box(p), black_box(0))
                        .unwrap();
                    let (data, _timestamp_ns) = fx
                        .input_mailboxes_inner
                        .read_raw(black_box("in"))
                        .unwrap()
                        .expect("every write is read back in-line");
                    black_box(data);
                });
            },
        );
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_baseline_direct_inner,
    bench_vtable_dispatch,
    bench_payload_size_sweep,
    bench_write_raw_fanout,
    bench_channel_round_trip,
);
criterion_main!(benches);
