use super::metadata::MetadataValue;
use crate::core::links::{LinkPortMessage, LinkPortType};
use dasp::Frame;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

// Implement sealed trait for AudioFrame
impl crate::core::links::traits::link_port_message::sealed::Sealed for AudioFrame {}
use dasp::slice::{FromSampleSlice, ToSampleSlice};
use dasp::Signal;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum AudioChannelCount {
    One = 1,
    Two = 2,
    Three = 3,
    Four = 4,
    Five = 5,
    Six = 6,
    Seven = 7,
    Eight = 8,
}

impl AudioChannelCount {
    pub fn as_usize(self) -> usize {
        self as u8 as usize
    }

    pub fn from_usize(channels: usize) -> Option<Self> {
        match channels {
            1 => Some(Self::One),
            2 => Some(Self::Two),
            3 => Some(Self::Three),
            4 => Some(Self::Four),
            5 => Some(Self::Five),
            6 => Some(Self::Six),
            7 => Some(Self::Seven),
            8 => Some(Self::Eight),
            _ => None,
        }
    }
}

pub struct AudioFrameSignal {
    samples: Arc<Vec<f32>>,
    channels: AudioChannelCount,
    position: usize,
}

#[derive(Clone)]
pub struct AudioFrame {
    pub samples: Arc<Vec<f32>>,
    pub channels: AudioChannelCount,
    pub timestamp_ns: i64,
    pub frame_number: u64,
    pub sample_rate: u32,
    pub metadata: Option<HashMap<String, MetadataValue>>,
}

impl AudioFrame {
    pub fn new(
        samples: Vec<f32>,
        channels: AudioChannelCount,
        timestamp_ns: i64,
        frame_number: u64,
        sample_rate: u32,
    ) -> Self {
        let channels_usize = channels.as_usize();
        assert_eq!(
            samples.len() % channels_usize,
            0,
            "samples.len() ({}) must be divisible by channels ({})",
            samples.len(),
            channels_usize
        );

        Self {
            samples: Arc::new(samples),
            channels,
            timestamp_ns,
            frame_number,
            sample_rate,
            metadata: None,
        }
    }

    pub fn sample_count(&self) -> usize {
        self.samples.len() / self.channels.as_usize()
    }

    pub fn validate_buffer_size(&self, expected_size: usize) -> bool {
        self.sample_count() == expected_size
    }

    pub fn channels(&self) -> usize {
        self.channels.as_usize()
    }

    pub fn read(&self) -> AudioFrameSignal {
        AudioFrameSignal {
            samples: Arc::clone(&self.samples),
            channels: self.channels,
            position: 0,
        }
    }

    pub fn duration(&self) -> f64 {
        self.sample_count() as f64 / self.sample_rate as f64
    }

    pub fn duration_ns(&self) -> i64 {
        (self.sample_count() as i64 * 1_000_000_000) / self.sample_rate as i64
    }

    pub fn timestamp_seconds(&self) -> f64 {
        self.timestamp_ns as f64 / 1_000_000_000.0
    }

    pub fn as_frames<F>(&self) -> &[F]
    where
        F: Frame<Sample = f32>,
        for<'a> &'a [F]: FromSampleSlice<'a, f32>,
    {
        assert_eq!(
            F::CHANNELS,
            self.channels.as_usize(),
            "Frame type has {} channels but AudioFrame has {} channels",
            F::CHANNELS,
            self.channels.as_usize()
        );

        FromSampleSlice::from_sample_slice(&self.samples)
            .expect("Sample count must be divisible by channel count")
    }

    pub fn from_frames<F>(
        frames: &[F],
        channels: AudioChannelCount,
        timestamp_ns: i64,
        frame_number: u64,
        sample_rate: u32,
    ) -> Self
    where
        F: Frame<Sample = f32>,
        for<'a> &'a [F]: ToSampleSlice<'a, f32>,
    {
        assert_eq!(
            F::CHANNELS,
            channels.as_usize(),
            "Frame type has {} channels but AudioFrame has {} channels",
            F::CHANNELS,
            channels.as_usize()
        );

        let sample_slice: &[f32] = frames.to_sample_slice();
        let samples = sample_slice.to_vec();
        Self::new(samples, channels, timestamp_ns, frame_number, sample_rate)
    }
}

impl LinkPortMessage for AudioFrame {
    fn port_type() -> LinkPortType {
        LinkPortType::Audio
    }

    fn schema() -> std::sync::Arc<crate::core::Schema> {
        std::sync::Arc::clone(&crate::core::SCHEMA_AUDIO_FRAME)
    }

