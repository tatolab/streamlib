use super::metadata::MetadataValue;
use super::super::ports::{PortMessage, PortType};
use std::sync::Arc;
use std::collections::HashMap;
use dasp::Frame;
use dasp::slice::{FromSampleSlice, ToSampleSlice};

#[derive(Clone)]
pub struct AudioFrame {
    pub samples: Arc<Vec<f32>>,
    pub channels: u32,
    pub timestamp_ns: i64,
    pub frame_number: u64,
    pub metadata: Option<HashMap<String, MetadataValue>>,
}

impl AudioFrame {
    pub fn validate_buffer_size(&self, expected_size: usize) -> bool {
        self.sample_count() == expected_size
    }
}

impl AudioFrame {
    pub fn new(
        samples: Vec<f32>,
        timestamp_ns: i64,
        frame_number: u64,
        channels: u32,
    ) -> Self {
        assert_eq!(
            samples.len() % channels as usize,
            0,
            "samples.len() must be divisible by channels"
        );

        Self {
            samples: Arc::new(samples),
            channels,
            timestamp_ns,
            frame_number,
            metadata: None,
        }
    }

    pub fn sample_count(&self) -> usize {
        self.samples.len() / self.channels as usize
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

    pub fn as_frames<F>(&self) -> &[F]
    where
        F: Frame<Sample = f32>,
        for<'a> &'a [F]: FromSampleSlice<'a, f32>,
    {
        assert_eq!(F::CHANNELS, self.channels as usize,
            "Frame type has {} channels but AudioFrame has {} channels",
            F::CHANNELS, self.channels);

        // Use FromSampleSlice to convert &[f32] to &[[f32; N]]
        FromSampleSlice::from_sample_slice(&self.samples)
            .expect("Sample count must be divisible by channel count")
    }

    pub fn from_frames<F>(
        frames: &[F],
        timestamp_ns: i64,
        frame_number: u64,
    ) -> Self
    where
        F: Frame<Sample = f32>,
        for<'a> &'a [F]: ToSampleSlice<'a, f32>,
    {
        // Use ToSampleSlice to convert &[[f32; N]] to &[f32]
        let sample_slice: &[f32] = frames.to_sample_slice();
        let samples = sample_slice.to_vec();
        Self::new(samples, timestamp_ns, frame_number, F::CHANNELS as u32)
    }

    pub fn example_stereo() -> serde_json::Value {
        serde_json::json!({
            "sample_count": 2048,
            "channels": 2,
            "timestamp_ns": 0,
            "frame_number": 1,
            "metadata": {}
        })
    }

    pub fn example_mono() -> serde_json::Value {
        serde_json::json!({
            "sample_count": 2048,
            "channels": 1,
            "timestamp_ns": 0,
            "frame_number": 1,
            "metadata": {}
        })
    }
}

impl PortMessage for AudioFrame {
    fn port_type() -> PortType {
        PortType::Audio
    }

    fn schema() -> std::sync::Arc<crate::core::Schema> {
        std::sync::Arc::clone(&crate::core::SCHEMA_AUDIO_FRAME)
    }

    fn examples() -> Vec<(&'static str, serde_json::Value)> {
        vec![
            ("Stereo", Self::example_stereo()),
            ("Mono", Self::example_mono()),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_audioframe_creation() {
        let samples = vec![0.0; 480 * 2];
        let frame = AudioFrame::new(samples, 0, 0, 2);

        assert_eq!(frame.sample_count(), 480);
        assert_eq!(frame.channels, 2);
        assert_eq!(frame.samples.len(), 480 * 2);
    }

    #[test]
    fn test_audioframe_duration() {
        let samples = vec![0.0; 480 * 2];
        let frame = AudioFrame::new(samples, 0, 0, 2);

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
        let frame = AudioFrame::new(samples, 0, 0, 2);

        let frames = frame.as_frames::<[f32; 2]>();

        assert_eq!(frames.len(), 3);
        assert_eq!(frames[0], [1.0, -1.0]);
        assert_eq!(frames[1], [2.0, -2.0]);
        assert_eq!(frames[2], [3.0, -3.0]);
    }

    #[test]
    fn test_audioframe_timestamp_conversion() {
        let samples = vec![0.0; 480 * 2];
        let frame = AudioFrame::new(samples, 1_500_000_000, 0, 2);

        assert_eq!(frame.timestamp_seconds(), 1.5);
    }

    #[test]
    #[should_panic(expected = "samples.len() must be divisible by channels")]
    fn test_audioframe_invalid_sample_count() {
        let samples = vec![0.0; 5];
        AudioFrame::new(samples, 0, 0, 2);
    }

    #[test]
    fn test_audioframe_from_frames() {
        let dasp_frames: &[[f32; 2]] = &[
            [1.0, -1.0],
            [2.0, -2.0],
            [3.0, -3.0],
        ];

        let frame = AudioFrame::from_frames(dasp_frames, 0, 0);

        assert_eq!(frame.channels, 2);
        assert_eq!(frame.sample_count(), 3);
        assert_eq!(&*frame.samples, &[1.0, -1.0, 2.0, -2.0, 3.0, -3.0]);
    }

    #[test]
    fn test_audioframe_validate_buffer_size() {
        let samples = vec![0.0; 512 * 2];
        let frame = AudioFrame::new(samples, 0, 0, 2);

        assert!(frame.validate_buffer_size(512));
        assert!(!frame.validate_buffer_size(1024));
    }
}
