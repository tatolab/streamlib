//! Connection management for the runtime
//!
//! This module contains all connection-related functionality:
//! - `connect()` - Type-safe connection API using port references
//! - `connect_by_id()` - String-based connection API for MCP/Python
//! - `disconnect()` - Remove connections by port references
//! - `disconnect_by_id()` - Remove connections by ID
//! - `wire_pending_connections()` - Wire pending connections at start

use crate::core::bus::PortType;
use crate::core::error::{Result, StreamError};
use crate::core::handles::PendingConnection;

use super::state::RuntimeState;
use super::types::Connection;
use super::StreamRuntime;

impl StreamRuntime {
    /// Connect two processors using type-safe port references
    ///
    /// This is the primary connection API. It works in ANY runtime state:
    /// - **Stopped/Starting/Stopping**: Connection is stored and wired at start()
    /// - **Running/Paused**: Connection is wired immediately (hot reloading)
    /// - **Restarting/PurgeRebuild**: Returns error (transient states)
    ///
    /// # Example
    /// ```rust,ignore
    /// let camera = runtime.add_processor::<CameraProcessor>()?;
    /// let display = runtime.add_processor::<DisplayProcessor>()?;
    ///
    /// // Type-safe connection
    /// runtime.connect(camera.output("video"), display.input("video"))?;
    /// ```
    pub fn connect<T: crate::core::bus::PortMessage>(
        &mut self,
        output: crate::core::handles::OutputPortRef<T>,
        input: crate::core::handles::InputPortRef<T>,
    ) -> Result<super::types::ConnectionId> {
        let source = format!("{}.{}", output.processor_id(), output.port_name());
        let destination = format!("{}.{}", input.processor_id(), input.port_name());

        match self.state {
            RuntimeState::Running | RuntimeState::Paused => {
                // Runtime active - wire immediately
                tracing::debug!(
                    "Wiring connection immediately (runtime {:?}): {} → {}",
                    self.state,
                    source,
                    destination
                );
                self.connect_internal(&source, &destination)
            }
            RuntimeState::Stopped | RuntimeState::Starting | RuntimeState::Stopping => {
                // Runtime not active - store for later wiring
                let connection_id = format!("connection_{}", self.next_connection_id);
                self.next_connection_id += 1;

                let pending = PendingConnection::new(
                    connection_id.clone(),
                    output.processor_id().clone(),
                    output.port_name().to_string(),
                    input.processor_id().clone(),
                    input.port_name().to_string(),
                );

                self.pending_connections.push(pending);

                // Update graph (source of truth)
                let graph_connection_id = crate::core::bus::connection_id::__private::new_unchecked(
                    connection_id.clone(),
                );

                // Determine port type for graph
                let port_type = self.infer_port_type::<T>();

                if let Err(e) = self.graph.add_connection(
                    graph_connection_id,
                    source.clone(),
                    destination.clone(),
                    port_type,
                ) {
                    tracing::warn!(
                        "Failed to add connection to graph (will retry at start): {}",
                        e
                    );
                }
                self.dirty = true;

                tracing::debug!(
                    "Registered pending connection (runtime {:?}): {} → {}",
                    self.state,
                    source,
                    destination
                );

                Ok(connection_id)
            }
            RuntimeState::Restarting | RuntimeState::PurgeRebuild => {
                // In transition - defer to pending connections
                Err(StreamError::Configuration(format!(
                    "Cannot connect during state {:?} - wait for transition to complete",
                    self.state
                )))
            }
        }
    }

    /// Helper to infer PortType from generic type T
    pub(super) fn infer_port_type<T: crate::core::bus::PortMessage>(&self) -> PortType {
        use crate::core::frames::{DataFrame, VideoFrame};
        use std::any::TypeId;

        let type_id = TypeId::of::<T>();

        if type_id == TypeId::of::<VideoFrame>() {
            PortType::Video
        } else if type_id == TypeId::of::<DataFrame>() {
            PortType::Data
        } else {
            // Default to Audio for AudioFrame<N> and other types
            // Note: This is imperfect since we can't easily match AudioFrame<N> for all N
            PortType::Audio
        }
    }