    fn examples() -> Vec<(&'static str, serde_json::Value)> {
        vec![(
            "AudioFrame",
            serde_json::json!({
                "sample_count": 2048,
                "channels": 2,
                "timestamp_ns": 0,
                "frame_number": 1,
                "metadata": {}
            }),
        )]
    }

    fn link_read_behavior() -> crate::core::links::LinkBufferReadMode {
        // Audio frames must be read in order to avoid audio dropouts/glitches
        crate::core::links::LinkBufferReadMode::ReadNextInOrder
    }
}

// Enum for dynamic frame types - zero-copy enum dispatch
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DynamicFrame {
    One([f32; 1]),
    Two([f32; 2]),
    Three([f32; 3]),
    Four([f32; 4]),
    Five([f32; 5]),
    Six([f32; 6]),
    Seven([f32; 7]),
    Eight([f32; 8]),
}

impl DynamicFrame {
    pub fn as_slice(&self) -> &[f32] {
        match self {
            DynamicFrame::One(f) => f,
            DynamicFrame::Two(f) => f,
            DynamicFrame::Three(f) => f,
            DynamicFrame::Four(f) => f,
            DynamicFrame::Five(f) => f,
            DynamicFrame::Six(f) => f,
            DynamicFrame::Seven(f) => f,
            DynamicFrame::Eight(f) => f,
        }
    }

    pub fn channels(&self) -> usize {
        match self {
            DynamicFrame::One(_) => 1,
            DynamicFrame::Two(_) => 2,
            DynamicFrame::Three(_) => 3,
            DynamicFrame::Four(_) => 4,
            DynamicFrame::Five(_) => 5,
            DynamicFrame::Six(_) => 6,
            DynamicFrame::Seven(_) => 7,
            DynamicFrame::Eight(_) => 8,
        }
    }
}

// Iterator for DynamicFrame channels to satisfy dasp::Frame trait
#[derive(Debug, Clone)]
pub struct DynamicChannelIterator {
    samples: DynamicFrame,
    position: usize,
}

impl Iterator for DynamicChannelIterator {
    type Item = f32;

    fn next(&mut self) -> Option<Self::Item> {
        let slice = self.samples.as_slice();
        if self.position < slice.len() {
            let sample = slice[self.position];
            self.position += 1;
            Some(sample)
        } else {
            None
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.samples.as_slice().len() - self.position;
        (remaining, Some(remaining))
    }
}

impl ExactSizeIterator for DynamicChannelIterator {
    fn len(&self) -> usize {
        self.samples.as_slice().len() - self.position
    }
}

// Implement dasp::Frame trait for DynamicFrame to support Signal usage
// This allows DynamicFrame to be used with dasp's audio processing utilities
// Note: Many methods are stubs since our code only uses the Signal trait, not Frame methods directly
impl dasp::Frame for DynamicFrame {
    type Sample = f32;
    type Channels = DynamicChannelIterator;
    type NumChannels = dasp::frame::N8; // Max channels we support (8 channels)
    type Signed = DynamicFrame; // Already using f32 (signed)
    type Float = DynamicFrame; // Already using f32 (float)

    const CHANNELS: usize = 8;
    const EQUILIBRIUM: Self = DynamicFrame::One([0.0]);

    fn channels(self) -> Self::Channels {
        DynamicChannelIterator {
            samples: self,
            position: 0,
        }
    }

    fn from_fn<F>(mut func: F) -> Self
    where
        F: FnMut(usize) -> Self::Sample,
    {
        DynamicFrame::One([func(0)])
    }

    fn from_samples<I>(samples: &mut I) -> Option<Self>
    where
        I: Iterator<Item = Self::Sample>,
    {
        samples.next().map(|s| DynamicFrame::One([s]))
    }

    fn channel(&self, idx: usize) -> Option<&Self::Sample> {
        self.as_slice().get(idx)
    }

    unsafe fn channel_unchecked(&self, idx: usize) -> &Self::Sample {
        self.as_slice().get_unchecked(idx)
    }

    fn map<F, M>(self, mut map: M) -> F
    where
        F: dasp::Frame<NumChannels = Self::NumChannels>,
        M: FnMut(Self::Sample) -> F::Sample,
    {
        F::from_fn(|i| {
            if i < self.as_slice().len() {
                map(self.as_slice()[i])
            } else {
                // Return equilibrium value via map function applied to 0.0
                map(0.0)
            }
        })
    }

