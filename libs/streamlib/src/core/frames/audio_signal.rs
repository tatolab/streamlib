use dasp::signal::{Signal, bus::SignalBus};
use std::sync::{Arc, Mutex};

pub struct AudioSignal<const CHANNELS: usize> {
    bus: Arc<Mutex<SignalBus<Box<dyn Signal<Frame = [f32; CHANNELS]> + Send>>>>,
    sample_rate: u32,
    timestamp_ns: i64,
}

impl<const CHANNELS: usize> AudioSignal<CHANNELS> {
    pub fn new(
        signal: Box<dyn Signal<Frame = [f32; CHANNELS]> + Send>,
        sample_rate: u32,
        timestamp_ns: i64,
    ) -> Self {
        let bus = signal.bus();
        Self {
            bus: Arc::new(Mutex::new(bus)),
            sample_rate,
            timestamp_ns,
        }
    }

    pub const fn channels(&self) -> usize {
        CHANNELS
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn timestamp_ns(&self) -> i64 {
        self.timestamp_ns
    }

    pub fn create_signal(&self) -> Box<dyn dasp::signal::Signal<Frame = [f32; CHANNELS]> + Send> {
        let mut bus = self.bus.lock().unwrap();
        bus.send()
    }

    pub fn from_bus_reader(
        reader: Arc<Mutex<Option<Box<dyn dasp::signal::Signal<Frame = [f32; CHANNELS]> + Send>>>>,
        sample_rate: u32,
        timestamp_ns: i64,
    ) -> Self {
        struct ReaderWrapper<const CHANNELS: usize> {
            reader: Arc<Mutex<Option<Box<dyn Signal<Frame = [f32; CHANNELS]> + Send>>>>,
        }

        impl<const CHANNELS: usize> Signal for ReaderWrapper<CHANNELS> {
            type Frame = [f32; CHANNELS];

            fn next(&mut self) -> Self::Frame {
                let mut reader = self.reader.lock().unwrap();
                if let Some(ref mut sig) = *reader {
                    sig.next()
                } else {
                    [0.0; CHANNELS]
                }
            }
        }

        let wrapper = Box::new(ReaderWrapper { reader });
        let bus = wrapper.bus();
        Self {
            bus: Arc::new(Mutex::new(bus)),
            sample_rate,
            timestamp_ns,
        }
    }
}

impl MonoSignal {
    pub fn take_samples(&self, n: usize) -> Vec<[f32; 1]> {
        let mut sig = self.create_signal();
        (0..n).map(|_| sig.next()).collect()
    }

    pub fn take_interleaved(&self, n: usize) -> Vec<f32> {
        let frames = self.take_samples(n);
        frames.into_iter().flat_map(|frame| frame.into_iter()).collect()
    }
}

impl StereoSignal {
    pub fn take_samples(&self, n: usize) -> Vec<[f32; 2]> {
        let mut sig = self.create_signal();
        (0..n).map(|_| sig.next()).collect()
    }

    pub fn take_interleaved(&self, n: usize) -> Vec<f32> {
        let frames = self.take_samples(n);
        frames.into_iter().flat_map(|frame| frame.into_iter()).collect()
    }
}

impl QuadSignal {
    pub fn take_samples(&self, n: usize) -> Vec<[f32; 4]> {
        let mut sig = self.create_signal();
        (0..n).map(|_| sig.next()).collect()
    }

    pub fn take_interleaved(&self, n: usize) -> Vec<f32> {
        let frames = self.take_samples(n);
        frames.into_iter().flat_map(|frame| frame.into_iter()).collect()
    }
}

impl FiveOneSignal {
    pub fn take_samples(&self, n: usize) -> Vec<[f32; 6]> {
        let mut sig = self.create_signal();
        (0..n).map(|_| sig.next()).collect()
    }

    pub fn take_interleaved(&self, n: usize) -> Vec<f32> {
        let frames = self.take_samples(n);
        frames.into_iter().flat_map(|frame| frame.into_iter()).collect()
    }
}

impl<const CHANNELS: usize> Clone for AudioSignal<CHANNELS> {
    fn clone(&self) -> Self {
        Self {
            bus: Arc::clone(&self.bus),
            sample_rate: self.sample_rate,
            timestamp_ns: self.timestamp_ns,
        }
    }
}

/// Type alias for mono (1-channel) audio signals
pub type MonoSignal = AudioSignal<1>;

/// Type alias for stereo (2-channel) audio signals
pub type StereoSignal = AudioSignal<2>;

/// Type alias for quad (4-channel) audio signals
pub type QuadSignal = AudioSignal<4>;

/// Type alias for 5.1 surround (6-channel) audio signals
pub type FiveOneSignal = AudioSignal<6>;

pub struct SineGenerator {
    frequency: f64,
    sample_rate: u32,
    amplitude: f32,
}

impl SineGenerator {
    pub fn new(frequency: f64, amplitude: f32, sample_rate: u32) -> Self {
        Self {
            frequency,
            sample_rate,
            amplitude,
        }
    }

