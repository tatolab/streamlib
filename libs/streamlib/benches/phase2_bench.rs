use criterion::{
    black_box, criterion_group, criterion_main, BenchmarkGroup, BenchmarkId, Criterion,
};
use std::sync::Arc;
use std::thread;
use streamlib::core::bus::connection::{create_owned_connection, ProcessorConnection};

/// Phase 2 Benchmark: Compare lock-based (Phase 1) vs lock-free (Phase 2) connections

fn bench_phase1_vs_phase2_write(c: &mut Criterion) {
    let mut group = c.benchmark_group("write_comparison");

    // Phase 1: Lock-based write (with mutex)
    group.bench_function("phase1_lock_based", |b| {
        let conn = ProcessorConnection::<i32>::new(
            "source".to_string(),
            "out".to_string(),
            "dest".to_string(),
            "in".to_string(),
            128,
        );

        let mut counter = 0i32;
        b.iter(|| {
            conn.write(black_box(counter));
            counter += 1;

            // Periodically drain to stay in happy path
            if counter % 64 == 0 {
                conn.read_latest();
            }
        });
    });

    // Phase 2: Lock-free write (owned)
    group.bench_function("phase2_lock_free", |b| {
        let (mut producer, mut consumer) = create_owned_connection::<i32>(128);

        let mut counter = 0i32;
        b.iter(|| {
            producer.write(black_box(counter));
            counter += 1;

            // Periodically drain to stay in happy path
            if counter % 64 == 0 {
                consumer.read_latest();
            }
        });
    });

    group.finish();
}

fn bench_phase1_vs_phase2_read_latest(c: &mut Criterion) {
    let mut group = c.benchmark_group("read_latest_comparison");

    for buffer_depth in [4, 16, 64].iter() {
        // Phase 1: Lock-based read_latest
        group.bench_with_input(
            BenchmarkId::new("phase1_lock_based", buffer_depth),
            buffer_depth,
            |b, &depth| {
                let conn = ProcessorConnection::<i32>::new(
                    "source".to_string(),
                    "out".to_string(),
                    "dest".to_string(),
                    "in".to_string(),
                    depth,
                );

                b.iter(|| {
                    // Fill buffer
                    for i in 0..depth {
                        conn.write(i as i32);
                    }

                    // read_latest should discard N-1 items
                    black_box(conn.read_latest());
                });
            },
        );

        // Phase 2: Lock-free read_latest
        group.bench_with_input(
            BenchmarkId::new("phase2_lock_free", buffer_depth),
            buffer_depth,
            |b, &depth| {
                let (mut producer, mut consumer) = create_owned_connection::<i32>(depth);

                b.iter(|| {
                    // Fill buffer
                    for i in 0..depth {
                        producer.write(i as i32);
                    }

                    // read_latest should discard N-1 items
                    black_box(consumer.read_latest());
                });
            },
        );
    }

    group.finish();
}

fn bench_phase1_vs_phase2_has_data(c: &mut Criterion) {
    let mut group = c.benchmark_group("has_data_comparison");

    // Phase 1: Lock-based has_data (with atomic cache)
    group.bench_function("phase1_lock_based", |b| {
        let conn = ProcessorConnection::<i32>::new(
            "source".to_string(),
            "out".to_string(),
            "dest".to_string(),
            "in".to_string(),
            128,
        );
        conn.write(42);

        b.iter(|| {
            black_box(conn.has_data());
        });
    });

    // Phase 2: Lock-free has_data
    group.bench_function("phase2_lock_free", |b| {
        let (mut producer, consumer) = create_owned_connection::<i32>(128);
        producer.write(42);

        b.iter(|| {
            black_box(consumer.has_data());
        });
    });

    group.finish();
}

fn bench_phase1_vs_phase2_concurrent(c: &mut Criterion) {
    let mut group = c.benchmark_group("concurrent_comparison");

    // Phase 1: Lock-based concurrent write/read
    group.bench_function("phase1_lock_based", |b| {
        b.iter_batched(
            || {
                // Setup: Create connection once per sample
                Arc::new(ProcessorConnection::<i32>::new(
                    "source".to_string(),
                    "out".to_string(),
                    "dest".to_string(),
                    "in".to_string(),
                    256,
                ))
            },
            |conn| {
                // Benchmark: Spawn threads and measure the actual concurrent work
                let conn_writer = Arc::clone(&conn);
                let conn_reader = Arc::clone(&conn);

                let writer = thread::spawn(move || {
                    for i in 0..1000 {
                        conn_writer.write(i);
                    }
                });

                let reader = thread::spawn(move || {
                    let mut count = 0;
                    while count < 1000 {
                        if conn_reader.has_data() {
                            conn_reader.read_latest();
                            count += 1;
                        }
                    }
                });

                writer.join().unwrap();
                reader.join().unwrap();
            },
            criterion::BatchSize::SmallInput,
        )
    });

    // Phase 2: Lock-free concurrent write/read
    // Note: Can't use Arc with owned types, need different approach
    group.bench_function("phase2_lock_free", |b| {
        b.iter_batched(
            || {
                // Setup: Create owned connection once per sample
                create_owned_connection::<i32>(256)
            },
            |(mut producer, mut consumer)| {
                // Benchmark: Move owned halves into threads and measure concurrent work
                let writer = thread::spawn(move || {
                    for i in 0..1000 {
                        producer.write(i);
                    }
                });

                let reader = thread::spawn(move || {
                    let mut count = 0;
                    while count < 1000 {
                        if consumer.has_data() {
                            consumer.read_latest();
                            count += 1;
                        }
                    }
                });

                writer.join().unwrap();
                reader.join().unwrap();
            },
            criterion::BatchSize::SmallInput,
        )
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_phase1_vs_phase2_write,
    bench_phase1_vs_phase2_read_latest,
    bench_phase1_vs_phase2_has_data,
    bench_phase1_vs_phase2_concurrent,
);
criterion_main!(benches);
