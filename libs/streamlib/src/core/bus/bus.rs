
use super::connection_manager::ConnectionManager;
use super::connection::ProcessorConnection;
use super::frames::{AudioFrame, VideoFrame, DataFrame};
use std::sync::Arc;
use parking_lot::RwLock;

pub struct Bus {
    manager: Arc<RwLock<ConnectionManager>>,
}

impl Bus {
    pub fn new() -> Self {
        Self {
            manager: Arc::new(RwLock::new(ConnectionManager::new())),
        }
    }

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

    pub fn get_audio_connections_from_output<const CHANNELS: usize>(
        &self,
        source_processor: &str,
        source_port: &str,
    ) -> Vec<Arc<ProcessorConnection<AudioFrame<CHANNELS>>>> {
        self.manager.read().get_audio_connections_from_output(source_processor, source_port)
    }

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

    pub fn get_video_connections_from_output(
        &self,
        source_processor: &str,
        source_port: &str,
    ) -> Vec<Arc<ProcessorConnection<VideoFrame>>> {
        self.manager.read().get_video_connections_from_output(source_processor, source_port)
    }

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

    pub fn get_data_connections_from_output(
        &self,
        source_processor: &str,
        source_port: &str,
    ) -> Vec<Arc<ProcessorConnection<DataFrame>>> {
        self.manager.read().get_data_connections_from_output(source_processor, source_port)
    }

    pub fn connection_count(&self) -> usize {
        self.manager.read().connection_count()
    }
}

impl Default for Bus {
    fn default() -> Self {
        Self::new()
    }
}
