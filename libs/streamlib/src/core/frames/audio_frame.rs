use super::metadata::MetadataValue;
use super::super::ports::{PortMessage, PortType};
use std::sync::Arc;
use std::collections::HashMap;
use dasp::Frame;
use dasp::slice::{FromSampleSlice, ToSampleSlice};
use dasp::Signal;

/// Signal adapter that yields frames from an AudioFrame's interleaved buffer.
///
/// This allows AudioFrame to be consumed as a dasp Signal for DSP operations.
pub struct AudioFrameSignal<const CHANNELS: usize> {
    samples: Arc<Vec<f32>>,
    position: usize,
}

/// Generic audio frame with compile-time channel count.
///
/// Stores interleaved f32 samples: [L, R, L, R, ...] for stereo.
/// CHANNELS parameter is checked at compile time for type safety.
///
/// # Examples
///
/// ```ignore
/// // Mono: AudioFrame<1>
/// let mono = AudioFrame::<1>::new(vec![0.0; 2048], 0, 0);
///
/// // Stereo: AudioFrame<2>
/// let stereo = AudioFrame::<2>::new(vec![0.0; 2048 * 2], 0, 0);
///
/// // Quad: AudioFrame<4>
/// let quad = AudioFrame::<4>::new(vec![0.0; 2048 * 4], 0, 0);
/// ```
#[derive(Clone)]
pub struct AudioFrame<const CHANNELS: usize> {
    /// Interleaved audio samples (e.g., [L, R, L, R, ...] for stereo)
    pub samples: Arc<Vec<f32>>,
    pub timestamp_ns: i64,
    pub frame_number: u64,
    pub metadata: Option<HashMap<String, MetadataValue>>,
}

impl<const CHANNELS: usize> AudioFrame<CHANNELS> {
    /// Create a new audio frame with interleaved samples.
    ///
    /// # Panics
    /// Panics if samples.len() is not divisible by CHANNELS.
    pub fn new(
        samples: Vec<f32>,
        timestamp_ns: i64,
        frame_number: u64,
    ) -> Self {
        assert_eq!(
            samples.len() % CHANNELS,
            0,
            "samples.len() ({}) must be divisible by CHANNELS ({})",
            samples.len(),
            CHANNELS
        );

        Self {
            samples: Arc::new(samples),
            timestamp_ns,
            frame_number,
            metadata: None,
        }
    }

    /// Number of sample frames (not individual samples).
    /// For stereo with 2048 interleaved samples, returns 1024.
    pub fn sample_count(&self) -> usize {
        self.samples.len() / CHANNELS
    }

    /// Check if buffer has expected number of sample frames.
    pub fn validate_buffer_size(&self, expected_size: usize) -> bool {
        self.sample_count() == expected_size
    }

    /// Get number of channels (compile-time constant).
    pub fn channels(&self) -> usize {
        CHANNELS
    }

    /// Convert AudioFrame to a dasp Signal for DSP operations.
    ///
    /// This enables the "double read" pattern:
    /// 1. StreamInput.read() → pops AudioFrame from rtrb
    /// 2. AudioFrame.read() → returns dasp Signal for processing
    ///
    /// # Returns
    /// A Signal that yields frames of type `[f32; CHANNELS]`.
    ///
    /// # Example
    /// ```ignore
    /// if let Some(audio_frame) = self.input.read_latest() {
    ///     let signal = audio_frame.read();
    ///     // Now use dasp signal combinators:
    ///     let processed = signal.map(|frame| frame * 0.5);
    /// }
    /// ```
    pub fn read(&self) -> AudioFrameSignal<CHANNELS> {
        AudioFrameSignal {
            samples: Arc::clone(&self.samples),
            position: 0,
        }
    }

    pub fn duration(&self, sample_rate: u32) -> f64 {
        self.sample_count() as f64 / sample_rate as f64
    }

    pub fn duration_ns(&self, sample_rate: u32) -> i64 {
        (self.sample_count() as i64 * 1_000_000_000) / sample_rate as i64
    }

