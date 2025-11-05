use super::metadata::MetadataValue;
use super::super::ports::{PortMessage, PortType};
use std::sync::Arc;
use std::collections::HashMap;
use dasp::Frame;
use dasp::slice::{FromSampleSlice, ToSampleSlice};
use dasp::Signal;

pub struct AudioFrameSignal<const CHANNELS: usize> {
    samples: Arc<Vec<f32>>,
    position: usize,
}

#[derive(Clone)]
pub struct AudioFrame<const CHANNELS: usize> {
    pub samples: Arc<Vec<f32>>,
    pub timestamp_ns: i64,
    pub frame_number: u64,
    pub metadata: Option<HashMap<String, MetadataValue>>,
}

impl<const CHANNELS: usize> AudioFrame<CHANNELS> {
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

    pub fn sample_count(&self) -> usize {
        self.samples.len() / CHANNELS
    }

    pub fn validate_buffer_size(&self, expected_size: usize) -> bool {
        self.sample_count() == expected_size
    }

    pub fn channels(&self) -> usize {
        CHANNELS
    }

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

    pub fn as_frames<F>(&self) -> &[F]
    where
        F: Frame<Sample = f32>,
        for<'a> &'a [F]: FromSampleSlice<'a, f32>,
    {
        assert_eq!(F::CHANNELS, CHANNELS,
            "Frame type has {} channels but AudioFrame has {} channels",
            F::CHANNELS, CHANNELS);

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
        assert_eq!(F::CHANNELS, CHANNELS,
            "Frame type has {} channels but AudioFrame has {} channels",
            F::CHANNELS, CHANNELS);

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

impl<const CHANNELS: usize> Signal for AudioFrameSignal<CHANNELS>
where
    [f32; CHANNELS]: Frame<Sample = f32>,
{
    type Frame = [f32; CHANNELS];

    fn next(&mut self) -> Self::Frame {
        if self.position >= self.samples.len() {
            return [0.0; CHANNELS];
        }

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

        let mut signal = frame.read();

        assert_eq!(signal.next(), [1.0, 2.0]);
        assert_eq!(signal.next(), [3.0, 4.0]);
        assert_eq!(signal.next(), [5.0, 6.0]);

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