    /// Connect processors by string port addresses - works in ANY runtime state
    ///
    /// This is the string-based connection API for external interfaces (MCP, Python, etc.).
    /// It accepts port addresses in the format "processor_id.port_name".
    ///
    /// # Example
    /// ```rust,ignore
    /// // Using string-based API
    /// runtime.connect_by_id("camera.video", "display.video")?;
    ///
    /// // Equivalent to typed API:
    /// // runtime.connect(camera.output("video"), display.input("video"))?;
    /// ```
    ///
    /// # Arguments
    /// * `source` - Source port address (e.g., "processor_0.video")
    /// * `destination` - Destination port address (e.g., "processor_1.video")
    pub fn connect_by_id(
        &mut self,
        source: &str,
        destination: &str,
    ) -> Result<super::types::ConnectionId> {
        match self.state {
            RuntimeState::Running | RuntimeState::Paused => {
                // Runtime active - wire immediately
                tracing::debug!(
                    "Wiring connection immediately (runtime {:?}): {} → {}",
                    self.state,
                    source,
                    destination
                );
                self.connect_internal(source, destination)
            }
            RuntimeState::Stopped | RuntimeState::Starting | RuntimeState::Stopping => {
                // Runtime not active - store for later wiring
                // Parse port addresses to extract processor IDs
                let (source_proc_id, source_port) = source.split_once('.').ok_or_else(|| {
                    StreamError::Configuration(format!(
                        "Invalid source format '{}'. Expected 'processor_id.port_name'",
                        source
                    ))
                })?;

                let (dest_proc_id, dest_port) = destination.split_once('.').ok_or_else(|| {
                    StreamError::Configuration(format!(
                        "Invalid destination format '{}'. Expected 'processor_id.port_name'",
                        destination
                    ))
                })?;

                let connection_id = format!("connection_{}", self.next_connection_id);
                self.next_connection_id += 1;

                let pending = PendingConnection::new(
                    connection_id.clone(),
                    source_proc_id.to_string(),
                    source_port.to_string(),
                    dest_proc_id.to_string(),
                    dest_port.to_string(),
                );

                self.pending_connections.push(pending);

                // Update graph (source of truth)
                // Note: We don't know the port type at this point, so we use Video as default
                // The actual port type will be validated during wiring at start()
                let graph_connection_id = crate::core::bus::connection_id::__private::new_unchecked(
                    connection_id.clone(),
                );

                if let Err(e) = self.graph.add_connection(
                    graph_connection_id,
                    source.to_string(),
                    destination.to_string(),
                    PortType::Video, // Default - will be validated at wiring time
                ) {
                    tracing::warn!(
                        "Failed to add connection to graph (will retry at start): {}",
                        e
                    );
                }
                self.dirty = true;

                tracing::debug!(
                    "Registered pending connection (runtime {:?}): {} → {}",
                    self.state,
                    source,
                    destination
                );

                Ok(connection_id)
            }
            RuntimeState::Restarting | RuntimeState::PurgeRebuild => {
                Err(StreamError::Configuration(format!(
                    "Cannot connect during state {:?} - wait for transition to complete",
                    self.state
                )))
            }
        }
    }

