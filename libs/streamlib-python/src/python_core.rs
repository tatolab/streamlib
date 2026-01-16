// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Shared core logic for Python subprocess processors.
//!
//! Implements the XPC bridge architecture for host-to-subprocess communication:
//! - gRPC for signaling and coordination with broker
//! - XPC for frame transport (IOSurface/xpc_shmem)

use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;
use tokio::sync::oneshot;
use tracing::{debug, error, info, trace, warn};

use streamlib::core::{ProcessorUniqueId, RuntimeUniqueId};
use streamlib::{Result, StreamError, VideoFrame};

use crate::venv_manager::VenvManager;

/// Configuration for Python subprocess processors.
pub use crate::python_processor_core::PythonProcessorConfig;

/// ACK ping magic bytes: "SLP" = StreamLib Ping
const ACK_PING_MAGIC: [u8; 3] = [0x53, 0x4C, 0x50];

/// ACK pong magic bytes: "SLA" = StreamLib Ack
const ACK_PONG_MAGIC: [u8; 3] = [0x53, 0x4C, 0x41];

/// Default broker gRPC endpoint.
fn default_broker_endpoint() -> String {
    let port = std::env::var("STREAMLIB_BROKER_PORT").unwrap_or_else(|_| "50051".to_string());
    format!("http://127.0.0.1:{}", port)
}

/// Bridge state for tracking connection lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BridgeState {
    /// Initial state, not yet set up.
    NotStarted,
    /// Connection allocated with broker, XPC setup in progress.
    Connecting,
    /// XPC endpoint stored, waiting for client.
    AwaitingClient,
    /// Client connected, ACK exchange in progress.
    AckExchange,
    /// Bridge is ready for frame transfer.
    Ready,
    /// Bridge setup failed.
    Failed,
}

/// Shared core for Python subprocess processors.
///
/// Implements XPC-based bridge for host-to-subprocess frame transport.
pub struct PythonCore {
    /// Processor configuration.
    pub config: PythonProcessorConfig,
    /// Virtual environment manager.
    venv_manager: Option<VenvManager>,
    /// Runtime ID for unique naming.
    runtime_id: Option<RuntimeUniqueId>,
    /// Processor ID for unique naming.
    processor_id: Option<ProcessorUniqueId>,
    /// Connection ID allocated by broker.
    connection_id: Option<String>,
    /// Bridge ready flag (atomic for fast checking in hot path).
    bridge_ready: Arc<AtomicBool>,
    /// Current bridge state.
    bridge_state: Arc<Mutex<BridgeState>>,
    /// Handle to spawned subprocess.
    subprocess: Option<Child>,
    /// Shutdown signal sender for bridge task.
    shutdown_tx: Option<oneshot::Sender<()>>,
}

impl Default for PythonCore {
    fn default() -> Self {
        Self {
            config: PythonProcessorConfig::default(),
            venv_manager: None,
            runtime_id: None,
            processor_id: None,
            connection_id: None,
            bridge_ready: Arc::new(AtomicBool::new(false)),
            bridge_state: Arc::new(Mutex::new(BridgeState::NotStarted)),
            subprocess: None,
            shutdown_tx: None,
        }
    }
}

impl PythonCore {
    /// Create a new subprocess core with the given config.
    pub fn new(config: PythonProcessorConfig) -> Self {
        Self {
            config,
            ..Default::default()
        }
    }

    /// Check if the bridge is ready for frame transfer.
    #[inline]
    pub fn is_bridge_ready(&self) -> bool {
        self.bridge_ready.load(Ordering::Acquire)
    }

    /// Get the current bridge state.
    pub fn bridge_state(&self) -> BridgeState {
        *self.bridge_state.lock()
    }

