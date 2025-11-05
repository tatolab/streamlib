use super::connection::{ProcessorConnection, ConnectionId};
use super::frames::{AudioFrame, VideoFrame, DataFrame};
use std::collections::HashMap;
use std::sync::Arc;

pub struct ConnectionManager {
    audio_connections: HashMap<ConnectionId, Arc<dyn std::any::Any + Send + Sync>>,
    video_connections: HashMap<ConnectionId, Arc<ProcessorConnection<VideoFrame>>>,
    data_connections: HashMap<ConnectionId, Arc<ProcessorConnection<DataFrame>>>,
    source_to_connections: HashMap<String, Vec<ConnectionId>>,
}

impl ConnectionManager {
    pub fn new() -> Self {
        Self {
            audio_connections: HashMap::new(),
            video_connections: HashMap::new(),
            data_connections: HashMap::new(),
            source_to_connections: HashMap::new(),
        }
    }

    pub fn create_audio_connection<const CHANNELS: usize>(
        &mut self,
        source_processor: String,
        source_port: String,
        dest_processor: String,
        dest_port: String,
        capacity: usize,
    ) -> Arc<ProcessorConnection<AudioFrame<CHANNELS>>> {
        let connection = Arc::new(ProcessorConnection::new(
            source_processor.clone(),
            source_port.clone(),
            dest_processor,
            dest_port,
            capacity,
        ));

        let source_key = format!("{}.{}", source_processor, source_port);
        self.source_to_connections
            .entry(source_key)
            .or_insert_with(Vec::new)
            .push(connection.id);

        self.audio_connections.insert(
            connection.id,
            Arc::clone(&connection) as Arc<dyn std::any::Any + Send + Sync>
        );

        connection
    }

    pub fn create_video_connection(
        &mut self,
        source_processor: String,
        source_port: String,
        dest_processor: String,
        dest_port: String,
        capacity: usize,
    ) -> Arc<ProcessorConnection<VideoFrame>> {
        let connection = Arc::new(ProcessorConnection::new(
            source_processor.clone(),
            source_port.clone(),
            dest_processor,
            dest_port,
            capacity,
        ));

        let source_key = format!("{}.{}", source_processor, source_port);
        self.source_to_connections
            .entry(source_key)
            .or_insert_with(Vec::new)
            .push(connection.id);

        self.video_connections.insert(connection.id, Arc::clone(&connection));
        connection
    }

    pub fn create_data_connection(
        &mut self,
        source_processor: String,
        source_port: String,
        dest_processor: String,
        dest_port: String,
        capacity: usize,
    ) -> Arc<ProcessorConnection<DataFrame>> {
        let connection = Arc::new(ProcessorConnection::new(
            source_processor.clone(),
            source_port.clone(),
            dest_processor,
            dest_port,
            capacity,
        ));

        let source_key = format!("{}.{}", source_processor, source_port);
        self.source_to_connections
            .entry(source_key)
            .or_insert_with(Vec::new)
            .push(connection.id);

        self.data_connections.insert(connection.id, Arc::clone(&connection));
        connection
    }

    pub fn get_audio_connection<const CHANNELS: usize>(
        &self,
        id: ConnectionId,
    ) -> Option<Arc<ProcessorConnection<AudioFrame<CHANNELS>>>> {
        self.audio_connections
            .get(&id)
            .and_then(|any| any.clone().downcast::<ProcessorConnection<AudioFrame<CHANNELS>>>().ok())
    }

    pub fn get_audio_connections_from_output<const CHANNELS: usize>(
        &self,
        source_processor: &str,
        source_port: &str,
    ) -> Vec<Arc<ProcessorConnection<AudioFrame<CHANNELS>>>> {
        let source_key = format!("{}.{}", source_processor, source_port);
        self.source_to_connections
            .get(&source_key)
            .map(|ids| {
                ids.iter()
                    .filter_map(|id| self.get_audio_connection(*id))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn get_video_connections_from_output(
        &self,
        source_processor: &str,
        source_port: &str,
    ) -> Vec<Arc<ProcessorConnection<VideoFrame>>> {
        let source_key = format!("{}.{}", source_processor, source_port);
        self.source_to_connections
            .get(&source_key)
            .map(|ids| {
                ids.iter()
                    .filter_map(|id| self.video_connections.get(id))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn get_data_connections_from_output(
        &self,
        source_processor: &str,
        source_port: &str,
    ) -> Vec<Arc<ProcessorConnection<DataFrame>>> {
        let source_key = format!("{}.{}", source_processor, source_port);
        self.source_to_connections
            .get(&source_key)
            .map(|ids| {
                ids.iter()
                    .filter_map(|id| self.data_connections.get(id))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn remove_connection(&mut self, id: ConnectionId) {
        self.audio_connections.remove(&id);
        self.video_connections.remove(&id);
        self.data_connections.remove(&id);

        for (_, ids) in self.source_to_connections.iter_mut() {
            ids.retain(|&conn_id| conn_id != id);
        }
    }

    pub fn connection_count(&self) -> usize {
        self.audio_connections.len() + self.video_connections.len() + self.data_connections.len()
    }
}

impl Default for ConnectionManager {
    fn default() -> Self {
        Self::new()
    }
}
