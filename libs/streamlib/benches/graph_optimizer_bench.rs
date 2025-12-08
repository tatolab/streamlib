// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// Performance benchmarks for GraphOptimizer
//
// Validates that optimizer operations meet performance targets:
// - add_processor/remove_processor: <50μs
// - add_connection/remove_connection: <50μs
// - optimize(): <100μs for typical graphs (5-20 processors)

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use streamlib::core::{GraphOptimizer, NodeIndex};

fn bench_add_processor(c: &mut Criterion) {
    c.bench_function("add_processor", |b| {
        b.iter(|| {
            let mut optimizer = GraphOptimizer::new();
            let id = format!("processor_{}", line!());
            optimizer.add_processor(
                black_box(&id),
                black_box("TestProcessor".to_string()),
                black_box(None),
            );
        })
    });
}

fn bench_remove_processor(c: &mut Criterion) {
    c.bench_function("remove_processor", |b| {
        b.iter_batched(
            || {
                let mut optimizer = GraphOptimizer::new();
                let id = format!("processor_{}", line!());
                optimizer.add_processor(&id.clone(), "TestProcessor".to_string(), None);
                (optimizer, id)
            },
            |(mut optimizer, id)| {
                black_box(optimizer.remove_processor(&id));
            },
            criterion::BatchSize::SmallInput,
        )
    });
}

fn bench_add_connection(c: &mut Criterion) {
    c.bench_function("add_connection", |b| {
        b.iter_batched(
            || {
                let mut optimizer = GraphOptimizer::new();
                let p1 = format!("processor_{}", line!());
                let p2 = format!("processor_{}", line!());
                optimizer.add_processor(&p1, "P1".to_string(), None);
                optimizer.add_processor(&p2, "P2".to_string(), None);
                (optimizer, p1, p2)
            },
            |(mut optimizer, p1, p2)| {
                let conn_id = format!("conn_{}", 0);
                black_box(optimizer.add_connection(
                    black_box(&conn_id),
                    &p1,
                    &p2,
                    black_box("out".to_string()),
                    black_box("in".to_string()),
                    black_box("VideoFrame".to_string()),
                    black_box(3),
                ));
            },
            criterion::BatchSize::SmallInput,
        )
    });
}

fn bench_remove_connection(c: &mut Criterion) {
    c.bench_function("remove_connection", |b| {
        b.iter_batched(
            || {
                let mut optimizer = GraphOptimizer::new();
                let p1 = format!("processor_{}", line!());
                let p2 = format!("processor_{}", line!());
                let conn_id = format!("conn_0");
                optimizer.add_processor(&p1, "P1".to_string(), None);
                optimizer.add_processor(&p2, "P2".to_string(), None);
                optimizer.add_connection(
                    &conn_id,
                    &p1,
                    &p2,
                    "out".to_string(),
                    "in".to_string(),
                    "VideoFrame".to_string(),
                    3,
                );
                (optimizer, conn_id)
            },
            |(mut optimizer, conn_id)| {
                black_box(optimizer.remove_connection(&conn_id));
            },
            criterion::BatchSize::SmallInput,
        )
    });
}

fn bench_optimize(c: &mut Criterion) {
    let mut group = c.benchmark_group("optimize");

    for size in [1, 5, 10, 20, 50].iter() {
        group.bench_with_input(BenchmarkId::from_parameter(size), size, |b, &size| {
            b.iter_batched(
                || {
                    let mut optimizer = GraphOptimizer::new();
                    let mut processors = Vec::new();

                    // Create linear pipeline
                    for i in 0..size {
                        let id = format!("processor_{}", line!());
                        optimizer.add_processor(&id.clone(), format!("P{}", i), None);
                        processors.push(id);
                    }

                    // Connect processors in sequence
                    for i in 0..size - 1 {
                        let conn_id = format!("conn_{}", i);
                        optimizer.add_connection(
                            &conn_id,
                            &processors[i],
                            &processors[i + 1],
                            "out".to_string(),
                            "in".to_string(),
                            "VideoFrame".to_string(),
                            3,
                        );
                    }

                    optimizer
                },
                |mut optimizer| {
                    black_box(optimizer.optimize());
                },
                criterion::BatchSize::SmallInput,
            )
        });
    }

    group.finish();
}