    /// Common setup logic for all subprocess processors.
    ///
    /// This method:
    /// 1. Allocates a connection with the broker
    /// 2. Creates an anonymous XPC listener
    /// 3. Stores the endpoint with the broker
    /// 4. Spawns the Python subprocess
    /// 5. Starts the bridge task for ACK exchange
    #[cfg(target_os = "macos")]
    pub fn setup_common(
        &mut self,
        config: PythonProcessorConfig,
        runtime_id: &RuntimeUniqueId,
        processor_id: &ProcessorUniqueId,
    ) -> Result<()> {
        use streamlib_broker::proto::broker_service_client::BrokerServiceClient;

        self.config = config;
        self.runtime_id = Some(runtime_id.clone());
        self.processor_id = Some(processor_id.clone());

        info!(
            "PythonCore: Setting up processor {} in runtime {}",
            processor_id.as_str(),
            runtime_id.as_str()
        );

        *self.bridge_state.lock() = BridgeState::Connecting;

        // Create and setup venv
        let mut venv_manager = VenvManager::new(runtime_id, processor_id)?;
        let venv_path = venv_manager.ensure_venv(&self.config.project_path)?;

        info!("PythonCore: Venv ready at '{}'", venv_path.display());
        self.venv_manager = Some(venv_manager);

        // Get broker endpoint from environment or use default
        let broker_endpoint = default_broker_endpoint();
        info!("PythonCore: Using broker endpoint: {}", broker_endpoint);

        // Use tokio runtime for async gRPC calls
        let rt = tokio::runtime::Handle::try_current()
            .map_err(|_| StreamError::Configuration("No tokio runtime available".to_string()))?;

        // Step 1: Allocate connection via gRPC
        let runtime_id_str = runtime_id.as_str().to_string();
        let processor_id_str = processor_id.as_str().to_string();
        let broker_endpoint_clone = broker_endpoint.clone();

        let connection_id = rt.block_on(async {
            let mut client = BrokerServiceClient::connect(broker_endpoint_clone)
                .await
                .map_err(|e| {
                    StreamError::Configuration(format!("Failed to connect to broker gRPC: {}", e))
                })?;

            let request = tonic::Request::new(streamlib_broker::proto::AllocateConnectionRequest {
                runtime_id: runtime_id_str,
                processor_id: processor_id_str,
            });

            let response = client.allocate_connection(request).await.map_err(|e| {
                StreamError::Configuration(format!("AllocateConnection failed: {}", e))
            })?;

            let resp = response.into_inner();
            if !resp.success {
                return Err(StreamError::Configuration(format!(
                    "AllocateConnection rejected: {}",
                    resp.error
                )));
            }

            Ok::<String, StreamError>(resp.connection_id)
        })?;

        info!("PythonCore: Allocated connection_id: {}", connection_id);
        self.connection_id = Some(connection_id.clone());

        // Step 2: Report HostAlive via gRPC
        let connection_id_clone = connection_id.clone();
        let broker_endpoint_clone = broker_endpoint.clone();

        rt.block_on(async {
            let mut client = BrokerServiceClient::connect(broker_endpoint_clone)
                .await
                .map_err(|e| {
                    StreamError::Configuration(format!("Failed to connect to broker gRPC: {}", e))
                })?;

            let request = tonic::Request::new(streamlib_broker::proto::HostAliveRequest {
                connection_id: connection_id_clone,
            });

            client
                .host_alive(request)
                .await
                .map_err(|e| StreamError::Configuration(format!("HostAlive failed: {}", e)))?;

            Ok::<(), StreamError>(())
        })?;

        debug!("PythonCore: Reported HostAlive");

        // Step 3: XPC endpoint storage is deferred to Phase 4c
        // For now, we rely on gRPC-only coordination:
        // 1. Host allocates connection, spawns subprocess
        // 2. Subprocess calls ClientAlive via gRPC
        // 3. Host polls GetClientStatus until client ACKs
        // 4. XPC frame transport will be added in Phase 4c

        *self.bridge_state.lock() = BridgeState::AwaitingClient;

        // Step 4: Spawn subprocess with environment variables
        let venv_path = self
            .venv_manager
            .as_ref()
            .ok_or_else(|| StreamError::Configuration("Venv manager not initialized".to_string()))?
            .venv_path();

        // Construct python path from venv
        let python_path = venv_path.join("bin").join("python");

        let subprocess_runner = self.config.project_path.join("_subprocess_runner.py");
        if !subprocess_runner.exists() {
            return Err(StreamError::Configuration(format!(
                "Subprocess runner not found at: {}",
                subprocess_runner.display()
            )));
        }

        // Get broker XPC service name from environment
        let broker_xpc_service = std::env::var("STREAMLIB_BROKER_XPC_SERVICE")
            .unwrap_or_else(|_| "com.tatolab.streamlib.broker".to_string());

        info!(
            "PythonCore: Spawning subprocess with python: {}",
            python_path.display()
        );

        let child = Command::new(&python_path)
            .arg(&subprocess_runner)
            .arg("--class-name")
            .arg(&self.config.class_name)
            .env("STREAMLIB_CONNECTION_ID", &connection_id)
            .env("STREAMLIB_BROKER_ENDPOINT", &broker_endpoint)
            .env("STREAMLIB_BROKER_XPC_SERVICE", &broker_xpc_service)
            .current_dir(&self.config.project_path)
            .spawn()
            .map_err(|e| {
                StreamError::Configuration(format!("Failed to spawn subprocess: {}", e))
            })?;

        info!("PythonCore: Subprocess spawned with PID: {}", child.id());
        self.subprocess = Some(child);

        // Step 5: Start bridge task for polling client status and ACK exchange
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        self.shutdown_tx = Some(shutdown_tx);

        let bridge_ready = self.bridge_ready.clone();
        let bridge_state = self.bridge_state.clone();
        let connection_id_for_task = connection_id.clone();
        let broker_endpoint_for_task = broker_endpoint.clone();

        // Spawn bridge task
        rt.spawn(async move {
            if let Err(e) = run_bridge_task(
                connection_id_for_task,
                broker_endpoint_for_task,
                bridge_ready,
                bridge_state,
                shutdown_rx,
            )
            .await
            {
                error!("PythonCore: Bridge task failed: {}", e);
            }
        });

        info!("PythonCore: Setup complete, bridge task started");
        Ok(())
    }

