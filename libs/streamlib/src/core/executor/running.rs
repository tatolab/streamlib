use std::ops::Deref;
use std::sync::Arc;
use std::thread::JoinHandle;

use parking_lot::Mutex;
use serde::ser::{SerializeStruct, Serializer};
use serde::Serialize;

use crate::core::graph::{Link, ProcessorNode};
use crate::core::link_channel::{LinkPortType, LinkWakeupEvent};
use crate::core::processors::ProcessorState;

use super::BoxedProcessor;

/// A processor that is currently running in the executor
///
/// Extends `ProcessorNode` (graph data) with runtime state:
/// - Thread handle for the processor's execution thread
/// - Channels for shutdown and wakeup signals
/// - Current execution state
/// - Reference to the actual processor instance
///
/// Implements `Deref<Target = ProcessorNode>` so all node fields
/// are directly accessible (e.g., `running.id`, `running.processor_type`).
pub(crate) struct RunningProcessor {
    /// The graph node this runtime state extends
    pub node: ProcessorNode,
    /// Thread handle (None if not yet started or already joined)
    pub thread: Option<JoinHandle<()>>,
    /// Channel to signal shutdown
    pub shutdown_tx: crossbeam_channel::Sender<()>,
    /// Channel to wake up the processor (for push/pull scheduling)
    pub wakeup_tx: crossbeam_channel::Sender<LinkWakeupEvent>,
    /// Current processor state
    pub state: Arc<Mutex<ProcessorState>>,
    /// The actual processor instance
    pub processor: Option<Arc<Mutex<BoxedProcessor>>>,
}

impl Deref for RunningProcessor {
    type Target = ProcessorNode;

    fn deref(&self) -> &Self::Target {
        &self.node
    }
}

impl RunningProcessor {
    /// Create a new running processor from a node and runtime components
    pub fn new(
        node: ProcessorNode,
        thread: Option<JoinHandle<()>>,
        shutdown_tx: crossbeam_channel::Sender<()>,
        wakeup_tx: crossbeam_channel::Sender<LinkWakeupEvent>,
        state: Arc<Mutex<ProcessorState>>,
        processor: Option<Arc<Mutex<BoxedProcessor>>>,
    ) -> Self {
        Self {
            node,
            thread,
            shutdown_tx,
            wakeup_tx,
            state,
            processor,
        }
    }

    /// Get the current state (locks the mutex briefly)
    #[allow(dead_code)]
    pub fn current_state(&self) -> ProcessorState {
        *self.state.lock()
    }
}

/// Manual Serialize implementation for RunningProcessor
///
/// Serializes the observable/debuggable parts:
/// - node: Full ProcessorNode (id, type, config, ports)
/// - state: Current ProcessorState
/// - has_thread: Whether thread is active
///
/// Skips non-serializable runtime artifacts (thread handle, channels, processor instance)
impl Serialize for RunningProcessor {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut s = serializer.serialize_struct("RunningProcessor", 3)?;
        s.serialize_field("node", &self.node)?;
        s.serialize_field("state", &*self.state.lock())?;
        s.serialize_field("has_thread", &self.thread.is_some())?;
        s.end()
    }
}

/// A link that has been wired with actual ring buffers
///
/// Extends `Link` (graph edge) with runtime metadata:
/// - Port type (Audio, Video, Data)
/// - Ring buffer capacity
///
/// Implements `Deref<Target = Link>` so all link fields
/// are directly accessible (e.g., `wired.id`, `wired.source`, `wired.target`).
#[derive(Debug, Clone, Serialize)]
pub(crate) struct WiredLink {
    /// The graph link this runtime state extends
    pub link: Link,
    /// Type of data flowing through this link
    pub port_type: LinkPortType,
    /// Ring buffer capacity
    pub capacity: usize,
}

impl Deref for WiredLink {
    type Target = Link;

    fn deref(&self) -> &Self::Target {
        &self.link
    }
}

impl WiredLink {
    /// Create a new wired link from a graph link and runtime metadata
    pub fn new(link: Link, port_type: LinkPortType, capacity: usize) -> Self {
        Self {
            link,
            port_type,
            capacity,
        }
    }

    /// Get the source processor ID
    pub fn source_processor(&self) -> &str {
        &self.link.source.node
    }

    /// Get the destination processor ID
    pub fn dest_processor(&self) -> &str {
        &self.link.target.node
    }
}
