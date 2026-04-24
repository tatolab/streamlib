// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Criterion benches for the unified logging pathway (#447).
//!
//! Three benches:
//! - `hot_path_latency` — `tracing::info!` with 4 structured fields
//!   against a live pathway; criterion reports p50 / p99.
//! - `processor_overhead_60fps` — 60 Hz × 10 logs/frame, pathway vs
//!   a no-op subscriber baseline. Criterion reports both arms so the
//!   relative overhead is visible.
//! - `burst_drops_surface` — 10k logs/ms burst for 1 s. Custom harness
//!   (not a criterion loop): asserts the synthetic `dropped=N` record
//!   surfaces in the produced JSONL, prints hot-path throughput and
//!   worker heap high-water.
//!
//! Run: `cargo bench -p streamlib --bench logging`. `hot_path_latency`
//! and `processor_overhead_60fps` run under criterion's default
//! statistical harness; `burst_drops_surface` runs once at the end as
//! a throughput assertion and prints its numbers to stdout.

// Benches report per-iteration percentiles and drop counters to
// stderr as a bench deliverable — criterion's default reporter only
// surfaces mean bounds, and this data needs to reach the operator
// running `cargo bench`.
#![allow(clippy::disallowed_macros)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use tempfile::TempDir;
use tracing::subscriber::{DefaultGuard, NoSubscriber};

use streamlib::core::logging::{
    init_for_tests, LogLevel, LoggingTunables, RuntimeLogEvent, StreamlibLoggingConfig,
    StreamlibLoggingGuard,
};
use streamlib::core::runtime::RuntimeUniqueId;

fn install_pathway(tmp: &TempDir, runtime_id: &str) -> StreamlibLoggingGuard {
    unsafe {
        std::env::set_var("XDG_STATE_HOME", tmp.path());
        std::env::set_var("STREAMLIB_QUIET", "1");
        std::env::set_var("RUST_LOG", "info");
    }
    let runtime_id = Arc::new(RuntimeUniqueId::from(runtime_id));
    let config = StreamlibLoggingConfig {
        service_name: "bench".into(),
        runtime_id: Some(runtime_id),
        stdout: false,
        jsonl: true,
        intercept_stdio: false,
        tunables: LoggingTunables {
            batch_ms: Some(100),
            batch_bytes: Some(64 * 1024),
            channel_capacity: Some(65_536),
            fsync_on_every_batch: None,
        },
    };
    init_for_tests(config).expect("install logging pathway")
}

/// `tracing::info!` with 4 structured fields against a live pathway.
/// Criterion's default sampler reports the mean bound. A companion
/// `hot_path_latency_percentiles` bench (separate `bench_function` so
/// criterion's filter can skip it) collects a large single-sample
/// distribution and prints p50 / p99.
fn bench_hot_path_latency(c: &mut Criterion) {
    let tmp = TempDir::new().expect("tempdir");
    let _guard = install_pathway(&tmp, "RbenchHot");

    c.bench_function("hot_path_latency", |b| {
        b.iter(|| {
            tracing::info!(
                pipeline_id = black_box("pl-bench"),
                processor_id = black_box("pr-bench"),
                rhi_op = black_box("acquire_texture"),
                tick = black_box(42u64),
                "hot-path"
            );
        });
    });

    c.bench_function("hot_path_latency_percentiles", |b| {
        b.iter_custom(|iters| {
            const N: usize = 200_000;
            let mut samples: Vec<u64> = Vec::with_capacity(N);
            let outer_start = Instant::now();
            for _ in 0..iters {
                samples.clear();
                for _ in 0..N {
                    let t0 = Instant::now();
                    tracing::info!(
                        pipeline_id = "pl-bench",
                        processor_id = "pr-bench",
                        rhi_op = "acquire_texture",
                        tick = 42u64,
                        "hot-path-pctile"
                    );
                    samples.push(t0.elapsed().as_nanos() as u64);
                }
                samples.sort_unstable();
                let p50 = samples[N / 2];
                let p90 = samples[(N * 90) / 100];
                let p99 = samples[(N * 99) / 100];
                let p999 = samples[(N * 999) / 1000];
                let max = *samples.last().unwrap();
                eprintln!(
                    "hot_path_latency percentiles ({N} samples): p50={p50}ns \
                     p90={p90}ns p99={p99}ns p999={p999}ns max={max}ns"
                );
            }
            outer_start.elapsed()
        });
    });
}