    /// Non-macOS stub.
    #[cfg(not(target_os = "macos"))]
    pub fn setup_common(
        &mut self,
        _config: PythonProcessorConfig,
        _runtime_id: &RuntimeUniqueId,
        _processor_id: &ProcessorUniqueId,
    ) -> Result<()> {
        Err(StreamError::NotSupported(
            "Python subprocess requires macOS".into(),
        ))
    }

    /// Send an input VideoFrame to the subprocess.
    ///
    /// Frame gate: drops frames silently if bridge not ready.
    pub fn send_input_frame(&mut self, port: &str, frame: &VideoFrame) -> Result<()> {
        if !self.is_bridge_ready() {
            trace!(
                "PythonCore: Dropping frame on port '{}' - bridge not ready",
                port
            );
            return Ok(());
        }

        // TODO: Implement XPC frame send via xpc_frame_transport
        // For now, log that we would send the frame
        trace!(
            "PythonCore: Would send frame {} on port '{}' via XPC",
            frame.frame_number,
            port
        );

        Ok(())
    }

    /// Receive an output VideoFrame from the subprocess.
    ///
    /// Frame gate: returns None if bridge not ready.
    pub fn recv_output_frame(&mut self, port: &str) -> Result<Option<VideoFrame>> {
        if !self.is_bridge_ready() {
            trace!("PythonCore: No frame on port '{}' - bridge not ready", port);
            return Ok(None);
        }

        // TODO: Implement XPC frame receive via xpc_frame_transport
        trace!("PythonCore: Would receive frame on port '{}' via XPC", port);

        Ok(None)
    }

    /// Trigger a process cycle in the subprocess.
    ///
    /// For Manual/Reactive processors, this sends a "process" control message.
    pub fn process(&mut self) -> Result<()> {
        if !self.is_bridge_ready() {
            trace!("PythonCore: Skipping process() - bridge not ready");
            return Ok(());
        }

        // TODO: Implement XPC control message for process trigger
        trace!("PythonCore: Would send process() trigger via XPC");

        Ok(())
    }

    /// Send pause message to subprocess.
    pub fn on_pause(&mut self) -> Result<()> {
        if !self.is_bridge_ready() {
            debug!("PythonCore: on_pause - bridge not ready");
            return Ok(());
        }

        // TODO: Implement XPC control message for pause
        debug!("PythonCore: Would send on_pause via XPC");
        Ok(())
    }

    /// Send resume message to subprocess.
    pub fn on_resume(&mut self) -> Result<()> {
        if !self.is_bridge_ready() {
            debug!("PythonCore: on_resume - bridge not ready");
            return Ok(());
        }

        // TODO: Implement XPC control message for resume
        debug!("PythonCore: Would send on_resume via XPC");
        Ok(())
    }

    /// Common teardown logic.
    pub fn teardown_common(&mut self) -> Result<()> {
        info!("PythonCore: Teardown starting");

        // Send shutdown signal to bridge task
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }

        // Close connection with broker
        #[cfg(target_os = "macos")]
        if let Some(connection_id) = &self.connection_id {
            let rt = tokio::runtime::Handle::try_current();
            if let Ok(rt) = rt {
                let broker_endpoint = default_broker_endpoint();
                let connection_id = connection_id.clone();

                let _ = rt.block_on(async {
                    if let Ok(mut client) = streamlib_broker::proto::broker_service_client::BrokerServiceClient::connect(broker_endpoint).await {
                        let request = tonic::Request::new(streamlib_broker::proto::CloseConnectionRequest {
                            connection_id,
                            reason: "shutdown".to_string(),
                        });
                        let _ = client.close_connection(request).await;
                    }
                });
            }
        }

