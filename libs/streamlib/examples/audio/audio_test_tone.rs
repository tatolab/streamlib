//! Simple audio test - plays a 440Hz tone through the default speaker
//!
//! This example demonstrates:
//! - Creating a TestToneGenerator
//! - Creating an AudioOutputProcessor (speaker)
//! - Manually pushing audio frames (without runtime integration)
//!
//! Run with: cargo run --example simple_audio_test

use streamlib::{
    TestToneGenerator,
    apple::AppleAudioOutputProcessor,
};
use streamlib::core::processors::AudioOutputProcessor; // For trait methods
use std::thread;
use std::time::Duration;

fn main() -> anyhow::Result<()> {
    // Initialize logging
    tracing_subscriber::fmt::init();

    println!("🔊 Simple Audio Test - Playing 440Hz tone for 3 seconds");
    println!();

    // Create test tone generator (440Hz, 48kHz, stereo)
    let mut tone_gen = TestToneGenerator::new(440.0, 48000, 2);
    tone_gen.set_amplitude(0.3); // 30% volume to avoid clipping
    tone_gen.set_samples_per_frame(2048); // ~42ms frames at 48kHz (more buffer)

    println!("✅ Created test tone generator (440Hz A4 note)");

    // Create audio output processor using default device
    let mut speaker = AppleAudioOutputProcessor::new(None)?;
    let device = speaker.current_device();
    println!("✅ Created audio output: {}", device.name);
    println!("   Sample rate: {}Hz, Channels: {}", device.sample_rate, device.channels);
    println!();

    // Pre-fill buffer to avoid initial gaps
    println!("🎵 Pre-filling audio buffer...");
    for i in 0..5 {
        let timestamp_ns = (i as i64) * 42_666_667; // ~23.4 FPS (matching ~42ms frames)
        let frame = tone_gen.generate_frame(timestamp_ns);
        speaker.push_frame(&frame)?;
    }

    // Play tone for 3 seconds
    println!("🎵 Playing tone...");
    let frame_duration_ms = 40; // ~40ms per frame (slightly less than 42ms for overlap)
    let total_frames = (3000 / frame_duration_ms) as usize; // 3 seconds

    for i in 5..total_frames {
        // Generate audio frame
        let timestamp_ns = (i as i64) * 42_666_667;
        let frame = tone_gen.generate_frame(timestamp_ns);

        // Push to speaker
        speaker.push_frame(&frame)?;

        // Wait for next frame (slightly faster than generation to keep buffer full)
        thread::sleep(Duration::from_millis(frame_duration_ms));

        // Print progress
        if i % 25 == 0 {
            let seconds = (i * frame_duration_ms as usize) / 1000;
            println!("   Playing... {}s (buffer: {:.0}%)", seconds, speaker.buffer_level() * 100.0);
        }
    }

    println!();
    println!("✅ Done! Waiting for audio to finish playing...");

    // Wait a bit for buffered audio to finish
    thread::sleep(Duration::from_millis(500));

    println!("🎵 Audio test complete!");

    Ok(())
}