    /// Connect processors by string port addresses at runtime
    ///
    /// # Deprecated
    /// Use `connect_by_id()` instead, which works in all runtime states.
    #[deprecated(
        since = "0.2.0",
        note = "Use connect_by_id() instead, which works in all runtime states"
    )]
    pub fn connect_at_runtime(
        &mut self,
        source: &str,
        destination: &str,
    ) -> Result<super::types::ConnectionId> {
        self.connect_by_id(source, destination)
    }

    /// Internal connection wiring - creates the actual bus connection and wires ports
    ///
    /// This is called by:
    /// - `connect()` when runtime is Running/Paused
    /// - `wire_pending_connections()` at start()
    pub(super) fn connect_internal(
        &mut self,
        source: &str,
        destination: &str,
    ) -> Result<super::types::ConnectionId> {
        let (source_proc_id, source_port) = source.split_once('.').ok_or_else(|| {
            StreamError::Configuration(format!(
                "Invalid source format '{}'. Expected 'processor_id.port_name'",
                source
            ))
        })?;

        let (dest_proc_id, dest_port) = destination.split_once('.').ok_or_else(|| {
            StreamError::Configuration(format!(
                "Invalid destination format '{}'. Expected 'processor_id.port_name'",
                destination
            ))
        })?;

        // Generate connection ID early
        let connection_id = format!("connection_{}", self.next_connection_id);
        self.next_connection_id += 1;

        tracing::info!(
            "Connecting {} ({}:{}) → ({}:{}) [{}]",
            source,
            source_proc_id,
            source_port,
            dest_proc_id,
            dest_port,
            connection_id
        );

        // Send WillConnect events BEFORE wiring
        self.publish_will_connect_events(
            &connection_id,
            source_proc_id,
            source_port,
            dest_proc_id,
            dest_port,
        );

        let (source_processor, dest_processor) = {
            let processors = self.processors.lock();

            let source_handle = processors.get(source_proc_id).ok_or_else(|| {
                StreamError::Configuration(format!(
                    "Source processor '{}' not found",
                    source_proc_id
                ))
            })?;

            let dest_handle = processors.get(dest_proc_id).ok_or_else(|| {
                StreamError::Configuration(format!(
                    "Destination processor '{}' not found",
                    dest_proc_id
                ))
            })?;

            let source_proc = source_handle.processor.as_ref().ok_or_else(|| {
                StreamError::Runtime(format!(
                    "Source processor '{}' has no processor reference (not started?)",
                    source_proc_id
                ))
            })?;

            let dest_proc = dest_handle.processor.as_ref().ok_or_else(|| {
                StreamError::Runtime(format!(
                    "Destination processor '{}' has no processor reference (not started?)",
                    dest_proc_id
                ))
            })?;

            (
                std::sync::Arc::clone(source_proc),
                std::sync::Arc::clone(dest_proc),
            )
        };

        // Validate audio requirements if both processors have them
        {
            let source_guard = source_processor.lock();
            let dest_guard = dest_processor.lock();

            let source_descriptor = source_guard.descriptor_instance();
            let dest_descriptor = dest_guard.descriptor_instance();

            if let (Some(source_desc), Some(dest_desc)) = (source_descriptor, dest_descriptor) {
                if let (Some(source_audio), Some(dest_audio)) = (
                    &source_desc.audio_requirements,
                    &dest_desc.audio_requirements,
                ) {
                    if !source_audio.compatible_with(dest_audio) {
                        let error_msg = source_audio.compatibility_error(dest_audio);
                        return Err(StreamError::Configuration(format!(
                            "Audio requirements incompatible when connecting {} → {}: {}",
                            source, destination, error_msg
                        )));
                    }

                    tracing::debug!(
                        "Audio requirements validated: {} → {} (compatible)",
                        source_proc_id,
                        dest_proc_id
                    );
                }
            }
        }

        let (source_port_type, dest_port_type) = {
            let source_guard = source_processor.lock();
            let dest_guard = dest_processor.lock();

            let src_type = source_guard
                .get_output_port_type(source_port)
                .ok_or_else(|| {
                    StreamError::Configuration(format!(
                        "Source processor '{}' does not have output port '{}'",
                        source_proc_id, source_port
                    ))
                })?;

            let dst_type = dest_guard.get_input_port_type(dest_port).ok_or_else(|| {
                StreamError::Configuration(format!(
                    "Destination processor '{}' does not have input port '{}'",
                    dest_proc_id, dest_port
                ))
            })?;

            if !src_type.compatible_with(&dst_type) {
                return Err(StreamError::Configuration(format!(
                    "Port type mismatch: {} ({:?}) → {} ({:?})",
                    source, src_type, destination, dst_type
                )));
            }

            (src_type, dst_type)
        };

        // Create PortAddresses for the new generic API
        use crate::core::bus::PortAddress;
        let source_addr = PortAddress::new(source_proc_id.to_string(), source_port.to_string());
        let dest_addr = PortAddress::new(dest_proc_id.to_string(), dest_port.to_string());
        let capacity = source_port_type.default_capacity();

        // Phase 2: create_connection returns (OwnedProducer, OwnedConsumer)
        // We need to split them and pass separately via Box<dyn Any + Send>
        self.wire_connection_by_port_type(
            source_port_type,
            &source_addr,
            &dest_addr,
            capacity,
            &source_processor,
            &dest_processor,
            source_proc_id,
            dest_proc_id,
            source_port,
            dest_port,
        )?;

        tracing::info!(
            "Connected {} ({:?}) → {} ({:?}) via rtrb",
            source,
            source_port_type,
            destination,
            dest_port_type
        );

        // Store connection with metadata
        let connection = Connection::new(
            connection_id.clone(),
            source.to_string(),
            destination.to_string(),
            source_port_type,
            capacity,
        );

        {
            let mut connections = self.connections.lock();
            connections.insert(connection_id.clone(), connection.clone());
        }

        // Update connection index for both source and dest processors
        self.processor_connections
            .entry(connection.source_processor.clone())
            .or_default()
            .push(connection_id.clone());

        self.processor_connections
            .entry(connection.dest_processor.clone())
            .or_default()
            .push(connection_id.clone());

        // Send Connected events AFTER wiring complete
        self.publish_connected_events(
            &connection_id,
            source_proc_id,
            source_port,
            dest_proc_id,
            dest_port,
            source,
            destination,
        );

        // Update graph (source of truth for topology)
        let graph_connection_id =
            crate::core::bus::connection_id::__private::new_unchecked(connection_id.clone());
        if let Err(e) = self.graph.add_connection(
            graph_connection_id,
            source.to_string(),
            destination.to_string(),
            source_port_type,
        ) {
            tracing::warn!(
                "[{}] Failed to add connection to graph: {}",
                connection_id,
                e
            );
        }
        self.dirty = true;
        tracing::debug!("[{}] Added connection to graph", connection_id);

        tracing::info!("Registered runtime connection: {}", connection_id);
        Ok(connection_id)
    }

    /// Wire connection based on port type
    fn wire_connection_by_port_type(
        &mut self,
        port_type: PortType,
        source_addr: &crate::core::bus::PortAddress,
        dest_addr: &crate::core::bus::PortAddress,
        capacity: usize,
        source_processor: &std::sync::Arc<parking_lot::Mutex<super::types::DynProcessor>>,
        dest_processor: &std::sync::Arc<parking_lot::Mutex<super::types::DynProcessor>>,
        source_proc_id: &str,
        dest_proc_id: &str,
        source_port: &str,
        dest_port: &str,
    ) -> Result<()> {
        match port_type {
            PortType::Audio => {
                use crate::core::frames::AudioFrame;
                let (producer, consumer) = self.bus.create_connection::<AudioFrame>(
                    source_addr.clone(),
                    dest_addr.clone(),
                    capacity,
                )?;

                let mut source_guard = source_processor.lock();
                let success = source_guard.wire_output_producer(source_port, Box::new(producer));
                drop(source_guard);

                if !success {
                    return Err(StreamError::Configuration(format!(
                        "Failed to wire producer to output port '{}' on processor '{}'",
                        source_port, source_proc_id
                    )));
                }

                let mut dest_guard = dest_processor.lock();
                let success = dest_guard.wire_input_consumer(dest_port, Box::new(consumer));
                drop(dest_guard);

                if !success {
                    return Err(StreamError::Configuration(format!(
                        "Failed to wire consumer to input port '{}' on processor '{}'",
                        dest_port, dest_proc_id
                    )));
                }
            }
            PortType::Video => {
                use crate::core::frames::VideoFrame;
                let (producer, consumer) = self.bus.create_connection::<VideoFrame>(
                    source_addr.clone(),
                    dest_addr.clone(),
                    capacity,
                )?;

                let mut source_guard = source_processor.lock();
                let success = source_guard.wire_output_producer(source_port, Box::new(producer));
                drop(source_guard);

                if !success {
                    return Err(StreamError::Configuration(format!(
                        "Failed to wire producer to output port '{}' on processor '{}'",
                        source_port, source_proc_id
                    )));
                }

                let mut dest_guard = dest_processor.lock();
                let success = dest_guard.wire_input_consumer(dest_port, Box::new(consumer));
                drop(dest_guard);

                if !success {
                    return Err(StreamError::Configuration(format!(
                        "Failed to wire consumer to input port '{}' on processor '{}'",
                        dest_port, dest_proc_id
                    )));
                }
            }
            PortType::Data => {
                use crate::core::frames::DataFrame;
                let (producer, consumer) = self.bus.create_connection::<DataFrame>(
                    source_addr.clone(),
                    dest_addr.clone(),
                    capacity,
                )?;

                let mut source_guard = source_processor.lock();
                let success = source_guard.wire_output_producer(source_port, Box::new(producer));
                drop(source_guard);

                if !success {
                    return Err(StreamError::Configuration(format!(
                        "Failed to wire producer to output port '{}' on processor '{}'",
                        source_port, source_proc_id
                    )));
                }

                let mut dest_guard = dest_processor.lock();
                let success = dest_guard.wire_input_consumer(dest_port, Box::new(consumer));
                drop(dest_guard);

                if !success {
                    return Err(StreamError::Configuration(format!(
                        "Failed to wire consumer to input port '{}' on processor '{}'",
                        dest_port, dest_proc_id
                    )));
                }
            }
        }
        Ok(())
    }

    /// Publish WillConnect events to both processors
    fn publish_will_connect_events(
        &self,
        connection_id: &str,
        source_proc_id: &str,
        source_port: &str,
        dest_proc_id: &str,
        dest_port: &str,
    ) {
        use crate::core::pubsub::{Event, PortType as EventPortType, ProcessorEvent, EVENT_BUS};

        // Source processor (output port)
        EVENT_BUS.publish(
            &format!("processor:{}", source_proc_id),
            &Event::ProcessorEvent {
                processor_id: source_proc_id.to_string(),
                event: ProcessorEvent::WillConnect {
                    connection_id: connection_id.to_string(),
                    port_name: source_port.to_string(),
                    port_type: EventPortType::Output,
                },
            },
        );

        // Destination processor (input port)
        EVENT_BUS.publish(
            &format!("processor:{}", dest_proc_id),
            &Event::ProcessorEvent {
                processor_id: dest_proc_id.to_string(),
                event: ProcessorEvent::WillConnect {
                    connection_id: connection_id.to_string(),
                    port_name: dest_port.to_string(),
                    port_type: EventPortType::Input,
                },
            },
        );
    }

    /// Publish Connected events after wiring is complete
    fn publish_connected_events(
        &self,
        connection_id: &str,
        source_proc_id: &str,
        source_port: &str,
        dest_proc_id: &str,
        dest_port: &str,
        source: &str,
        destination: &str,
    ) {
        use crate::core::pubsub::{
            Event, PortType as EventPortType, ProcessorEvent, RuntimeEvent, EVENT_BUS,
        };

        // Source processor (output port)
        EVENT_BUS.publish(
            &format!("processor:{}", source_proc_id),
            &Event::ProcessorEvent {
                processor_id: source_proc_id.to_string(),
                event: ProcessorEvent::Connected {
                    connection_id: connection_id.to_string(),
                    port_name: source_port.to_string(),
                    port_type: EventPortType::Output,
                },
            },
        );

        // Destination processor (input port)
        EVENT_BUS.publish(
            &format!("processor:{}", dest_proc_id),
            &Event::ProcessorEvent {
                processor_id: dest_proc_id.to_string(),
                event: ProcessorEvent::Connected {
                    connection_id: connection_id.to_string(),
                    port_name: dest_port.to_string(),
                    port_type: EventPortType::Input,
                },
            },
        );

        // Broadcast RuntimeEvent
        EVENT_BUS.publish(
            "runtime:global",
            &Event::RuntimeGlobal(RuntimeEvent::ConnectionCreated {
                connection_id: connection_id.to_string(),
                from_port: source.to_string(),
                to_port: destination.to_string(),
            }),
        );
    }

    /// Disconnect a connection by port references
    ///
    /// This can disconnect both pre-runtime pending connections and runtime connections.
    pub fn disconnect<T: crate::core::bus::PortMessage>(
        &mut self,
        output: crate::core::handles::OutputPortRef<T>,
        input: crate::core::handles::InputPortRef<T>,
    ) -> Result<()> {
        let source = format!("{}.{}", output.processor_id(), output.port_name());
        let destination = format!("{}.{}", input.processor_id(), input.port_name());

        // Check if this is a pending connection (pre-runtime)
        if self.state != RuntimeState::Running {
            // Find and remove from pending_connections
            let removed_connection = self
                .pending_connections
                .iter()
                .position(|p| {
                    p.source_processor_id.as_str() == output.processor_id()
                        && p.source_port_name.as_str() == output.port_name()
                        && p.dest_processor_id.as_str() == input.processor_id()
                        && p.dest_port_name.as_str() == input.port_name()
                })
                .map(|idx| self.pending_connections.remove(idx));

            if let Some(removed) = removed_connection {
                tracing::info!(
                    "Removed pending connection {} ({} → {})",
                    removed.id,
                    source,
                    destination
                );
                return Ok(());
            }
        }

        // Otherwise, it's a runtime connection
        // Find the connection ID by searching connections HashMap
        let connection_id = {
            let connections = self.connections.lock();
            connections
                .iter()
                .find(|(_, conn)| conn.from_port == source && conn.to_port == destination)
                .map(|(id, _)| id.clone())
        };

        if let Some(id) = connection_id {
            self.disconnect_by_id(&id)
        } else {
            Err(StreamError::Configuration(format!(
                "Connection not found: {} → {}",
                source, destination
            )))
        }
    }

    /// Disconnect a connection by its ID
    pub fn disconnect_by_id(&mut self, connection_id: &super::types::ConnectionId) -> Result<()> {
        // Look up connection
        let connection = {
            let connections = self.connections.lock();
            connections.get(connection_id).cloned()
        };

        let connection = connection.ok_or_else(|| {
            StreamError::Configuration(format!("Connection {} not found", connection_id))
        })?;

        // Parse port addresses
        let (source_proc_id, source_port) =
            connection.from_port.split_once('.').ok_or_else(|| {
                StreamError::Configuration(format!(
                    "Invalid source format in connection: {}",
                    connection.from_port
                ))
            })?;

        let (dest_proc_id, dest_port) = connection.to_port.split_once('.').ok_or_else(|| {
            StreamError::Configuration(format!(
                "Invalid destination format in connection: {}",
                connection.to_port
            ))
        })?;

        tracing::info!(
            "Disconnecting {} ({}:{} → {}:{}) [{}]",
            connection.from_port,
            source_proc_id,
            source_port,
            dest_proc_id,
            dest_port,
            connection_id
        );

        // Send WillDisconnect events to both processors
        self.publish_will_disconnect_events(
            connection_id,
            source_proc_id,
            source_port,
            dest_proc_id,
            dest_port,
        );

        // Best-effort drain with timeout (500ms default)
        let drain_timeout = std::time::Duration::from_millis(500);

        // Give processors a moment to react to WillDisconnect
        std::thread::sleep(std::time::Duration::from_millis(10));

        // Attempt to drain ports (wait for buffers to empty)
        {
            let processors = self.processors.lock();

            if let Some(src_handle) = processors.get(source_proc_id) {
                if let Some(src_proc) = &src_handle.processor {
                    let src_guard = src_proc.lock();
                    // Note: drain methods would be called on the processor if implemented
                    // For now, just wait the timeout
                    drop(src_guard);
                }
            }

            if let Some(dest_handle) = processors.get(dest_proc_id) {
                if let Some(dest_proc) = &dest_handle.processor {
                    let dest_guard = dest_proc.lock();
                    // Note: drain methods would be called on the processor if implemented
                    drop(dest_guard);
                }
            }
        }

        std::thread::sleep(drain_timeout);

        // TODO: Clean up processor ports (remove producers/consumers)
        // This requires access to processor internals which isn't exposed through the trait
        // Full implementation would:
        // 1. Remove OwnedProducer from source processor's StreamOutput
        // 2. Remove wakeup channel from source processor's downstream_wakeups
        // 3. Remove OwnedConsumer from dest processor's StreamInput
        // 4. Call bus.disconnect() with the bus-level ConnectionId (not the runtime string ID)
        //
        // For now, we only clean up runtime-level tracking.
        // The connection will stop being used but resources won't be fully freed.
        tracing::warn!(
            "Disconnection partial: runtime tracking cleaned up, but port-level cleanup not yet implemented for {}.{} → {}.{}",
            source_proc_id,
            source_port,
            dest_proc_id,
            dest_port
        );

        // Remove from runtime connections
        {
            let mut connections = self.connections.lock();
            connections.remove(connection_id);
        }

        // Remove from connection index for both source and dest processors
        if let Some(connections_vec) = self
            .processor_connections
            .get_mut(&connection.source_processor)
        {
            connections_vec.retain(|id| id != connection_id);
        }
        if let Some(connections_vec) = self
            .processor_connections
            .get_mut(&connection.dest_processor)
        {
            connections_vec.retain(|id| id != connection_id);
        }

        // Send Disconnected events to both processors
        self.publish_disconnected_events(
            connection_id,
            &connection,
            source_proc_id,
            source_port,
            dest_proc_id,
            dest_port,
        );

        // Update graph (source of truth for topology)
        let graph_connection_id =
            crate::core::bus::connection_id::__private::new_unchecked(connection_id.clone());
        self.graph.remove_connection(&graph_connection_id);
        self.dirty = true;
        tracing::debug!("[{}] Removed connection from graph", connection_id);

        tracing::info!("Successfully disconnected connection: {}", connection_id);
        Ok(())
    }

    /// Publish WillDisconnect events to both processors
    fn publish_will_disconnect_events(
        &self,
        connection_id: &str,
        source_proc_id: &str,
        source_port: &str,
        dest_proc_id: &str,
        dest_port: &str,
    ) {
        use crate::core::pubsub::{Event, PortType as EventPortType, ProcessorEvent, EVENT_BUS};

        EVENT_BUS.publish(
            &format!("processor:{}", source_proc_id),
            &Event::ProcessorEvent {
                processor_id: source_proc_id.to_string(),
                event: ProcessorEvent::WillDisconnect {
                    connection_id: connection_id.to_string(),
                    port_name: source_port.to_string(),
                    port_type: EventPortType::Output,
                },
            },
        );

        EVENT_BUS.publish(
            &format!("processor:{}", dest_proc_id),
            &Event::ProcessorEvent {
                processor_id: dest_proc_id.to_string(),
                event: ProcessorEvent::WillDisconnect {
                    connection_id: connection_id.to_string(),
                    port_name: dest_port.to_string(),
                    port_type: EventPortType::Input,
                },
            },
        );
    }

    /// Publish Disconnected events after cleanup is complete
    fn publish_disconnected_events(
        &self,
        connection_id: &str,
        connection: &Connection,
        source_proc_id: &str,
        source_port: &str,
        dest_proc_id: &str,
        dest_port: &str,
    ) {
        use crate::core::pubsub::{
            Event, PortType as EventPortType, ProcessorEvent, RuntimeEvent, EVENT_BUS,
        };

        EVENT_BUS.publish(
            &format!("processor:{}", source_proc_id),
            &Event::ProcessorEvent {
                processor_id: source_proc_id.to_string(),
                event: ProcessorEvent::Disconnected {
                    connection_id: connection_id.to_string(),
                    port_name: source_port.to_string(),
                    port_type: EventPortType::Output,
                },
            },
        );

        EVENT_BUS.publish(
            &format!("processor:{}", dest_proc_id),
            &Event::ProcessorEvent {
                processor_id: dest_proc_id.to_string(),
                event: ProcessorEvent::Disconnected {
                    connection_id: connection_id.to_string(),
                    port_name: dest_port.to_string(),
                    port_type: EventPortType::Input,
                },
            },
        );

        // Broadcast RuntimeEvent
        EVENT_BUS.publish(
            "runtime:global",
            &Event::RuntimeGlobal(RuntimeEvent::ConnectionRemoved {
                connection_id: connection_id.to_string(),
                from_port: connection.from_port.clone(),
                to_port: connection.to_port.clone(),
            }),
        );
    }

    /// Wire all pending connections at start()
    ///
    /// This is called during runtime start to wire connections that were
    /// registered before the runtime was started.
    pub(super) fn wire_pending_connections(&mut self) -> Result<()> {
        use super::state::WakeupEvent;

        if self.pending_connections.is_empty() {
            tracing::debug!("No pending connections to wire");
            return Ok(());
        }

        tracing::info!(
            "Wiring {} pending connections...",
            self.pending_connections.len()
        );

        let connections_to_wire = std::mem::take(&mut self.pending_connections);

        for pending in connections_to_wire {
            let source = format!(
                "{}.{}",
                pending.source_processor_id, pending.source_port_name
            );
            let destination = format!("{}.{}", pending.dest_processor_id, pending.dest_port_name);

            tracing::info!("Wiring connection: {} → {}", source, destination);

            self.connect_internal(&source, &destination)?;

            {
                let processors = self.processors.lock();
                let source_handle = processors.get(&pending.source_processor_id);
                let dest_handle = processors.get(&pending.dest_processor_id);

                if let (Some(src), Some(dst)) = (source_handle, dest_handle) {
                    if let Some(src_proc) = src.processor.as_ref() {
                        let mut source_guard = src_proc.lock();
                        source_guard
                            .set_output_wakeup(&pending.source_port_name, dst.wakeup_tx.clone());

                        tracing::debug!(
                            "Wired wakeup notification: {} ({}) → {} ({})",
                            pending.source_processor_id,
                            pending.source_port_name,
                            pending.dest_processor_id,
                            pending.dest_port_name
                        );
                    }
                }
            }
        }

        tracing::info!("All pending connections wired successfully");

        tracing::debug!("Sending initialization wakeup to Pull mode processors");
        {
            let processors = self.processors.lock();
            for (proc_id, handle) in processors.iter() {
                if let Some(proc_ref) = &handle.processor {
                    let sched_config = proc_ref.lock().scheduling_config();
                    if matches!(
                        sched_config.mode,
                        crate::core::scheduling::SchedulingMode::Pull
                    ) {
                        if let Err(e) = handle.wakeup_tx.send(WakeupEvent::DataAvailable) {
                            tracing::warn!(
                                "[{}] Failed to send Pull mode initialization wakeup: {}",
                                proc_id,
                                e
                            );
                        } else {
                            tracing::debug!("[{}] Sent Pull mode initialization wakeup", proc_id);
                        }
                    }
                }
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_connect_requires_valid_format() {
        let mut runtime = StreamRuntime::new();

        // Invalid source format
        let result = runtime.connect_by_id("invalid", "processor_1.port");
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Invalid source format"));

        // Invalid destination format
        let result = runtime.connect_by_id("processor_0.port", "invalid");
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Invalid destination format"));
    }

    #[test]
    fn test_connect_by_id_creates_pending_connection_when_stopped() {
        let mut runtime = StreamRuntime::new();

        let result = runtime.connect_by_id("processor_0.video", "processor_1.video");
        assert!(result.is_ok());

        let connection_id = result.unwrap();
        assert!(connection_id.starts_with("connection_"));

        // Should be in pending_connections
        assert_eq!(runtime.pending_connections.len(), 1);
    }

    #[test]
    fn test_connect_fails_during_restarting() {
        let mut runtime = StreamRuntime::new();
        runtime.state = RuntimeState::Restarting;

        let result = runtime.connect_by_id("processor_0.video", "processor_1.video");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Restarting"));
    }

    #[test]
    fn test_connect_fails_during_purge_rebuild() {
        let mut runtime = StreamRuntime::new();
        runtime.state = RuntimeState::PurgeRebuild;

        let result = runtime.connect_by_id("processor_0.video", "processor_1.video");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("PurgeRebuild"));
    }

    #[test]
    fn test_disconnect_by_id_not_found() {
        let mut runtime = StreamRuntime::new();

        let result = runtime.disconnect_by_id(&"nonexistent".to_string());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[test]
    fn test_pending_connections_accumulate() {
        let mut runtime = StreamRuntime::new();

        runtime.connect_by_id("proc_0.out", "proc_1.in").unwrap();
        runtime.connect_by_id("proc_1.out", "proc_2.in").unwrap();
        runtime.connect_by_id("proc_2.out", "proc_3.in").unwrap();

        assert_eq!(runtime.pending_connections.len(), 3);
    }

    #[test]
    fn test_connect_by_id_increments_connection_id() {
        let mut runtime = StreamRuntime::new();

        let id1 = runtime.connect_by_id("proc_0.out", "proc_1.in").unwrap();
        let id2 = runtime.connect_by_id("proc_1.out", "proc_2.in").unwrap();
        let id3 = runtime.connect_by_id("proc_2.out", "proc_3.in").unwrap();

        assert_eq!(id1, "connection_0");
        assert_eq!(id2, "connection_1");
        assert_eq!(id3, "connection_2");
    }
}