    pub fn timestamp_seconds(&self) -> f64 {
        self.timestamp_ns as f64 / 1_000_000_000.0
    }

    /// Convert interleaved samples to dasp frames.
    ///
    /// # Panics
    /// Panics if F::CHANNELS doesn't match CHANNELS.
    pub fn as_frames<F>(&self) -> &[F]
    where
        F: Frame<Sample = f32>,
        for<'a> &'a [F]: FromSampleSlice<'a, f32>,
    {
        assert_eq!(F::CHANNELS, CHANNELS,
            "Frame type has {} channels but AudioFrame has {} channels",
            F::CHANNELS, CHANNELS);

        // Use FromSampleSlice to convert &[f32] to &[[f32; N]]
        FromSampleSlice::from_sample_slice(&self.samples)
            .expect("Sample count must be divisible by channel count")
    }

    /// Create AudioFrame from dasp frames.
    ///
    /// # Panics
    /// Panics if F::CHANNELS doesn't match CHANNELS.
    pub fn from_frames<F>(
        frames: &[F],
        timestamp_ns: i64,
        frame_number: u64,
    ) -> Self
    where
        F: Frame<Sample = f32>,
        for<'a> &'a [F]: ToSampleSlice<'a, f32>,
    {
        assert_eq!(F::CHANNELS, CHANNELS,
            "Frame type has {} channels but AudioFrame has {} channels",
            F::CHANNELS, CHANNELS);

        // Use ToSampleSlice to convert &[[f32; N]] to &[f32]
        let sample_slice: &[f32] = frames.to_sample_slice();
        let samples = sample_slice.to_vec();
        Self::new(samples, timestamp_ns, frame_number)
    }

}

impl<const CHANNELS: usize> PortMessage for AudioFrame<CHANNELS> {
    fn port_type() -> PortType {
        match CHANNELS {
            1 => PortType::Audio1,
            2 => PortType::Audio2,
            4 => PortType::Audio4,
            6 => PortType::Audio6,
            8 => PortType::Audio8,
            _ => panic!("Unsupported channel count: {}. Use 1, 2, 4, 6, or 8 channels.", CHANNELS),
        }
    }

    fn schema() -> std::sync::Arc<crate::core::Schema> {
        std::sync::Arc::clone(&crate::core::SCHEMA_AUDIO_FRAME)
    }

    fn examples() -> Vec<(&'static str, serde_json::Value)> {
        vec![
            ("AudioFrame", serde_json::json!({
                "sample_count": 2048,
                "channels": CHANNELS,
                "timestamp_ns": 0,
                "frame_number": 1,
                "metadata": {}
            })),
        ]
    }
}

