//! ECS components for PropertyGraph.
//!
//! Each component represents a specific aspect of processor runtime state.
//! Components are attached to processor entities in the ECS world.

use std::sync::Arc;
use std::thread::JoinHandle;

use crossbeam_channel::{Receiver, Sender};
use parking_lot::Mutex;
use serde_json::Value as JsonValue;

use crate::core::graph::link::LinkState;
use crate::core::links::LinkOutputToProcessorMessage;
use crate::core::processors::{BoxedProcessor, ProcessorState};

// ============================================================================
// ECS Component JSON Serialization
// ============================================================================

/// Trait for ECS components that can serialize to JSON.
///
/// Components implement this trait to opt-in to JSON serialization.
/// Components that don't implement this trait are simply skipped during
/// serialization - they don't cause errors.
pub trait EcsComponentJson {
    /// The component's key in the JSON output.
    fn json_key(&self) -> &'static str;

    /// Serialize this component to JSON.
    fn to_json(&self) -> JsonValue;
}

/// The instantiated processor instance.
pub struct ProcessorInstance(pub Arc<Mutex<BoxedProcessor>>);

/// Thread handle for dedicated-thread processors.
pub struct ThreadHandle(pub JoinHandle<()>);

/// Channel to signal processor shutdown.
pub struct ShutdownChannel {
    pub sender: Sender<()>,
    pub receiver: Option<Receiver<()>>,
}

impl ShutdownChannel {
    /// Create a new shutdown channel.
    pub fn new() -> Self {
        let (sender, receiver) = crossbeam_channel::bounded(1);
        Self {
            sender,
            receiver: Some(receiver),
        }
    }

    /// Take the receiver (can only be done once).
    pub fn take_receiver(&mut self) -> Option<Receiver<()>> {
        self.receiver.take()
    }
}

impl Default for ShutdownChannel {
    fn default() -> Self {
        Self::new()
    }
}

/// Writer and reader pair for messages from LinkOutput to this processor.
pub struct LinkOutputToProcessorWriterAndReader {
    pub writer: Sender<LinkOutputToProcessorMessage>,
    pub reader: Option<Receiver<LinkOutputToProcessorMessage>>,
}

impl LinkOutputToProcessorWriterAndReader {
    /// Create a new writer and reader pair.
    pub fn new() -> Self {
        let (writer, reader) = crossbeam_channel::unbounded();
        Self {
            writer,
            reader: Some(reader),
        }
    }

    /// Take the reader (can only be done once).
    pub fn take_reader(&mut self) -> Option<Receiver<LinkOutputToProcessorMessage>> {
        self.reader.take()
    }
}

impl Default for LinkOutputToProcessorWriterAndReader {
    fn default() -> Self {
        Self::new()
    }
}

/// Current state of the processor.
pub struct StateComponent(pub Arc<Mutex<ProcessorState>>);

impl Default for StateComponent {
    fn default() -> Self {
        Self(Arc::new(Mutex::new(ProcessorState::Idle)))
    }
}

impl EcsComponentJson for StateComponent {
    fn json_key(&self) -> &'static str {
        "state"
    }

    fn to_json(&self) -> JsonValue {
        let state = self.0.lock();
        serde_json::json!(format!("{:?}", *state))
    }
}

/// Runtime metrics for a processor.
#[derive(Default, Clone)]
pub struct ProcessorMetrics {
    /// Frames per second throughput.
    pub throughput_fps: f64,
    /// 50th percentile latency in milliseconds.
    pub latency_p50_ms: f64,
    /// 99th percentile latency in milliseconds.
    pub latency_p99_ms: f64,
    /// Total frames processed.
    pub frames_processed: u64,
    /// Total frames dropped.
    pub frames_dropped: u64,
}

impl EcsComponentJson for ProcessorMetrics {
    fn json_key(&self) -> &'static str {
        "metrics"
    }

    fn to_json(&self) -> JsonValue {
        serde_json::json!({
            "throughput_fps": self.throughput_fps,
            "latency_p50_ms": self.latency_p50_ms,
            "latency_p99_ms": self.latency_p99_ms,
            "frames_processed": self.frames_processed,
            "frames_dropped": self.frames_dropped
        })
    }
}

/// Marker for processors that must run on main thread (Apple frameworks).
pub struct MainThreadMarker;

/// Marker for processors using Rayon work-stealing pool.
pub struct RayonPoolMarker;

/// Marker for lightweight processors (no dedicated resources).
pub struct LightweightMarker;

/// Runtime state component for links (attached to link entities).
pub struct LinkStateComponent(pub LinkState);

impl Default for LinkStateComponent {
    fn default() -> Self {
        Self(LinkState::Pending)
    }
}

impl EcsComponentJson for LinkStateComponent {
    fn json_key(&self) -> &'static str {
        "state"
    }

    fn to_json(&self) -> JsonValue {
        serde_json::json!(format!("{:?}", self.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shutdown_channel() {
        let mut channel = ShutdownChannel::new();
        let receiver = channel.take_receiver().expect("should have receiver");

        // Send shutdown signal
        channel.sender.send(()).unwrap();

        // Receive it
        assert!(receiver.recv().is_ok());

        // Second take should return None
        assert!(channel.take_receiver().is_none());
    }

    #[test]
    fn test_link_output_to_processor_writer_and_reader() {
        let mut pair = LinkOutputToProcessorWriterAndReader::new();
        let reader = pair.take_reader().expect("should have reader");

        // Send message
        pair.writer
            .send(LinkOutputToProcessorMessage::InvokeProcessingNow)
            .unwrap();

        // Receive it
        assert!(matches!(
            reader.recv(),
            Ok(LinkOutputToProcessorMessage::InvokeProcessingNow)
        ));
    }

    #[test]
    fn test_state_component_default() {
        let state = StateComponent::default();
        assert_eq!(*state.0.lock(), ProcessorState::Idle);
    }
}
