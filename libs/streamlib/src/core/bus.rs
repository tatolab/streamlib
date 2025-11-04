//! Centralized bus system for all processor connections.
//!
//! The Bus wraps ConnectionManager and provides helper methods for creating
//! audio, video, and data connections. All connections use rtrb ring buffers.
//!
//! # Architecture
//!
//! - **Centralized Bus**: Contains ConnectionManager for all connections
//! - **Unified rtrb**: All frame types (AudioFrame, VideoFrame, DataFrame) use rtrb
//! - **Fan-out support**: 1 output → N inputs = N separate rtrb connections
//! - **Compile-time safety**: Generic CHANNELS parameter for AudioFrame
//!
//! # Example
//!
//! ```ignore
//! let bus = Bus::new();
//!
//! // Create audio connection with 2 channels (stereo)
//! let audio_conn = bus.create_audio_connection::<2>(
//!     "mixer".to_string(),
//!     "audio".to_string(),
//!     "speaker".to_string(),
//!     "audio".to_string(),
//!     4,  // capacity: 4 frames
//! );
//!
//! // Fan-out: Connect same output to multiple inputs
//! let reverb_conn = bus.create_audio_connection::<2>(
//!     "mixer".to_string(),  // Same source!
//!     "audio".to_string(),
//!     "reverb".to_string(),
//!     "audio".to_string(),
//!     4,
//! );
//! ```

use super::connection_manager::ConnectionManager;
use super::connection::ProcessorConnection;
use super::frames::{AudioFrame, VideoFrame, DataFrame};
use std::sync::Arc;
use parking_lot::RwLock;

/// Centralized bus system for all processor connections.
///
/// The Bus manages all rtrb ring buffer connections between processors.
/// It provides helper methods for creating audio, video, and data connections
/// with type-safe channel count checking at compile time.
pub struct Bus {
    manager: Arc<RwLock<ConnectionManager>>,
}

impl Bus {
    pub fn new() -> Self {
        Self {
            manager: Arc::new(RwLock::new(ConnectionManager::new())),
        }
    }

    /// Create an audio connection with compile-time channel count checking.
    ///
    /// # Type Parameters
    /// * `CHANNELS` - Number of audio channels (e.g., 1 for mono, 2 for stereo)
    ///
    /// # Arguments
    /// * `source_processor` - Name of the source processor
    /// * `source_port` - Name of the output port on the source processor
    /// * `dest_processor` - Name of the destination processor
    /// * `dest_port` - Name of the input port on the destination processor
    /// * `capacity` - Ring buffer capacity (number of frames, typically 4)
    ///
    /// # Returns
    /// An Arc to the created ProcessorConnection
    pub fn create_audio_connection<const CHANNELS: usize>(
        &self,
        source_processor: String,
        source_port: String,
        dest_processor: String,
        dest_port: String,
        capacity: usize,
    ) -> Arc<ProcessorConnection<AudioFrame<CHANNELS>>> {
        self.manager.write().create_audio_connection(
            source_processor,
            source_port,
            dest_processor,
            dest_port,
            capacity,
        )
    }

    /// Get all audio connections from a specific output port.
    /// Supports fan-out (1 output → N inputs).
    pub fn get_audio_connections_from_output<const CHANNELS: usize>(
        &self,
        source_processor: &str,
        source_port: &str,
    ) -> Vec<Arc<ProcessorConnection<AudioFrame<CHANNELS>>>> {
        self.manager.read().get_audio_connections_from_output(source_processor, source_port)
    }

    /// Create a video connection.
    ///
    /// # Arguments
    /// * `source_processor` - Name of the source processor
    /// * `source_port` - Name of the output port on the source processor
    /// * `dest_processor` - Name of the destination processor
    /// * `dest_port` - Name of the input port on the destination processor
    /// * `capacity` - Ring buffer capacity (number of frames, typically 4)
    pub fn create_video_connection(
        &self,
        source_processor: String,
        source_port: String,
        dest_processor: String,
        dest_port: String,
        capacity: usize,
    ) -> Arc<ProcessorConnection<VideoFrame>> {
        self.manager.write().create_video_connection(
            source_processor,
            source_port,
            dest_processor,
            dest_port,
            capacity,
        )
    }

    /// Get all video connections from a specific output port.
    /// Supports fan-out (1 output → N inputs).
    pub fn get_video_connections_from_output(
        &self,
        source_processor: &str,
        source_port: &str,
    ) -> Vec<Arc<ProcessorConnection<VideoFrame>>> {
        self.manager.read().get_video_connections_from_output(source_processor, source_port)
    }

    /// Create a data connection.
    ///
    /// # Arguments
    /// * `source_processor` - Name of the source processor
    /// * `source_port` - Name of the output port on the source processor
    /// * `dest_processor` - Name of the destination processor
    /// * `dest_port` - Name of the input port on the destination processor
    /// * `capacity` - Ring buffer capacity (number of frames, typically 4)
    pub fn create_data_connection(
        &self,
        source_processor: String,
        source_port: String,
        dest_processor: String,
        dest_port: String,
        capacity: usize,
    ) -> Arc<ProcessorConnection<DataFrame>> {
        self.manager.write().create_data_connection(
            source_processor,
            source_port,
            dest_processor,
            dest_port,
            capacity,
        )
    }

    /// Get all data connections from a specific output port.
    /// Supports fan-out (1 output → N inputs).
    pub fn get_data_connections_from_output(
        &self,
        source_processor: &str,
        source_port: &str,
    ) -> Vec<Arc<ProcessorConnection<DataFrame>>> {
        self.manager.read().get_data_connections_from_output(source_processor, source_port)
    }

    /// Get total number of connections across all types.
    pub fn connection_count(&self) -> usize {
        self.manager.read().connection_count()
    }
}

impl Default for Bus {
    fn default() -> Self {
        Self::new()
    }
}
