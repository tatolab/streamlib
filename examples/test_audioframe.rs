//! Simple test to validate AudioFrame implementation
//!
//! This example creates audio frames and validates the API without
//! requiring GPU or full runtime setup.

use streamlib::{AudioFrame, AudioFormat};

fn main() {
    println!("Testing AudioFrame implementation...\n");

    // Test 1: Create a simple stereo frame
    println!("Test 1: Creating stereo audio frame (48kHz, 10ms)");
    let samples = vec![0.0; 480 * 2]; // 480 samples per channel, 2 channels
    let frame = AudioFrame::new(samples, 0, 0, 48000, 2);

    assert_eq!(frame.sample_count, 480);
    assert_eq!(frame.channels, 2);
    assert_eq!(frame.sample_rate, 48000);
    assert_eq!(frame.format, AudioFormat::F32);
    assert_eq!(frame.duration(), 0.01); // 10ms
    assert_eq!(frame.duration_ns(), 10_000_000); // 10ms in nanoseconds
    println!("  ✓ Frame created: {} samples, {} channels, {} Hz",
             frame.sample_count, frame.channels, frame.sample_rate);
    println!("  ✓ Duration: {:.3}s ({} ns)\n", frame.duration(), frame.duration_ns());

    // Test 2: Create frame with distinct L/R channels
    println!("Test 2: Channel extraction");
    let samples = vec![
        1.0, -1.0,  // Sample 0: L=1.0, R=-1.0
        2.0, -2.0,  // Sample 1: L=2.0, R=-2.0
        3.0, -3.0,  // Sample 2: L=3.0, R=-3.0
    ];
    let frame = AudioFrame::new(samples, 0, 1, 48000, 2);

    let left = frame.channel_samples(0);
    let right = frame.channel_samples(1);

    assert_eq!(left, vec![1.0, 2.0, 3.0]);
    assert_eq!(right, vec![-1.0, -2.0, -3.0]);
    println!("  ✓ Left channel:  {:?}", left);
    println!("  ✓ Right channel: {:?}\n", right);

    // Test 3: Timestamp conversion
    println!("Test 3: Timestamp conversion");
    let samples = vec![0.0; 480 * 2];
    let frame = AudioFrame::new(samples, 1_500_000_000, 2, 48000, 2); // 1.5 seconds

    assert_eq!(frame.timestamp_ns, 1_500_000_000);
    assert_eq!(frame.timestamp_seconds(), 1.5);
    println!("  ✓ Timestamp: {} ns = {:.3}s\n", frame.timestamp_ns, frame.timestamp_seconds());

    // Test 4: Mono audio
    println!("Test 4: Mono audio (voice)");
    let samples = vec![0.0; 160]; // 10ms at 16kHz (voice quality)
    let frame = AudioFrame::new(samples, 0, 3, 16000, 1);

    assert_eq!(frame.sample_count, 160);
    assert_eq!(frame.channels, 1);
    assert_eq!(frame.sample_rate, 16000);
    assert_eq!(frame.duration(), 0.01); // Still 10ms
    println!("  ✓ Mono frame: {} samples at {} Hz", frame.sample_count, frame.sample_rate);
    println!("  ✓ Duration: {:.3}s\n", frame.duration());

    // Test 5: Frame numbering (for A/V sync)
    println!("Test 5: Frame numbering");
    for i in 0..5 {
        let samples = vec![0.0; 480 * 2];
        let frame = AudioFrame::new(samples, i * 10_000_000, i, 48000, 2);
        assert_eq!(frame.frame_number, i);
        println!("  ✓ Frame #{}: timestamp = {} ns", frame.frame_number, frame.timestamp_ns);
    }

    println!("\n✅ All AudioFrame tests passed!");
}