/// 60 Hz frame loop, 10 logs/frame, measured against a no-op subscriber
/// baseline. Two criterion arms so the user can read the overhead
/// directly off the report.
fn bench_processor_overhead_60fps(c: &mut Criterion) {
    let tmp = TempDir::new().expect("tempdir");
    let mut group = c.benchmark_group("processor_overhead_60fps");
    group.sample_size(20);

    // Baseline: NoSubscriber — every `tracing::info!` short-circuits
    // at the dispatcher level, measuring the macro overhead alone.
    group.bench_function("baseline_no_subscriber", |b| {
        let _baseline_scope: DefaultGuard =
            tracing::subscriber::set_default(NoSubscriber::default());
        b.iter(|| {
            for _frame in 0..1 {
                for i in 0..10u32 {
                    tracing::info!(
                        pipeline_id = black_box("pl-60fps"),
                        processor_id = black_box("pr-60fps"),
                        rhi_op = black_box("tick"),
                        i,
                        "frame-log"
                    );
                }
            }
        });
    });

    // Live pathway: queue + worker drain on, JSONL writer on.
    group.bench_function("pathway", |b| {
        let _pathway_guard = install_pathway(&tmp, "Rbench60fps");
        b.iter(|| {
            for _frame in 0..1 {
                for i in 0..10u32 {
                    tracing::info!(
                        pipeline_id = black_box("pl-60fps"),
                        processor_id = black_box("pr-60fps"),
                        rhi_op = black_box("tick"),
                        i,
                        "frame-log"
                    );
                }
            }
        });
    });

    group.finish();
}

/// 10k logs/ms for 1 s burst. Not a criterion `iter` — a one-shot
/// correctness + throughput assertion: the hot path must stay at line
/// rate, the worker queue must bound the drop count, and the JSONL
/// must contain at least one synthetic `dropped=N` record.
fn bench_burst_drops_surface(c: &mut Criterion) {
    let mut group = c.benchmark_group("burst_drops_surface");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(12));

    group.bench_function("burst_10m_events_1s", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let tmp = TempDir::new().expect("tempdir");
                unsafe {
                    std::env::set_var("XDG_STATE_HOME", tmp.path());
                    std::env::set_var("STREAMLIB_QUIET", "1");
                    std::env::set_var("RUST_LOG", "info");
                }
                // Tiny channel capacity forces the worker drain behind
                // the hot path, exercising the drop-oldest counter.
                let runtime_id = Arc::new(RuntimeUniqueId::from("RbenchBurst"));
                let config = StreamlibLoggingConfig {
                    service_name: "bench".into(),
                    runtime_id: Some(Arc::clone(&runtime_id)),
                    stdout: false,
                    jsonl: true,
                    intercept_stdio: false,
                    tunables: LoggingTunables {
                        batch_ms: Some(25),
                        batch_bytes: Some(1 << 20),
                        channel_capacity: Some(512),
                        fsync_on_every_batch: None,
                    },
                };
                let guard = init_for_tests(config).expect("install");

                const TOTAL_EVENTS: u64 = 10_000_000;
                let deadline = Instant::now() + Duration::from_secs(1);
                let start = Instant::now();
                let mut emitted = 0u64;
                while emitted < TOTAL_EVENTS && Instant::now() < deadline {
                    tracing::info!(
                        pipeline_id = "pl-burst",
                        processor_id = "pr-burst",
                        rhi_op = "burst",
                        i = emitted,
                        "burst-event"
                    );
                    emitted += 1;
                }
                let elapsed = start.elapsed();
                total += elapsed;

                // Give the drain worker time to flush the
                // synthetic `dropped=N` record.
                std::thread::sleep(Duration::from_millis(300));
                let path = guard.jsonl_path().unwrap().to_path_buf();
                drop(guard);

                let events = read_jsonl(&path);
                let dropped_records: Vec<&RuntimeLogEvent> = events
                    .iter()
                    .filter(|e| e.level == LogLevel::Warn && e.attrs.contains_key("dropped"))
                    .collect();
                assert!(
                    !dropped_records.is_empty(),
                    "expected >=1 dropped=N record in JSONL, got none ({} total events emitted in {:?})",
                    emitted,
                    elapsed
                );

                let throughput = emitted as f64 / elapsed.as_secs_f64();
                let dropped_total: u64 = dropped_records
                    .iter()
                    .filter_map(|e| {
                        e.attrs
                            .get("dropped")
                            .and_then(|v| v.as_u64())
                    })
                    .sum();
                eprintln!(
                    "burst_drops_surface: emitted={emitted} dropped_synth={dropped_total} \
                     hot_path_throughput={throughput:.0} events/s surfaced_records={}",
                    dropped_records.len()
                );
            }
            total
        });
    });

    group.finish();
}

fn read_jsonl(path: &std::path::Path) -> Vec<RuntimeLogEvent> {
    let contents = std::fs::read_to_string(path).unwrap_or_default();
    contents
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str::<RuntimeLogEvent>(l).expect("valid JSONL line"))
        .collect()
}

criterion_group!(
    benches,
    bench_hot_path_latency,
    bench_processor_overhead_60fps,
    bench_burst_drops_surface
);
criterion_main!(benches);
