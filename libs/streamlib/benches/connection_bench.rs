use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use streamlib::core::bus::connection::ProcessorConnection;

fn bench_write_happy_path(c: &mut Criterion) {
    let mut group = c.benchmark_group("write_happy_path");

    for buffer_size in [16, 128, 1024].iter() {
        group.bench_with_input(
            BenchmarkId::from_parameter(buffer_size),
            buffer_size,
            |b, &size| {
                let conn = ProcessorConnection::<i32>::new(
                    "source".to_string(),
                    "out".to_string(),
                    "dest".to_string(),
                    "in".to_string(),
                    size,
                );

                let mut counter = 0i32;
                b.iter(|| {
                    conn.write(black_box(counter));
                    counter += 1;

                    // Periodically drain to stay in happy path
                    if counter % (size / 2) as i32 == 0 {
                        conn.read_latest();
                    }
                });
            },
        );
    }
    group.finish();
}

fn bench_write_with_rolloff(c: &mut Criterion) {
    let mut group = c.benchmark_group("write_with_rolloff");

    for buffer_size in [16, 128, 1024].iter() {
        group.bench_with_input(
            BenchmarkId::from_parameter(buffer_size),
            buffer_size,
            |b, &size| {
                let conn = ProcessorConnection::<i32>::new(
                    "source".to_string(),
                    "out".to_string(),
                    "dest".to_string(),
                    "in".to_string(),
                    size,
                );

                // Pre-fill buffer to trigger roll-off
                for i in 0..size {
                    conn.write(i as i32);
                }

                let mut counter = 0i32;
                b.iter(|| {
                    conn.write(black_box(counter));
                    counter += 1;
                });
            },
        );
    }
    group.finish();
}

fn bench_read_latest(c: &mut Criterion) {
    let mut group = c.benchmark_group("read_latest");

    for buffer_depth in [4, 16, 64, 256].iter() {
        group.bench_with_input(
            BenchmarkId::from_parameter(buffer_depth),
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
                    // Fill buffer with 'depth' items
                    for i in 0..depth {
                        conn.write(i as i32);
                    }

                    // read_latest should discard N-1 items and return newest
                    black_box(conn.read_latest());
                });
            },
        );
    }
    group.finish();
}

fn bench_has_data(c: &mut Criterion) {
    let mut group = c.benchmark_group("has_data_lock_free");

    let conn_empty = ProcessorConnection::<i32>::new(
        "source".to_string(),
        "out".to_string(),
        "dest".to_string(),
        "in".to_string(),
        128,
    );

    let conn_full = ProcessorConnection::<i32>::new(
        "source".to_string(),
        "out".to_string(),
        "dest".to_string(),
        "in".to_string(),
        128,
    );
    for i in 0..64 {
        conn_full.write(i);
    }

    group.bench_function("empty_buffer", |b| {
        b.iter(|| {
            black_box(conn_empty.has_data());
        });
    });

    group.bench_function("full_buffer", |b| {
        b.iter(|| {
            black_box(conn_full.has_data());
        });
    });

    group.finish();
}

fn bench_concurrent_write_read(c: &mut Criterion) {
    use std::sync::Arc;
    use std::thread;

    let mut group = c.benchmark_group("concurrent_write_read");

    group.bench_function("single_producer_consumer", |b| {
        b.iter(|| {
            let conn = Arc::new(ProcessorConnection::<i32>::new(
                "source".to_string(),
                "out".to_string(),
                "dest".to_string(),
                "in".to_string(),
                256,
            ));

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
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_write_happy_path,
    bench_write_with_rolloff,
    bench_read_latest,
    bench_has_data,
    bench_concurrent_write_read,
);
criterion_main!(benches);