        // Terminate subprocess
        if let Some(mut child) = self.subprocess.take() {
            info!("PythonCore: Terminating subprocess PID: {}", child.id());
            let _ = child.kill();
            let _ = child.wait();
        }

        // Cleanup venv
        if let Some(ref mut venv_manager) = self.venv_manager {
            if let Err(e) = venv_manager.cleanup() {
                warn!("PythonCore: Venv cleanup failed: {}", e);
            }
        }
        self.venv_manager = None;

        self.bridge_ready.store(false, Ordering::Release);
        *self.bridge_state.lock() = BridgeState::NotStarted;

        info!(
            "PythonCore: Teardown complete for '{}'",
            self.config.class_name
        );

        Ok(())
    }
}

/// Bridge task that polls for client connection and performs ACK exchange.
#[cfg(target_os = "macos")]
async fn run_bridge_task(
    connection_id: String,
    broker_endpoint: String,
    bridge_ready: Arc<AtomicBool>,
    bridge_state: Arc<Mutex<BridgeState>>,
    mut shutdown_rx: oneshot::Receiver<()>,
) -> Result<()> {
    use streamlib_broker::proto::broker_service_client::BrokerServiceClient;

    info!(
        "PythonCore: Bridge task started for connection {}",
        connection_id
    );

    // Poll for client to become alive and ready
    let poll_interval = std::time::Duration::from_millis(100);
    let timeout = std::time::Duration::from_secs(30);
    let start = std::time::Instant::now();

    loop {
        // Check for shutdown signal
        if shutdown_rx.try_recv().is_ok() {
            info!("PythonCore: Bridge task received shutdown signal");
            return Ok(());
        }

        // Check timeout
        if start.elapsed() > timeout {
            *bridge_state.lock() = BridgeState::Failed;
            return Err(StreamError::Runtime(
                "Client did not connect within timeout".into(),
            ));
        }

        // Poll client status
        let mut client = BrokerServiceClient::connect(broker_endpoint.clone())
            .await
            .map_err(|e| {
                StreamError::Configuration(format!("Failed to connect to broker: {}", e))
            })?;

        let request = tonic::Request::new(streamlib_broker::proto::GetClientStatusRequest {
            connection_id: connection_id.clone(),
        });

        let response = client.get_client_status(request).await;

        match response {
            Ok(resp) => {
                let status = resp.into_inner();
                trace!("PythonCore: Client state: {}", status.client_state);

                // Client states: "pending", "alive", "xpc_endpoint_received", "acked"
                if status.client_state == "acked" {
                    info!("PythonCore: Client ACKed, connection ready!");
                    break;
                }

                if status.client_state == "alive" || status.client_state == "xpc_endpoint_received"
                {
                    *bridge_state.lock() = BridgeState::AckExchange;
                    // Client is connecting, keep polling
                }
            }
            Err(e) => {
                warn!("PythonCore: GetClientStatus failed: {}", e);
            }
        }

        tokio::time::sleep(poll_interval).await;
    }

    // Mark host as ACKed
    let mut client = BrokerServiceClient::connect(broker_endpoint.clone())
        .await
        .map_err(|e| StreamError::Configuration(format!("Failed to connect to broker: {}", e)))?;

    let request = tonic::Request::new(streamlib_broker::proto::MarkAckedRequest {
        connection_id: connection_id.clone(),
        side: "host".to_string(),
    });

    client
        .mark_acked(request)
        .await
        .map_err(|e| StreamError::Configuration(format!("MarkAcked failed: {}", e)))?;

    // Bridge is now ready
    bridge_ready.store(true, Ordering::Release);
    *bridge_state.lock() = BridgeState::Ready;

    info!("PythonCore: Bridge ready for connection {}", connection_id);

    // Keep task alive until shutdown
    let _ = shutdown_rx.await;
    info!("PythonCore: Bridge task shutting down");

    Ok(())
}

/// Non-macOS stub for bridge task.
#[cfg(not(target_os = "macos"))]
async fn run_bridge_task(
    _connection_id: String,
    _broker_endpoint: String,
    _bridge_ready: Arc<AtomicBool>,
    _bridge_state: Arc<Mutex<BridgeState>>,
    _shutdown_rx: oneshot::Receiver<()>,
) -> Result<()> {
    Err(StreamError::NotSupported(
        "Python subprocess requires macOS".into(),
    ))
}