    pub fn create_signal(&self) -> Box<dyn Signal<Frame = [f32; 1]> + Send> {
        use std::f64::consts::PI;

        let phase_inc = 2.0 * PI * self.frequency / self.sample_rate as f64;

        struct SineSignal {
            phase: f64,
            phase_inc: f64,
            amplitude: f32,
        }

        impl Signal for SineSignal {
            type Frame = [f32; 1];

            fn next(&mut self) -> Self::Frame {
                use std::f64::consts::PI;
                let sample = (self.phase.sin() * self.amplitude as f64) as f32;
                self.phase += self.phase_inc;
                if self.phase >= 2.0 * PI {
                    self.phase -= 2.0 * PI;
                }
                [sample]
            }
        }

        Box::new(SineSignal {
            phase: 0.0,
            phase_inc,
            amplitude: self.amplitude,
        })
    }
}

pub struct BufferGenerator<const CHANNELS: usize> {
    buffer: Vec<[f32; CHANNELS]>,
    looping: bool,
}

impl<const CHANNELS: usize> BufferGenerator<CHANNELS> {
    pub fn new(buffer: Vec<[f32; CHANNELS]>, looping: bool) -> Self {
        Self {
            buffer,
            looping,
        }
    }

    pub fn create_signal(&self) -> Box<dyn Signal<Frame = [f32; CHANNELS]> + Send>
    where
        [f32; CHANNELS]: Send + 'static,
    {
        struct BufferSignal<const CHANNELS: usize> {
            buffer: Vec<[f32; CHANNELS]>,
            position: usize,
            looping: bool,
        }

        macro_rules! impl_buffer_signal {
            ($channels:expr) => {
                impl Signal for BufferSignal<$channels> {
                    type Frame = [f32; $channels];
                    fn next(&mut self) -> Self::Frame {
                        let result = if self.position < self.buffer.len() {
                            self.buffer[self.position]
                        } else if self.looping && !self.buffer.is_empty() {
                            self.buffer[self.position % self.buffer.len()]
                        } else {
                            [0.0; $channels]
                        };
                        self.position += 1;
                        result
                    }
                }
            };
        }

        impl_buffer_signal!(1);
        impl_buffer_signal!(2);
        impl_buffer_signal!(4);
        impl_buffer_signal!(6);

        Box::new(BufferSignal {
            buffer: self.buffer.clone(),
            position: 0,
            looping: self.looping,
        })
    }
}

pub struct EquilibriumGenerator<const CHANNELS: usize>;

impl<const CHANNELS: usize> EquilibriumGenerator<CHANNELS> {
    pub fn create_signal(&self) -> Box<dyn Signal<Frame = [f32; CHANNELS]> + Send>
    where
        [f32; CHANNELS]: Send + 'static,
    {
        struct SilenceSignal<const CHANNELS: usize>;

        macro_rules! impl_silence_signal {
            ($channels:expr) => {
                impl Signal for SilenceSignal<$channels> {
                    type Frame = [f32; $channels];
                    fn next(&mut self) -> Self::Frame { [0.0; $channels] }
                }
            };
        }

        impl_silence_signal!(1);
        impl_silence_signal!(2);
        impl_silence_signal!(4);
        impl_silence_signal!(6);

        Box::new(SilenceSignal::<CHANNELS>)
    }
}

// =============================================================================
// PortMessage Implementation for AudioSignal
// =============================================================================

impl<const CHANNELS: usize> crate::core::ports::PortMessage for AudioSignal<CHANNELS> {
    fn port_type() -> crate::core::ports::PortType {
        crate::core::ports::PortType::Audio
    }

    fn schema() -> std::sync::Arc<crate::core::Schema> {
        use crate::core::schema::{Schema, Field, FieldType, SemanticVersion, SerializationFormat};

        std::sync::Arc::new(Schema::new(
            &format!("AudioSignal<{}>", CHANNELS),
            SemanticVersion { major: 1, minor: 0, patch: 0 },
            vec![
                Field::new("channels", FieldType::UInt32),
                Field::new("sample_rate", FieldType::UInt32),
                Field::new("timestamp_ns", FieldType::Int64),
            ],
            SerializationFormat::Json,
        ))
    }

    fn examples() -> Vec<(&'static str, serde_json::Value)> {
        vec![] // Signals are lazy - no concrete examples
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mono_signal_creation() {
        let gen = SineGenerator::new(440.0, 0.5, 48000);
        let signal = MonoSignal::new(gen.create_signal(), 48000, 0);

        assert_eq!(signal.channels(), 1);
        assert_eq!(signal.sample_rate(), 48000);
    }

    #[test]
    fn test_sine_generator() {
        let gen = SineGenerator::new(440.0, 1.0, 48000);
        let signal = MonoSignal::new(gen.create_signal(), 48000, 0);

        let samples = signal.take_interleaved(48000);

        assert_eq!(samples.len(), 48000);

        for sample in samples.iter() {
            assert!(sample.abs() <= 1.0);
        }
    }

    #[test]
    fn test_stereo_signal() {
        let buffer = vec![[0.5, -0.5]; 100];
        let gen = BufferGenerator::new(buffer, false);
        let signal = StereoSignal::new(gen.create_signal(), 48000, 0);

        assert_eq!(signal.channels(), 2);

        let samples = signal.take_samples(10);
        assert_eq!(samples.len(), 10);
        assert_eq!(samples[0], [0.5, -0.5]);
    }

    #[test]
    fn test_signal_cloning() {
        let gen = SineGenerator::new(440.0, 0.5, 48000);
        let signal1 = MonoSignal::new(gen.create_signal(), 48000, 0);
        let signal2 = signal1.clone();

        let samples1 = signal1.take_samples(10);
        let samples2 = signal2.take_samples(10);

        assert_eq!(samples1.len(), samples2.len());
    }
}
