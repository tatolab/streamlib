use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use parking_lot::Mutex;
use std::sync::Arc;
use streamlib::core::pubsub::{Event, EventBus, EventListener};
use streamlib::core::{KeyCode, KeyState};

// Simple test listener that counts events
struct CountingListener {
    count: Arc<std::sync::atomic::AtomicUsize>,
}

impl CountingListener {
    fn new() -> Self {
        Self {
            count: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }

    fn count(&self) -> usize {
        self.count.load(std::sync::atomic::Ordering::Relaxed)
    }
}

impl EventListener for CountingListener {
    fn on_event(&mut self, _event: &Event) -> streamlib::core::error::Result<()> {
        self.count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }
}

// Slow listener that simulates work
struct SlowListener {
    work_duration_us: u64,
}

impl SlowListener {
    fn new(work_duration_us: u64) -> Self {
        Self { work_duration_us }
    }
}

impl EventListener for SlowListener {
    fn on_event(&mut self, _event: &Event) -> streamlib::core::error::Result<()> {
        std::thread::sleep(std::time::Duration::from_micros(self.work_duration_us));
        Ok(())
    }
}

// Benchmark: Single publish with varying number of subscribers
fn bench_publish_scaling_subscribers(c: &mut Criterion) {
    let mut group = c.benchmark_group("publish_scaling_subscribers");

    for num_subscribers in [1, 5, 10, 50, 100, 500].iter() {
        group.throughput(Throughput::Elements(*num_subscribers as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(num_subscribers),
            num_subscribers,
            |b, &num_subs| {
                let bus = EventBus::new();

                // Subscribe multiple listeners
                let listeners: Vec<_> = (0..num_subs)
                    .map(|_| {
                        let listener_concrete = Arc::new(Mutex::new(CountingListener::new()));
                        let listener: Arc<Mutex<dyn EventListener>> = listener_concrete.clone();
                        bus.subscribe("test-topic", listener);
                        listener_concrete
                    })
                    .collect();

                let event = Event::Custom {
                    topic: "test-topic".to_string(),
                    data: serde_json::json!({"value": 42}),
                };

                b.iter(|| {
                    bus.publish(black_box("test-topic"), black_box(&event));
                });

                // Verify all listeners received the event
                let total_count: usize = listeners.iter().map(|l| l.lock().count()).sum();
                assert!(total_count > 0);
            },
        );
    }
    group.finish();
}

// Benchmark: High-frequency publishing (simulating mouse movement)
fn bench_high_frequency_publishing(c: &mut Criterion) {
    let mut group = c.benchmark_group("high_frequency_publishing");

    // Simulate different mouse movement rates (Hz)
    for frequency_hz in [60, 120, 240, 500, 1000].iter() {
        group.throughput(Throughput::Elements(*frequency_hz as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{}Hz", frequency_hz)),
            frequency_hz,
            |b, &freq| {
                let bus = EventBus::new();

                // Add a few listeners
                let _listeners: Vec<_> = (0..3)
                    .map(|_| {
                        let listener_concrete = Arc::new(Mutex::new(CountingListener::new()));
                        let listener: Arc<Mutex<dyn EventListener>> = listener_concrete.clone();
                        bus.subscribe("input:mouse", listener);
                        listener_concrete
                    })
                    .collect();

                b.iter(|| {
                    // Simulate burst of mouse events
                    for i in 0..freq {
                        let event = Event::custom(
                            "input:mouse",
                            serde_json::json!({
                                "x": (i % 1920) as f32,
                                "y": (i % 1080) as f32,
                            }),
                        );
                        bus.publish(black_box("input:mouse"), black_box(&event));
                    }
                });
            },
        );
    }
    group.finish();
}

// Benchmark: Many topics with selective subscribers
fn bench_topic_routing(c: &mut Criterion) {
    let mut group = c.benchmark_group("topic_routing");

    for num_topics in [10, 50, 100, 500].iter() {
        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{}_topics", num_topics)),
            num_topics,
            |b, &num_topics| {
                let bus = EventBus::new();

                // Create listeners for each topic
                let _listeners: Vec<_> = (0..num_topics)
                    .map(|i| {
                        let topic = format!("topic-{}", i);
                        let listener_concrete = Arc::new(Mutex::new(CountingListener::new()));
                        let listener: Arc<Mutex<dyn EventListener>> = listener_concrete.clone();
                        bus.subscribe(&topic, listener);
                        listener_concrete
                    })
                    .collect();

                b.iter(|| {
                    // Publish to a specific topic
                    let topic = format!("topic-{}", num_topics / 2);
                    let event = Event::Custom {
                        topic: topic.clone(),
                        data: serde_json::json!({"test": "data"}),
                    };
                    bus.publish(black_box(&topic), black_box(&event));
                });
            },
        );
    }
    group.finish();
}

// Benchmark: Parallel publishing from multiple threads
fn bench_concurrent_publishing(c: &mut Criterion) {
    let mut group = c.benchmark_group("concurrent_publishing");

    for num_threads in [2, 4, 8, 16].iter() {
        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{}_threads", num_threads)),
            num_threads,
            |b, &num_threads| {
                let bus = Arc::new(EventBus::new());

                // Add a listener
                let listener_concrete = Arc::new(Mutex::new(CountingListener::new()));
                let listener: Arc<Mutex<dyn EventListener>> = listener_concrete.clone();
                bus.subscribe("concurrent-topic", listener);

                b.iter(|| {
                    let handles: Vec<_> = (0..num_threads)
                        .map(|i| {
                            let bus = Arc::clone(&bus);
                            std::thread::spawn(move || {
                                let event = Event::Custom {
                                    topic: "concurrent-topic".to_string(),
                                    data: serde_json::json!({"thread": i}),
                                };
                                bus.publish("concurrent-topic", &event);
                            })
                        })
                        .collect();

                    for handle in handles {
                        handle.join().unwrap();
                    }
                });
            },
        );
    }
    group.finish();
}

// Benchmark: Slow vs fast listeners (fire-and-forget behavior)
fn bench_slow_listener_isolation(c: &mut Criterion) {
    let mut group = c.benchmark_group("slow_listener_isolation");

    for slow_listener_delay_us in [0, 100, 1000, 10000].iter() {
        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{}us_delay", slow_listener_delay_us)),
            slow_listener_delay_us,
            |b, &delay| {
                let bus = EventBus::new();

                // Add a fast listener
                let fast_listener_concrete = Arc::new(Mutex::new(CountingListener::new()));
                let fast_listener: Arc<Mutex<dyn EventListener>> = fast_listener_concrete.clone();
                bus.subscribe("mixed-speed", fast_listener);

                // Add a slow listener
                let slow_listener_concrete = Arc::new(Mutex::new(SlowListener::new(delay)));
                let slow_listener: Arc<Mutex<dyn EventListener>> = slow_listener_concrete;
                bus.subscribe("mixed-speed", slow_listener);

                let event = Event::Custom {
                    topic: "mixed-speed".to_string(),
                    data: serde_json::json!({"test": "data"}),
                };

                b.iter(|| {
                    bus.publish(black_box("mixed-speed"), black_box(&event));
                });

                // Verify fast listener still got events
                assert!(fast_listener_concrete.lock().count() > 0);
            },
        );
    }
    group.finish();
}