fn bench_cache_hit(c: &mut Criterion) {
    c.bench_function("optimize_cache_hit", |b| {
        let mut optimizer = GraphOptimizer::new();

        // Create small graph
        let p1 = format!("processor_{}", line!());
        let p2 = format!("processor_{}", line!());
        optimizer.add_processor(&p1, "P1".to_string(), None);
        optimizer.add_processor(&p2, "P2".to_string(), None);
        let conn_id = "conn_0".to_string();
        optimizer.add_connection(
            &conn_id,
            &p1,
            &p2,
            "out".to_string(),
            "in".to_string(),
            "VideoFrame".to_string(),
            3,
        );

        // Prime cache
        optimizer.optimize();

        // Benchmark cache hits
        b.iter(|| {
            black_box(optimizer.optimize());
        });
    });
}

fn bench_query_operations(c: &mut Criterion) {
    let mut group = c.benchmark_group("query");

    // Setup: Create medium-sized graph
    let mut optimizer = GraphOptimizer::new();
    let mut processors = Vec::new();

    for i in 0..20 {
        let id = format!("processor_{}", line!());
        optimizer.add_processor(&id.clone(), format!("P{}", i), None);
        processors.push(id);
    }

    for i in 0..19 {
        let conn_id = format!("conn_{}", i);
        optimizer.add_connection(
            &conn_id,
            &processors[i],
            &processors[i + 1],
            "out".to_string(),
            "in".to_string(),
            "VideoFrame".to_string(),
            3,
        );
    }

    group.bench_function("topological_order", |b| {
        b.iter(|| {
            black_box(optimizer.topological_order());
        });
    });

    group.bench_function("find_sources", |b| {
        b.iter(|| {
            black_box(optimizer.find_sources());
        });
    });

    group.bench_function("find_sinks", |b| {
        b.iter(|| {
            black_box(optimizer.find_sinks());
        });
    });

    group.bench_function("find_upstream", |b| {
        let target = &processors[10];
        b.iter(|| {
            black_box(optimizer.find_upstream(target));
        });
    });

    group.bench_function("find_downstream", |b| {
        let target = &processors[5];
        b.iter(|| {
            black_box(optimizer.find_downstream(target));
        });
    });

    group.bench_function("to_dot", |b| {
        b.iter(|| {
            black_box(optimizer.to_dot());
        });
    });

    group.bench_function("to_json", |b| {
        b.iter(|| {
            black_box(optimizer.to_json());
        });
    });

    group.finish();
}

fn bench_diamond_graph(c: &mut Criterion) {
    c.bench_function("optimize_diamond_graph", |b| {
        b.iter_batched(
            || {
                let mut optimizer = GraphOptimizer::new();

                // Create diamond pattern
                let p1 = format!("processor_{}", line!());
                let p2 = format!("processor_{}", line!());
                let p3 = format!("processor_{}", line!());
                let p4 = format!("processor_{}", line!());

                optimizer.add_processor(&p1, "P1".to_string(), None);
                optimizer.add_processor(&p2, "P2".to_string(), None);
                optimizer.add_processor(&p3, "P3".to_string(), None);
                optimizer.add_processor(&p4, "P4".to_string(), None);

                let conn_0 = "conn_0".to_string();
                optimizer.add_connection(
                    &conn_0,
                    &p1,
                    &p2,
                    "out".to_string(),
                    "in".to_string(),
                    "VideoFrame".to_string(),
                    3,
                );

                let conn_1 = "conn_1".to_string();
                optimizer.add_connection(
                    &conn_1,
                    &p1,
                    &p3,
                    "out".to_string(),
                    "in".to_string(),
                    "VideoFrame".to_string(),
                    3,
                );

                let conn_2 = "conn_2".to_string();
                optimizer.add_connection(
                    &conn_2,
                    &p2,
                    &p4,
                    "out".to_string(),
                    "in".to_string(),
                    "VideoFrame".to_string(),
                    3,
                );

                let conn_3 = "conn_3".to_string();
                optimizer.add_connection(
                    &conn_3,
                    &p3,
                    &p4,
                    "out".to_string(),
                    "in".to_string(),
                    "VideoFrame".to_string(),
                    3,
                );

                optimizer
            },
            |mut optimizer| {
                black_box(optimizer.optimize());
            },
            criterion::BatchSize::SmallInput,
        )
    });
}

criterion_group!(
    benches,
    bench_add_processor,
    bench_remove_processor,
    bench_add_connection,
    bench_remove_connection,
    bench_optimize,
    bench_cache_hit,
    bench_query_operations,
    bench_diamond_graph,
);

criterion_main!(benches);
