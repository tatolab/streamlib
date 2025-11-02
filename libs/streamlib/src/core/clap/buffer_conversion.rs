//! Audio buffer conversion utilities for CLAP plugin integration
//!
//! CLAP plugins expect non-interleaved audio (separate channel buffers),
//! while streamlib AudioFrame uses interleaved samples. These utilities
//! handle the conversion.
//!
//! **INTERNAL USE ONLY** - Not exposed in public API

use crate::core::AudioFrame;
use dasp::{Frame, frame::Stereo};

/// Convert interleaved AudioFrame to separate channel buffers for CLAP
///
/// AudioFrame stores samples interleaved (LRLRLR...), but CLAP expects
/// separate channel buffers (LLL... RRR...).
///
/// # Arguments
///
/// * `frame` - Input audio frame with interleaved samples
///
/// # Returns
///
/// Tuple of (left_channel, right_channel) as separate Vec<f32>
///
/// # Panics
///
/// Panics if frame is not stereo (2 channels)
pub(crate) fn deinterleave_audio_frame(frame: &AudioFrame) -> (Vec<f32>, Vec<f32>) {
    assert_eq!(frame.channels, 2, "Only stereo audio supported for CLAP plugins");

    let samples = &frame.samples;
    let num_samples = frame.sample_count();

    let mut left = Vec::with_capacity(num_samples);
    let mut right = Vec::with_capacity(num_samples);

    // Use dasp to safely deinterleave stereo frames
    for stereo_frame in samples.chunks_exact(2) {
        let frame = Stereo::<f32>::from_fn(|ch| stereo_frame[ch]);
        left.push(frame.channels()[0]);
        right.push(frame.channels()[1]);
    }

    (left, right)
}

/// Convert separate channel buffers from CLAP to interleaved AudioFrame
///
/// CLAP returns separate channel buffers (LLL... RRR...), but AudioFrame
/// stores samples interleaved (LRLRLR...).
///
/// # Arguments
///
/// * `left` - Left channel samples
/// * `right` - Right channel samples
/// * `timestamp_ns` - Timestamp in nanoseconds
/// * `frame_number` - Frame number
///
/// # Returns
///
/// AudioFrame with interleaved stereo data
///
/// # Panics
///
/// Panics if left and right have different lengths
pub(crate) fn interleave_to_audio_frame(
    left: &[f32],
    right: &[f32],
    timestamp_ns: i64,
    frame_number: u64,
) -> AudioFrame {
    assert_eq!(left.len(), right.len(), "Channel buffers must have same length");

    let num_samples = left.len();
    let mut samples = Vec::with_capacity(num_samples * 2);

    // Use dasp to safely interleave stereo frames
    for i in 0..num_samples {
        let frame = Stereo::<f32>::from_fn(|ch| if ch == 0 { left[i] } else { right[i] });
        samples.extend_from_slice(frame.channels());
    }

    AudioFrame::new(samples, timestamp_ns, frame_number, 2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deinterleave_audio_frame() {
        // Create test frame: 4 samples stereo (L1 R1 L2 R2 L3 R3 L4 R4)
        let frame = AudioFrame::new(
            vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0],
            0,      // timestamp_ns
            0,      // frame_number
            2,      // channels
        );

        let (left, right) = deinterleave_audio_frame(&frame);

        assert_eq!(left.len(), 4);
        assert_eq!(right.len(), 4);
        assert_eq!(left, vec![1.0, 3.0, 5.0, 7.0]);
        assert_eq!(right, vec![2.0, 4.0, 6.0, 8.0]);
    }

    #[test]
    fn test_interleave_to_audio_frame() {
        // Create test CLAP buffers (separate L/R channels)
        let left: Vec<f32> = vec![1.0, 3.0, 5.0, 7.0];
        let right: Vec<f32> = vec![2.0, 4.0, 6.0, 8.0];

        let frame = interleave_to_audio_frame(&left, &right, 1000, 1);

        assert_eq!(frame.channels, 2);
        assert_eq!(frame.samples.len(), 8);
        assert_eq!(*frame.samples, vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0]);
    }

    #[test]
    fn test_roundtrip_conversion() {
        // Test that deinterleave → interleave is lossless
        let original = AudioFrame::new(
            vec![0.1, 0.2, 0.3, 0.4, 0.5, 0.6],
            5000,   // timestamp_ns
            42,     // frame_number
            2,      // channels
        );

        let (left, right) = deinterleave_audio_frame(&original);
        let roundtrip = interleave_to_audio_frame(
            &left,
            &right,
            original.timestamp_ns,
            original.frame_number,
        );

        assert_eq!(*original.samples, *roundtrip.samples);
        assert_eq!(original.timestamp_ns, roundtrip.timestamp_ns);
        assert_eq!(original.frame_number, roundtrip.frame_number);
    }
}
