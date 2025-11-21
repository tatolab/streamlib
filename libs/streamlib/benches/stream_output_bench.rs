use criterion::{black_box, criterion_group, criterion_main, Criterion};
use std::sync::Arc;
use streamlib::core::bus::ports::StreamOutput;
use streamlib::core::frames::AudioFrame;

/// Benchmark: Arc-wrapped vs bare StreamOutput operations
///
/// This measures the performance impact of Arc-wrapping output ports,
/// which is now required for Push mode wakeup support.

fn bench_arc_clone_vs_deep_clone(c: &mut Criterion) {
    let mut group = c.benchmark_group("streamoutput_clone");

    // Benchmark Arc::clone() - just increments atomic counter
    group.bench_function("arc_clone", |b| {
        let output = Arc::new(StreamOutput::<AudioFrame<2>>::new("audio"));

        b.iter(|| {
            let _cloned = black_box(Arc::clone(&output));
        });
    });

    // Benchmark StreamOutput::clone() - deep clone with allocations
    group.bench_function("deep_clone", |b| {
        let output = StreamOutput::<AudioFrame<2>>::new("audio");

        b.iter(|| {
            let _cloned = black_box(output.clone());
        });
    });

    group.finish();
}

fn bench_arc_deref_write(c: &mut Criterion) {
    let mut group = c.benchmark_group("streamoutput_write");

    // Benchmark write through Arc (current approach)
    group.bench_function("arc_wrapped", |b| {
        let output = Arc::new(StreamOutput::<AudioFrame<2>>::new("audio"));

        // Simulate processor setup: take Arc clone
        let output_clone = Arc::clone(&output);

        let mut counter = 0u64;
        b.iter(|| {
            let frame = AudioFrame::<2>::new(vec![0.0; 2048], counter as i64, counter, 48000);
            output_clone.write(black_box(frame));
            counter += 1;
        });
    });

    // Benchmark direct write without Arc (legacy approach)
    group.bench_function("bare", |b| {
        let output = StreamOutput::<AudioFrame<2>>::new("audio");

        let mut counter = 0u64;
        b.iter(|| {
            let frame = AudioFrame::<2>::new(vec![0.0; 2048], counter as i64, counter, 48000);
            output.write(black_box(frame));
            counter += 1;
        });
    });

    group.finish();
}

fn bench_arc_overhead_in_callback(c: &mut Criterion) {
    let mut group = c.benchmark_group("callback_simulation");

    // Simulate AudioCapture callback scenario with Arc
    group.bench_function("with_arc", |b| {
        let output = Arc::new(StreamOutput::<AudioFrame<1>>::new("audio"));

        // Simulate one-time setup: clone Arc for callback
        let output_for_callback = Arc::clone(&output);

        // Simulate callback being invoked many times
        let mut counter = 0u64;
        b.iter(|| {
            // Inside callback: just deref and write
            let frame = AudioFrame::<1>::new(vec![0.0; 512], counter as i64, counter, 48000);
            output_for_callback.write(black_box(frame));
            counter += 1;
        });
    });

    // Simulate AudioCapture callback scenario with deep clone (old way)
    group.bench_function("with_deep_clone", |b| {
        let output = StreamOutput::<AudioFrame<1>>::new("audio");

        // Simulate one-time setup: deep clone for callback
        let output_for_callback = output.clone();

        // Simulate callback being invoked many times
        let mut counter = 0u64;
        b.iter(|| {
            let frame = AudioFrame::<1>::new(vec![0.0; 512], counter as i64, counter, 48000);
            output_for_callback.write(black_box(frame));
            counter += 1;
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_arc_clone_vs_deep_clone,
    bench_arc_deref_write,
    bench_arc_overhead_in_callback,
);
criterion_main!(benches);
