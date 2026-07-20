// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Microbench for the issue #894 `OutputWriter::write_raw` plugin ABI hop.
//!
//! Two arms compare the per-call cost of the pre-#894 direct-method
//! shape against the post-#894 vtable-dispatch PluginAbiObject. A third bench
//! varies payload size so the report shows how the hop cost scales
//! with the data length:
//!
//! - `baseline_direct_inner` — `Arc<OutputWriterInner>::write_raw`
//!   called directly (pre-#894 shape from the cdylib's perspective:
//!   the cdylib's struct held `Arc<OutputWriter>` and called methods
//!   on it via direct Rust dispatch, with `OutputWriter` being the
//!   real impl, not a PluginAbiObject). Includes the full iceoryx2 publish
//!   + notify step.
//! - `vtable_dispatch` — `OutputWriter::write_raw` on the PluginAbiObject,
//!   which dispatches through the host-installed vtable to the
//!   host-side `OutputWriterInner::write_raw`. In host mode (this
//!   bench) the fn pointer resolves to the same plugin; in cdylib mode
//!   it resolves to the host via the same fn pointer planted
//!   by `HostServices`. The PER-CALL machine cost is identical
//!   between the two modes — both pay the indirect-call overhead;
//!   neither pays any extra marshaling beyond pointer + length pairs
//!   on the stack. The delta vs `baseline_direct_inner` is the cost
//!   of the plugin ABI hop the #894 design adds.
//! - `payload_sweep_vtable` — vtable-dispatch arm at 64 B / 256 B /
//!   1 KiB / 8 KiB / 64 KiB payloads. Tells the reader whether the
//!   hop's per-call cost is dominated by the fixed overhead (call
//!   indirection + msgpack envelope) or scales with payload size
//!   (the inner's `Vec::with_capacity` + slice copy).
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

use streamlib_engine::iceoryx2::{
    ChannelEgressConfig, ChannelTrustTier, OutputWriter, OutputWriterInner, SchemaIdentWire,
    TRUSTED_CHANNEL_PAYLOAD_CEILING_BYTES,
};

/// Per-bench-run unique service-name suffix so parallel benches
/// don't collide on iceoryx2's machine-global `/dev/shm` namespace.
fn unique_suffix(tag: &str) -> String {
    format!(
        "bench/output_writer/{}/{}/{}",
        tag,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    )
}

/// Per-bench fixture that owns the iceoryx2 services + the
/// subscriber/listener so the bench's per-iteration loop can drain
/// in-line (iceoryx2's `Subscriber` / `Listener` are not `Send`,
/// hence no background-drainer thread).
struct BenchFixture {
    inner: Arc<OutputWriterInner>,
    subscriber: iceoryx2::port::subscriber::Subscriber<iceoryx2::service::ipc::Service, [u8], ()>,
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
    let pubsub_name = unique_suffix(&format!("{tag}/pubsub"));
    let notify_name = unique_suffix(&format!("{tag}/notify"));

    let pubsub = node
        .service_builder(&ServiceName::new(&pubsub_name).unwrap())
        .publish_subscribe::<[u8]>()
        .max_publishers(2)
        // Deep ring so 100k+ bench iterations don't backpressure
        // before the in-line drainer catches up.
        .subscriber_max_buffer_size(8192)
        .open_or_create()
        .unwrap();
    // Publisher slice cap covers payload + FRAME_HEADER_SIZE (96 B
    // today). 128 KiB headroom covers the bench's 64 KiB sweep
    // arm with margin.
    let publisher = pubsub
        .publisher_builder()
        .initial_max_slice_len(128 * 1024)
        .create()
        .unwrap();
    let subscriber = pubsub.subscriber_builder().create().unwrap();

    let notify = node
        .service_builder(&ServiceName::new(&notify_name).unwrap())
        .event()
        .max_notifiers(2)
        .max_listeners(1)
        .open_or_create()
        .unwrap();
    let notifier = notify.notifier_builder().create().unwrap();
    let listener = notify.listener_builder().create().unwrap();

    let inner = Arc::new(OutputWriterInner::new());
    let schema_ident =
        SchemaIdentWire::from_segments("tatolab", "bench", "FfiHop", 1, 0, 0).unwrap();
    inner.set_channel_publisher(
        "out",
        schema_ident,
        publisher,
        ChannelEgressConfig {
            service_name: "bench/out".to_string(),
            trust_tier: ChannelTrustTier::Trusted,
            expected_payload_bytes: 4096,
            ceiling_bytes: TRUSTED_CHANNEL_PAYLOAD_CEILING_BYTES,
        },
    );
    inner.add_channel_notifier("out", notifier);

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

/// Vary payload size to characterize how the plugin ABI hop cost scales
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
    subscribers:
        Vec<iceoryx2::port::subscriber::Subscriber<iceoryx2::service::ipc::Service, [u8], ()>>,
    listeners: Vec<iceoryx2::port::listener::Listener<iceoryx2::service::ipc::Service>>,
    _node: Node<iceoryx2::service::ipc::Service>,
}

/// Build an `OutputWriterInner` whose single "out" channel feeds
/// `subscriber_count` subscribers, mirroring the compiler op's 1→N wiring: ONE
/// `set_channel_publisher` + N `add_channel_notifier`, N subscribers on the one
/// pubsub service.
fn build_inner_with_fanout(tag: &str, subscriber_count: usize) -> FanoutFixture {
    let node = NodeBuilder::new().create::<ipc::Service>().unwrap();
    let pubsub_name = unique_suffix(&format!("{tag}/pubsub"));

    let pubsub = node
        .service_builder(&ServiceName::new(&pubsub_name).unwrap())
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

    let inner = Arc::new(OutputWriterInner::new());
    let schema_ident =
        SchemaIdentWire::from_segments("tatolab", "bench", "FfiHop", 1, 0, 0).unwrap();
    inner.set_channel_publisher(
        "out",
        schema_ident,
        publisher,
        ChannelEgressConfig {
            service_name: "bench/out".to_string(),
            trust_tier: ChannelTrustTier::Trusted,
            expected_payload_bytes: 4096,
            ceiling_bytes: TRUSTED_CHANNEL_PAYLOAD_CEILING_BYTES,
        },
    );

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
        inner.add_channel_notifier("out", notifier);
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

criterion_group!(
    benches,
    bench_baseline_direct_inner,
    bench_vtable_dispatch,
    bench_payload_size_sweep,
    bench_write_raw_fanout,
);
criterion_main!(benches);