/// Implement dasp Signal trait for AudioFrameSignal.
///
/// This allows AudioFrame data to be processed using dasp's signal combinators
/// (map, add_amp, zip, etc.) for audio DSP operations.
impl<const CHANNELS: usize> Signal for AudioFrameSignal<CHANNELS>
where
    [f32; CHANNELS]: Frame<Sample = f32>,
{
    type Frame = [f32; CHANNELS];

    fn next(&mut self) -> Self::Frame {
        // Check if we've reached the end
        if self.position >= self.samples.len() {
            // Return equilibrium (silence) after buffer exhausted
            return [0.0; CHANNELS];
        }

        // Extract next frame (CHANNELS samples)
        let mut frame = [0.0; CHANNELS];
        for i in 0..CHANNELS {
            if self.position + i < self.samples.len() {
                frame[i] = self.samples[self.position + i];
            }
        }

        self.position += CHANNELS;
        frame
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_audioframe_creation() {
        let samples = vec![0.0; 480 * 2];
        let frame = AudioFrame::<2>::new(samples, 0, 0);

        assert_eq!(frame.sample_count(), 480);
        assert_eq!(frame.channels(), 2);
        assert_eq!(frame.samples.len(), 480 * 2);
    }

    #[test]
    fn test_audioframe_duration() {
        let samples = vec![0.0; 480 * 2];
        let frame = AudioFrame::<2>::new(samples, 0, 0);

        assert_eq!(frame.duration(48000), 0.01);
        assert_eq!(frame.duration_ns(48000), 10_000_000);
    }

    #[test]
    fn test_audioframe_stereo_dasp() {
        let samples = vec![
            1.0, -1.0,
            2.0, -2.0,
            3.0, -3.0,
        ];
        let frame = AudioFrame::<2>::new(samples, 0, 0);

        let frames = frame.as_frames::<[f32; 2]>();

        assert_eq!(frames.len(), 3);
        assert_eq!(frames[0], [1.0, -1.0]);
        assert_eq!(frames[1], [2.0, -2.0]);
        assert_eq!(frames[2], [3.0, -3.0]);
    }

    #[test]
    fn test_audioframe_timestamp_conversion() {
        let samples = vec![0.0; 480 * 2];
        let frame = AudioFrame::<2>::new(samples, 1_500_000_000, 0);

        assert_eq!(frame.timestamp_seconds(), 1.5);
    }

    #[test]
    #[should_panic(expected = "samples.len()")]
    fn test_audioframe_invalid_sample_count() {
        let samples = vec![0.0; 5];
        AudioFrame::<2>::new(samples, 0, 0);
    }

    #[test]
    fn test_audioframe_from_frames() {
        let dasp_frames: &[[f32; 2]] = &[
            [1.0, -1.0],
            [2.0, -2.0],
            [3.0, -3.0],
        ];

        let frame = AudioFrame::<2>::from_frames(dasp_frames, 0, 0);

        assert_eq!(frame.channels(), 2);
        assert_eq!(frame.sample_count(), 3);
        assert_eq!(&*frame.samples, &[1.0, -1.0, 2.0, -2.0, 3.0, -3.0]);
    }

    #[test]
    fn test_audioframe_validate_buffer_size() {
        let samples = vec![0.0; 512 * 2];
        let frame = AudioFrame::<2>::new(samples, 0, 0);

        assert!(frame.validate_buffer_size(512));
        assert!(!frame.validate_buffer_size(1024));
    }

    #[test]
    fn test_audioframe_mono() {
        let samples = vec![1.0, 2.0, 3.0];
        let frame = AudioFrame::<1>::new(samples, 0, 0);

        assert_eq!(frame.channels(), 1);
        assert_eq!(frame.sample_count(), 3);
    }

    #[test]
    fn test_audioframe_quad() {
        let samples = vec![0.0; 512 * 4];
        let frame = AudioFrame::<4>::new(samples, 0, 0);

        assert_eq!(frame.channels(), 4);
        assert_eq!(frame.sample_count(), 512);
    }

    #[test]
    fn test_audioframe_read_signal() {
        let samples = vec![
            1.0, 2.0,  // Frame 0
            3.0, 4.0,  // Frame 1
            5.0, 6.0,  // Frame 2
        ];
        let frame = AudioFrame::<2>::new(samples, 0, 0);

        // Read as dasp signal
        let mut signal = frame.read();

        assert_eq!(signal.next(), [1.0, 2.0]);
        assert_eq!(signal.next(), [3.0, 4.0]);
        assert_eq!(signal.next(), [5.0, 6.0]);

        // After exhausting buffer, returns silence
        assert_eq!(signal.next(), [0.0, 0.0]);
        assert_eq!(signal.next(), [0.0, 0.0]);
    }

    #[test]
    fn test_audioframe_read_signal_mono() {
        let samples = vec![1.0, 2.0, 3.0];
        let frame = AudioFrame::<1>::new(samples, 0, 0);

        let mut signal = frame.read();

        assert_eq!(signal.next(), [1.0]);
        assert_eq!(signal.next(), [2.0]);
        assert_eq!(signal.next(), [3.0]);
        assert_eq!(signal.next(), [0.0]);
    }
}
