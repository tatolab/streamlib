//! Audio bus implementation using dasp's SignalBus
//!
//! This module wraps dasp's SignalBus to provide the Bus trait interface.
//! It enables fan-out for audio signals - one output can feed multiple inputs,
//! with each input getting its own independent reader.
//!
//! # Design
//!
//! - Uses dasp::signal::SignalBus under the hood (signal.bus())
//! - SignalGenerator creates ONE signal and ONE bus
//! - Each reader is created via bus.send() (independent iterator)
//! - Lazy evaluation - signals only computed when readers consume them
//!
//! # Example
//!
//! ```ignore
//! // Source creates signal once
//! let signal = sine_generator.create_signal();
//! let bus = signal.bus();
//!
//! // Inputs subscribe to get readers
//! let mut reader1 = bus.send();
//! let mut reader2 = bus.send();
//!
//! // Both readers independently iterate the same signal
//! reader1.take(100);
//! reader2.take(100);
//! ```

use super::{Bus, BusId, BusReader};
use crate::core::frames::AudioSignal;
use dasp::signal::{Signal, bus::SignalBus};
use std::sync::{Arc, Mutex};

/// Audio bus wrapping dasp's SignalBus
///
/// CHANNELS is compile-time constant: 1=mono, 2=stereo, 4=quad, 6=5.1
///
/// This stores the dasp SignalBus created from the source's signal.
/// Each call to create_reader() returns a new reader via bus.send().
pub struct AudioBus<const CHANNELS: usize> {
    id: BusId,
    /// The dasp SignalBus (created once from the signal)
    signal_bus: Arc<Mutex<Option<SignalBus<Box<dyn Signal<Frame = [f32; CHANNELS]> + Send>>>>>,
    sample_rate: Arc<Mutex<u32>>,
    timestamp_ns: Arc<Mutex<i64>>,
}

impl<const CHANNELS: usize> AudioBus<CHANNELS> {
    pub fn new() -> Self {
        Self {
            id: BusId::new(),
            signal_bus: Arc::new(Mutex::new(None)),
            sample_rate: Arc::new(Mutex::new(48000)),
            timestamp_ns: Arc::new(Mutex::new(0)),
        }
    }
}

impl<const CHANNELS: usize> Bus<AudioSignal<CHANNELS>> for AudioBus<CHANNELS> {
    fn id(&self) -> BusId {
        self.id
    }

    fn create_reader(&self) -> Box<dyn BusReader<AudioSignal<CHANNELS>>> {
        tracing::debug!("[AudioBus {}] Creating reader for {}-channel bus", self.id, CHANNELS);

        Box::new(AudioBusReader {
            bus_id: self.id,
            signal_bus: Arc::clone(&self.signal_bus),
            sample_rate: Arc::clone(&self.sample_rate),
            timestamp_ns: Arc::clone(&self.timestamp_ns),
            signal_reader: Mutex::new(None),
        })
    }

    fn write(&self, message: AudioSignal<CHANNELS>) {
        *self.sample_rate.lock().unwrap() = message.sample_rate();
        *self.timestamp_ns.lock().unwrap() = message.timestamp_ns();

        let signal = message.create_signal();
        let bus = signal.bus();
        *self.signal_bus.lock().unwrap() = Some(bus);

        tracing::trace!("[AudioBus {}] Created SignalBus from signal", self.id);
    }
}

/// Reader for AudioBus
///
/// Wraps dasp's signal reader created via bus.send()
pub struct AudioBusReader<const CHANNELS: usize> {
    bus_id: BusId,
    signal_bus: Arc<Mutex<Option<SignalBus<Box<dyn Signal<Frame = [f32; CHANNELS]> + Send>>>>>,
    sample_rate: Arc<Mutex<u32>>,
    timestamp_ns: Arc<Mutex<i64>>,
    signal_reader: Mutex<Option<Box<dyn Signal<Frame = [f32; CHANNELS]> + Send>>>,
}

impl<const CHANNELS: usize> AudioBusReader<CHANNELS> {
    fn ensure_reader(&self) {
        let mut reader = self.signal_reader.lock().unwrap();
        if reader.is_none() {
            if let Some(bus) = self.signal_bus.lock().unwrap().as_mut() {
                *reader = Some(bus.send());
                tracing::debug!("[AudioBus {}] Created signal reader via bus.send()", self.bus_id);
            }
        }
    }
}

impl<const CHANNELS: usize> BusReader<AudioSignal<CHANNELS>> for AudioBusReader<CHANNELS> {
    fn read_latest(&mut self) -> Option<AudioSignal<CHANNELS>> {
        self.ensure_reader();

        let signal_bus = self.signal_bus.lock().unwrap();
        if signal_bus.is_some() {
            let sample_rate = *self.sample_rate.lock().unwrap();
            let timestamp_ns = *self.timestamp_ns.lock().unwrap();

            Some(AudioSignal::from_bus_reader(
                Arc::clone(&self.signal_reader),
                sample_rate,
                timestamp_ns,
            ))
        } else {
            None
        }
    }

    fn has_data(&self) -> bool {
        self.signal_bus.lock().unwrap().is_some()
    }

    fn clone_reader(&self) -> Box<dyn BusReader<AudioSignal<CHANNELS>>> {
        Box::new(Self {
            bus_id: self.bus_id,
            signal_bus: Arc::clone(&self.signal_bus),
            sample_rate: Arc::clone(&self.sample_rate),
            timestamp_ns: Arc::clone(&self.timestamp_ns),
            signal_reader: Mutex::new(None),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::frames::{BufferGenerator, MonoSignal};

    #[test]
    fn test_audio_bus_fan_out() {
        // Create a mono bus
        let bus = AudioBus::<1>::new();

        // Create two readers
        let mut reader1 = bus.create_reader();
        let mut reader2 = bus.create_reader();

        let samples = vec![[1.0f32], [2.0], [3.0]];
        let gen = BufferGenerator::new(samples.clone(), false);
        let signal = MonoSignal::new(gen.create_signal(), 48000, 0);
        bus.write(signal);

        // Both readers should get the signal
        let sig1 = reader1.read_latest();
        let sig2 = reader2.read_latest();

        assert!(sig1.is_some());
        assert!(sig2.is_some());

        // They should be independent copies
        let samples1 = sig1.unwrap().take_samples(3);
        let samples2 = sig2.unwrap().take_samples(3);

        assert_eq!(samples1, samples);
        assert_eq!(samples2, samples);
    }

    #[test]
    fn test_audio_bus_no_data() {
        let bus = AudioBus::<1>::new();
        let mut reader = bus.create_reader();

        // No data written yet
        assert!(!reader.has_data());
        assert!(reader.read_latest().is_none());
    }
}