// Benchmark: Event size impact
fn bench_event_size_impact(c: &mut Criterion) {
    let mut group = c.benchmark_group("event_size_impact");

    for data_size_kb in [1, 10, 32, 64].iter() {
        group.throughput(Throughput::Bytes((*data_size_kb * 1024) as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{}KB", data_size_kb)),
            data_size_kb,
            |b, &size_kb| {
                let bus = EventBus::new();

                let listener_concrete = Arc::new(Mutex::new(CountingListener::new()));
                let listener: Arc<Mutex<dyn EventListener>> = listener_concrete.clone();
                bus.subscribe("large-event", listener);

                // Create large payload
                let payload = "x".repeat(size_kb * 1024);
                let event = Event::Custom {
                    topic: "large-event".to_string(),
                    data: serde_json::json!({"data": payload}),
                };

                b.iter(|| {
                    bus.publish(black_box("large-event"), black_box(&event));
                });
            },
        );
    }
    group.finish();
}

// Benchmark: Subscribe/unsubscribe overhead
fn bench_subscribe_unsubscribe(c: &mut Criterion) {
    let mut group = c.benchmark_group("subscribe_unsubscribe");

    group.bench_function("subscribe", |b| {
        let bus = EventBus::new();

        b.iter(|| {
            let listener_concrete = Arc::new(Mutex::new(CountingListener::new()));
            let listener: Arc<Mutex<dyn EventListener>> = listener_concrete.clone();
            bus.subscribe(black_box("test-topic"), listener);
            // Listener drops here, will be cleaned up on next publish
        });
    });

    group.finish();
}

// Benchmark: Keyboard event burst (simulating typing)
fn bench_keyboard_event_burst(c: &mut Criterion) {
    let mut group = c.benchmark_group("keyboard_event_burst");

    // Simulate different typing speeds (chars per second)
    for chars_per_second in [5, 10, 20, 50].iter() {
        group.throughput(Throughput::Elements(*chars_per_second as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{}cps", chars_per_second)),
            chars_per_second,
            |b, &cps| {
                let bus = EventBus::new();

                let listener_concrete = Arc::new(Mutex::new(CountingListener::new()));
                let listener: Arc<Mutex<dyn EventListener>> = listener_concrete.clone();
                bus.subscribe("input:keyboard", listener);

                let keys = vec![KeyCode::H, KeyCode::E, KeyCode::L, KeyCode::L, KeyCode::O];

                b.iter(|| {
                    // Simulate burst of keypresses
                    for _ in 0..cps {
                        for key in &keys {
                            let event = Event::custom(
                                "input:keyboard",
                                serde_json::json!({
                                    "key": format!("{:?}", key),
                                    "state": "Pressed",
                                }),
                            );
                            bus.publish(black_box("input:keyboard"), black_box(&event));
                        }
                    }
                });
            },
        );
    }
    group.finish();
}

// Simulate CPU-bound work (video/audio processing)
fn simulate_work_microseconds(duration_us: u64) {
    let start = std::time::Instant::now();
    let target = std::time::Duration::from_micros(duration_us);

    // Busy loop with actual computation to prevent optimization
    let mut accumulator: f64 = 0.0;
    while start.elapsed() < target {
        // Simulate computation (sqrt is CPU-intensive enough to not be optimized away)
        accumulator += (accumulator + 1.0).sqrt();
    }
    std::hint::black_box(accumulator);
}

// Listener that simulates video effect processing
struct VideoEffectListener {
    effect_type: &'static str,
    processing_time_us: u64,
}

impl VideoEffectListener {
    fn new(effect_type: &'static str, processing_time_us: u64) -> Self {
        Self {
            effect_type,
            processing_time_us,
        }
    }
}

impl EventListener for VideoEffectListener {
    fn on_event(&mut self, _event: &Event) -> streamlib::core::error::Result<()> {
        simulate_work_microseconds(self.processing_time_us);
        Ok(())
    }
}

// Listener that simulates audio DSP processing
struct AudioDSPListener {
    dsp_type: &'static str,
    processing_time_us: u64,
}

impl AudioDSPListener {
    fn new(dsp_type: &'static str, processing_time_us: u64) -> Self {
        Self {
            dsp_type,
            processing_time_us,
        }
    }
}

impl EventListener for AudioDSPListener {
    fn on_event(&mut self, _event: &Event) -> streamlib::core::error::Result<()> {
        simulate_work_microseconds(self.processing_time_us);
        Ok(())
    }
}

// Benchmark: Realistic video processing with multiple effects
fn bench_realistic_video_processing(c: &mut Criterion) {
    let mut group = c.benchmark_group("realistic_video_processing");

    // Test at different frame rates
    for fps in [30, 60, 120].iter() {
        group.throughput(Throughput::Elements(*fps as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{}fps", fps)),
            fps,
            |b, &fps| {
                let bus = EventBus::new();

                // Add realistic video effect listeners
                let effects = vec![
                    ("gaussian_blur", 80),    // ~80µs for 3x3 convolution
                    ("color_correction", 15), // ~15µs for RGB matrix + gamma
                    ("edge_detection", 120),  // ~120µs for Sobel filter
                    ("frame_analysis", 40),   // ~40µs for histogram
                ];

                let _listeners: Vec<_> = effects
                    .into_iter()
                    .map(|(name, time_us)| {
                        let listener_concrete =
                            Arc::new(Mutex::new(VideoEffectListener::new(name, time_us)));
                        let listener: Arc<Mutex<dyn EventListener>> = listener_concrete.clone();
                        bus.subscribe("video:frame", listener);
                        listener_concrete
                    })
                    .collect();

                let frame_event = Event::Custom {
                    topic: "video:frame".to_string(),
                    data: serde_json::json!({"frame_number": 0, "timestamp_ns": 0}),
                };

                b.iter(|| {
                    // Simulate frame processing at target FPS
                    for _ in 0..fps {
                        bus.publish(black_box("video:frame"), black_box(&frame_event));
                    }
                });
            },
        );
    }
    group.finish();
}

// Benchmark: Realistic audio processing with DSP effects
fn bench_realistic_audio_processing(c: &mut Criterion) {
    let mut group = c.benchmark_group("realistic_audio_processing");

    // Audio callback rates (sample_rate / buffer_size)
    // 48000 / 512 = 93.75Hz, 48000 / 256 = 187.5Hz
    for callbacks_per_sec in [93, 187].iter() {
        group.throughput(Throughput::Elements(*callbacks_per_sec as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{}Hz", callbacks_per_sec)),
            callbacks_per_sec,
            |b, &rate| {
                let bus = EventBus::new();

                // Add realistic audio DSP listeners
                let dsp_effects = vec![
                    ("3band_eq", 8),      // ~8µs for biquad filters
                    ("compressor", 5),    // ~5µs for envelope + gain
                    ("delay_line", 3),    // ~3µs for ring buffer
                    ("fft_analyzer", 50), // ~50µs for 512-point FFT
                ];

                let _listeners: Vec<_> = dsp_effects
                    .into_iter()
                    .map(|(name, time_us)| {
                        let listener_concrete =
                            Arc::new(Mutex::new(AudioDSPListener::new(name, time_us)));
                        let listener: Arc<Mutex<dyn EventListener>> = listener_concrete.clone();
                        bus.subscribe("audio:buffer", listener);
                        listener_concrete
                    })
                    .collect();

                let audio_event = Event::Custom {
                    topic: "audio:buffer".to_string(),
                    data: serde_json::json!({"buffer_size": 512, "timestamp_ns": 0}),
                };

                b.iter(|| {
                    // Simulate audio callbacks
                    for _ in 0..rate {
                        bus.publish(black_box("audio:buffer"), black_box(&audio_event));
                    }
                });
            },
        );
    }
    group.finish();
}

// Benchmark: Mixed realtime workload (video + audio + input simultaneously)
fn bench_mixed_realtime_workload(c: &mut Criterion) {
    let mut group = c.benchmark_group("mixed_realtime_workload");

    group.bench_function("60fps_video_192Hz_audio_input", |b| {
        let bus = EventBus::new();

        // Video listeners
        let _video_listeners: Vec<_> = vec![("blur", 80), ("color", 15)]
            .into_iter()
            .map(|(name, time)| {
                let listener_concrete = Arc::new(Mutex::new(VideoEffectListener::new(name, time)));
                let listener: Arc<Mutex<dyn EventListener>> = listener_concrete.clone();
                bus.subscribe("video:frame", listener);
                listener_concrete
            })
            .collect();

        // Audio listeners
        let _audio_listeners: Vec<_> = vec![("eq", 8), ("comp", 5)]
            .into_iter()
            .map(|(name, time)| {
                let listener_concrete = Arc::new(Mutex::new(AudioDSPListener::new(name, time)));
                let listener: Arc<Mutex<dyn EventListener>> = listener_concrete.clone();
                bus.subscribe("audio:buffer", listener);
                listener_concrete
            })
            .collect();

        // Input listeners (fast, just counting)
        let _input_listeners: Vec<_> = (0..2)
            .map(|_| {
                let listener_concrete = Arc::new(Mutex::new(CountingListener::new()));
                let listener: Arc<Mutex<dyn EventListener>> = listener_concrete.clone();
                bus.subscribe("input:mouse", listener);
                listener_concrete
            })
            .collect();

        let video_event = Event::Custom {
            topic: "video:frame".to_string(),
            data: serde_json::json!({"frame": 0}),
        };
        let audio_event = Event::Custom {
            topic: "audio:buffer".to_string(),
            data: serde_json::json!({"buffer": 0}),
        };
        let mouse_event = Event::custom("input:mouse", serde_json::json!({"x": 100.0, "y": 100.0}));

        b.iter(|| {
            // Simulate 1 second of events
            for _ in 0..60 {
                bus.publish(black_box("video:frame"), black_box(&video_event));
            }
            for _ in 0..192 {
                bus.publish(black_box("audio:buffer"), black_box(&audio_event));
            }
            for _ in 0..100 {
                bus.publish(black_box("input:mouse"), black_box(&mouse_event));
            }
        });
    });

    group.finish();
}

// Benchmark: Latency percentiles for dispatch
fn bench_latency_percentiles(c: &mut Criterion) {
    let mut group = c.benchmark_group("latency_percentiles");
    group.sample_size(1000); // Collect enough samples for percentiles

    group.bench_function("publish_latency", |b| {
        let bus = EventBus::new();

        // Add a few listeners with realistic work
        let _listeners: Vec<_> = (0..3)
            .map(|_| {
                let listener_concrete = Arc::new(Mutex::new(VideoEffectListener::new("test", 20)));
                let listener: Arc<Mutex<dyn EventListener>> = listener_concrete.clone();
                bus.subscribe("latency-test", listener);
                listener_concrete
            })
            .collect();

        let event = Event::Custom {
            topic: "latency-test".to_string(),
            data: serde_json::json!({"test": "data"}),
        };

        b.iter(|| {
            bus.publish(black_box("latency-test"), black_box(&event));
        });
    });

    group.finish();
}

// Benchmark: Backpressure handling (overloaded listeners)
fn bench_backpressure_handling(c: &mut Criterion) {
    let mut group = c.benchmark_group("backpressure_handling");

    for num_slow_listeners in [1, 5, 10].iter() {
        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{}_slow_listeners", num_slow_listeners)),
            num_slow_listeners,
            |b, &num_slow| {
                let bus = EventBus::new();

                // Add fast listeners
                let fast_listeners: Vec<_> = (0..3)
                    .map(|_| {
                        let listener_concrete = Arc::new(Mutex::new(CountingListener::new()));
                        let listener: Arc<Mutex<dyn EventListener>> = listener_concrete.clone();
                        bus.subscribe("backpressure", listener);
                        listener_concrete
                    })
                    .collect();

                // Add slow listeners (can't keep up)
                let _slow_listeners: Vec<_> = (0..num_slow)
                    .map(|_| {
                        let listener_concrete = Arc::new(Mutex::new(SlowListener::new(5000))); // 5ms work
                        let listener: Arc<Mutex<dyn EventListener>> = listener_concrete.clone();
                        bus.subscribe("backpressure", listener);
                        listener_concrete
                    })
                    .collect();

                let event = Event::Custom {
                    topic: "backpressure".to_string(),
                    data: serde_json::json!({"test": "data"}),
                };

                b.iter(|| {
                    // Rapid-fire 100 events
                    for _ in 0..100 {
                        bus.publish(black_box("backpressure"), black_box(&event));
                    }
                });

                // Verify fast listeners still processed events
                let total_fast: usize = fast_listeners.iter().map(|l| l.lock().count()).sum();
                assert!(total_fast > 0);
            },
        );
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_publish_scaling_subscribers,
    bench_high_frequency_publishing,
    bench_topic_routing,
    bench_concurrent_publishing,
    bench_slow_listener_isolation,
    bench_event_size_impact,
    bench_subscribe_unsubscribe,
    bench_keyboard_event_burst,
    bench_realistic_video_processing,
    bench_realistic_audio_processing,
    bench_mixed_realtime_workload,
    bench_latency_percentiles,
    bench_backpressure_handling,
);

criterion_main!(benches);