    fn zip_map<O, F, M>(self, other: O, mut zip_map: M) -> F
    where
        O: dasp::Frame<NumChannels = Self::NumChannels>,
        F: dasp::Frame<NumChannels = Self::NumChannels>,
        M: FnMut(Self::Sample, O::Sample) -> F::Sample,
    {
        F::from_fn(|i| {
            let a = self.channel(i).copied().unwrap_or(0.0);
            // For O::Sample we need to construct equilibrium - use default construction via map
            let b = other.channel(i).copied().unwrap_or_else(|| unsafe {
                // Safety: We're just creating a zero-initialized sample which is valid for audio samples
                std::mem::zeroed()
            });
            zip_map(a, b)
        })
    }

    fn to_signed_frame(self) -> Self::Signed {
        self // Already signed (f32)
    }

    fn to_float_frame(self) -> Self::Float {
        self // Already float (f32)
    }
}

// Enum dispatch for dasp Signal trait - zero-copy implementation
impl Signal for AudioFrameSignal {
    type Frame = DynamicFrame;

    fn next(&mut self) -> Self::Frame {
        let channels = self.channels.as_usize();

        if self.position >= self.samples.len() {
            return match self.channels {
                AudioChannelCount::One => DynamicFrame::One([0.0]),
                AudioChannelCount::Two => DynamicFrame::Two([0.0; 2]),
                AudioChannelCount::Three => DynamicFrame::Three([0.0; 3]),
                AudioChannelCount::Four => DynamicFrame::Four([0.0; 4]),
                AudioChannelCount::Five => DynamicFrame::Five([0.0; 5]),
                AudioChannelCount::Six => DynamicFrame::Six([0.0; 6]),
                AudioChannelCount::Seven => DynamicFrame::Seven([0.0; 7]),
                AudioChannelCount::Eight => DynamicFrame::Eight([0.0; 8]),
            };
        }

        #[allow(clippy::needless_range_loop)]
        let frame = match self.channels {
            AudioChannelCount::One => {
                let mut f = [0.0; 1];
                for i in 0..1 {
                    if self.position + i < self.samples.len() {
                        f[i] = self.samples[self.position + i];
                    }
                }
                DynamicFrame::One(f)
            }
            AudioChannelCount::Two => {
                let mut f = [0.0; 2];
                for i in 0..2 {
                    if self.position + i < self.samples.len() {
                        f[i] = self.samples[self.position + i];
                    }
                }
                DynamicFrame::Two(f)
            }
            AudioChannelCount::Three => {
                let mut f = [0.0; 3];
                for i in 0..3 {
                    if self.position + i < self.samples.len() {
                        f[i] = self.samples[self.position + i];
                    }
                }
                DynamicFrame::Three(f)
            }
            AudioChannelCount::Four => {
                let mut f = [0.0; 4];
                for i in 0..4 {
                    if self.position + i < self.samples.len() {
                        f[i] = self.samples[self.position + i];
                    }
                }
                DynamicFrame::Four(f)
            }
            AudioChannelCount::Five => {
                let mut f = [0.0; 5];
                for i in 0..5 {
                    if self.position + i < self.samples.len() {
                        f[i] = self.samples[self.position + i];
                    }
                }
                DynamicFrame::Five(f)
            }
            AudioChannelCount::Six => {
                let mut f = [0.0; 6];
                for i in 0..6 {
                    if self.position + i < self.samples.len() {
                        f[i] = self.samples[self.position + i];
                    }
                }
                DynamicFrame::Six(f)
            }
            AudioChannelCount::Seven => {
                let mut f = [0.0; 7];
                for i in 0..7 {
                    if self.position + i < self.samples.len() {
                        f[i] = self.samples[self.position + i];
                    }
                }
                DynamicFrame::Seven(f)
            }
            AudioChannelCount::Eight => {
                let mut f = [0.0; 8];
                for i in 0..8 {
                    if self.position + i < self.samples.len() {
                        f[i] = self.samples[self.position + i];
                    }
                }
                DynamicFrame::Eight(f)
            }
        };

        self.position += channels;
        frame
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_audioframe_creation() {
        let samples = vec![0.0; 480 * 2];
        let frame = AudioFrame::new(samples, AudioChannelCount::Two, 0, 0, 48000);

        assert_eq!(frame.sample_count(), 480);
        assert_eq!(frame.channels(), 2);
        assert_eq!(frame.samples.len(), 480 * 2);
    }

    #[test]
    fn test_audioframe_duration() {
        let samples = vec![0.0; 480 * 2];
        let frame = AudioFrame::new(samples, AudioChannelCount::Two, 0, 0, 48000);

        assert_eq!(frame.duration(), 0.01);
        assert_eq!(frame.duration_ns(), 10_000_000);
    }

    #[test]
    fn test_audioframe_stereo_dasp() {
        let samples = vec![1.0, -1.0, 2.0, -2.0, 3.0, -3.0];
        let frame = AudioFrame::new(samples, AudioChannelCount::Two, 0, 0, 48000);

        let frames = frame.as_frames::<[f32; 2]>();

        assert_eq!(frames.len(), 3);
        assert_eq!(frames[0], [1.0, -1.0]);
        assert_eq!(frames[1], [2.0, -2.0]);
        assert_eq!(frames[2], [3.0, -3.0]);
    }

    #[test]
    fn test_audioframe_timestamp_conversion() {
        let samples = vec![0.0; 480 * 2];
        let frame = AudioFrame::new(samples, AudioChannelCount::Two, 1_500_000_000, 0, 48000);

        assert_eq!(frame.timestamp_seconds(), 1.5);
    }

    #[test]
    #[should_panic(expected = "samples.len()")]
    fn test_audioframe_invalid_sample_count() {
        let samples = vec![0.0; 5];
        AudioFrame::new(samples, AudioChannelCount::Two, 0, 0, 48000);
    }

    #[test]
    fn test_audioframe_from_frames() {
        let dasp_frames: &[[f32; 2]] = &[[1.0, -1.0], [2.0, -2.0], [3.0, -3.0]];

        let frame = AudioFrame::from_frames(dasp_frames, AudioChannelCount::Two, 0, 0, 48000);

        assert_eq!(frame.channels(), 2);
        assert_eq!(frame.sample_count(), 3);
        assert_eq!(&*frame.samples, &[1.0, -1.0, 2.0, -2.0, 3.0, -3.0]);
    }

    #[test]
    fn test_audioframe_validate_buffer_size() {
        let samples = vec![0.0; 512 * 2];
        let frame = AudioFrame::new(samples, AudioChannelCount::Two, 0, 0, 48000);

        assert!(frame.validate_buffer_size(512));
        assert!(!frame.validate_buffer_size(1024));
    }

    #[test]
    fn test_audioframe_mono() {
        let samples = vec![1.0, 2.0, 3.0];
        let frame = AudioFrame::new(samples, AudioChannelCount::One, 0, 0, 48000);

        assert_eq!(frame.channels(), 1);
        assert_eq!(frame.sample_count(), 3);
    }

    #[test]
    fn test_audioframe_quad() {
        let samples = vec![0.0; 512 * 4];
        let frame = AudioFrame::new(samples, AudioChannelCount::Four, 0, 0, 48000);

        assert_eq!(frame.channels(), 4);
        assert_eq!(frame.sample_count(), 512);
    }

    #[test]
    fn test_audioframe_read_signal() {
        let samples = vec![
            1.0, 2.0, // Frame 0
            3.0, 4.0, // Frame 1
            5.0, 6.0, // Frame 2
        ];
        let frame = AudioFrame::new(samples, AudioChannelCount::Two, 0, 0, 48000);

        let mut signal = frame.read();

        assert_eq!(signal.next(), DynamicFrame::Two([1.0, 2.0]));
        assert_eq!(signal.next(), DynamicFrame::Two([3.0, 4.0]));
        assert_eq!(signal.next(), DynamicFrame::Two([5.0, 6.0]));

        assert_eq!(signal.next(), DynamicFrame::Two([0.0, 0.0]));
        assert_eq!(signal.next(), DynamicFrame::Two([0.0, 0.0]));
    }

    #[test]
    fn test_audioframe_read_signal_mono() {
        let samples = vec![1.0, 2.0, 3.0];
        let frame = AudioFrame::new(samples, AudioChannelCount::One, 0, 0, 48000);

        let mut signal = frame.read();

        assert_eq!(signal.next(), DynamicFrame::One([1.0]));
        assert_eq!(signal.next(), DynamicFrame::One([2.0]));
        assert_eq!(signal.next(), DynamicFrame::One([3.0]));
        assert_eq!(signal.next(), DynamicFrame::One([0.0]));
    }

    #[test]
    fn test_channel_count_conversions() {
        assert_eq!(AudioChannelCount::One.as_usize(), 1);
        assert_eq!(AudioChannelCount::Two.as_usize(), 2);
        assert_eq!(AudioChannelCount::Eight.as_usize(), 8);

        assert_eq!(
            AudioChannelCount::from_usize(1),
            Some(AudioChannelCount::One)
        );
        assert_eq!(
            AudioChannelCount::from_usize(2),
            Some(AudioChannelCount::Two)
        );
        assert_eq!(AudioChannelCount::from_usize(9), None);
    }
}
