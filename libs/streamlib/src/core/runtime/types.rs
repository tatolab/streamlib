//! Runtime data structures
//!
//! This module defines the core data structures used by the runtime:
//! - `Connection` - Metadata about a connection between processors
//! - `RuntimeProcessorHandle` - Per-processor runtime state and thread handle
//! - Type aliases for common patterns

use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use std::thread::JoinHandle;

use super::state::{ProcessorStatus, WakeupEvent};
use crate::core::bus::PortType;
use crate::core::traits::DynStreamElement;
use crate::core::Result;

/// Processor identifier type
pub type ProcessorId = String;

/// Connection identifier type
pub type ConnectionId = String;

/// GPU shader identifier
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ShaderId(pub u64);

/// Type-erased processor (boxed trait object)
pub type DynProcessor = Box<dyn DynStreamElement>;

/// Event loop function type for custom main loops
pub type EventLoopFn = Box<dyn FnOnce() -> Result<()> + Send>;

/// Internal handle for a processor's runtime state
///
/// This struct tracks everything the runtime needs to manage a processor:
/// - Thread handle for joining
/// - Shutdown channel for graceful termination
/// - Wakeup channel for event-driven scheduling
/// - Status for lifecycle tracking
/// - Reference to the processor itself
pub(crate) struct RuntimeProcessorHandle {
    /// Unique processor identifier
    pub id: ProcessorId,
    /// Human-readable name
    pub name: String,
    /// Thread handle (None if not yet spawned or already joined)
    pub(crate) thread: Option<JoinHandle<()>>,
    /// Channel to send shutdown signal
    pub(crate) shutdown_tx: crossbeam_channel::Sender<()>,
    /// Channel to send wakeup events (DataAvailable, TimerTick)
    pub(crate) wakeup_tx: crossbeam_channel::Sender<WakeupEvent>,
    /// Current processor status
    pub(crate) status: Arc<Mutex<ProcessorStatus>>,
    /// Reference to the processor (wrapped for thread-safe access)
    pub(crate) processor: Option<Arc<Mutex<DynProcessor>>>,
}

/// Connection metadata
///
/// Stores both high-level port addresses (e.g., "processor_0.video") and
/// decomposed processor IDs for efficient graph traversal and optimization.
#[derive(Debug, Clone)]
pub struct Connection {
    /// Unique connection identifier
    pub id: ConnectionId,
    /// Full source port address (e.g., "processor_0.video")
    pub from_port: String,
    /// Full destination port address (e.g., "processor_1.video")
    pub to_port: String,
    /// Source processor ID (parsed from from_port)
    pub source_processor: ProcessorId,
    /// Destination processor ID (parsed from to_port)
    pub dest_processor: ProcessorId,
    /// Type of data flowing through this connection
    pub port_type: PortType,
    /// Current buffer capacity (number of frames/samples)
    pub buffer_capacity: usize,
    /// When this connection was created
    pub created_at: std::time::Instant,
}

impl Connection {
    /// Create a new connection with metadata
    ///
    /// Parses processor IDs from the port addresses (format: "processor_id.port_name")
    pub fn new(
        id: ConnectionId,
        from_port: String,
        to_port: String,
        port_type: PortType,
        buffer_capacity: usize,
    ) -> Self {
        // Parse processor IDs from port addresses (format: "processor_0.video")
        let source_processor = from_port.split('.').next().unwrap_or("").to_string();
        let dest_processor = to_port.split('.').next().unwrap_or("").to_string();

        Self {
            id,
            from_port,
            to_port,
            source_processor,
            dest_processor,
            port_type,
            buffer_capacity,
            created_at: std::time::Instant::now(),
        }
    }
}

/// Runtime status snapshot
///
/// Used by `StreamRuntime::status()` to provide a point-in-time view
/// of the runtime's state.
#[derive(Debug, Clone)]
pub struct RuntimeStatus {
    /// Whether the runtime is currently running
    pub running: bool,
    /// Number of processors registered
    pub processor_count: usize,
    /// Number of active connections
    pub connection_count: usize,
    /// Per-processor status
    pub processor_statuses: HashMap<ProcessorId, ProcessorStatus>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_connection_parses_processor_ids() {
        let conn = Connection::new(
            "conn-1".to_string(),
            "processor_0.video_out".to_string(),
            "processor_1.video_in".to_string(),
            PortType::Video,
            3,
        );

        assert_eq!(conn.source_processor, "processor_0");
        assert_eq!(conn.dest_processor, "processor_1");
        assert_eq!(conn.from_port, "processor_0.video_out");
        assert_eq!(conn.to_port, "processor_1.video_in");
    }

    #[test]
    fn test_connection_stores_port_type_and_capacity() {
        let conn = Connection::new(
            "conn-1".to_string(),
            "proc1.audio".to_string(),
            "proc2.audio".to_string(),
            PortType::Audio,
            32,
        );

        assert_eq!(conn.buffer_capacity, 32);
        assert!(matches!(conn.port_type, PortType::Audio));
    }

    #[test]
    fn test_connection_handles_edge_cases() {
        // No port name (just processor ID)
        let conn = Connection::new(
            "conn".to_string(),
            "processor".to_string(),
            "other".to_string(),
            PortType::Data,
            1,
        );

        assert_eq!(conn.source_processor, "processor");
        assert_eq!(conn.dest_processor, "other");
    }

    #[test]
    fn test_connection_handles_multiple_dots() {
        let conn = Connection::new(
            "conn".to_string(),
            "proc.port.extra".to_string(),
            "dest.in.more".to_string(),
            PortType::Video,
            1,
        );

        // Should take first part before dot
        assert_eq!(conn.source_processor, "proc");
        assert_eq!(conn.dest_processor, "dest");
    }

    #[test]
    fn test_connection_different_ids_same_ports() {
        let conn1 = Connection::new(
            "conn-1".to_string(),
            "proc_a.output".to_string(),
            "proc_b.input".to_string(),
            PortType::Video,
            5,
        );

        let conn2 = Connection::new(
            "conn-2".to_string(),
            "proc_a.output".to_string(),
            "proc_b.input".to_string(),
            PortType::Video,
            5,
        );

        // Same port addresses should parse to same processor IDs
        assert_eq!(conn1.source_processor, conn2.source_processor);
        assert_eq!(conn1.dest_processor, conn2.dest_processor);

        // But different connection IDs
        assert_ne!(conn1.id, conn2.id);
    }
}
