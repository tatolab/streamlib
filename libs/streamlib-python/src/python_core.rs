// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Shared core logic for Python subprocess processors.
//!
//! TODO: This module needs complete redesign to use the XPC-based SubprocessRhi
//! architecture instead of the old Unix socket IPC approach.
//!
//! The new architecture will use:
//! - XPC broker for connection registry (runtime + processor endpoints)
//! - XPC channels for direct frame transfer (IOSurface/xpc_shmem)
//! - Tokio async tasks for subprocess bridge coordination
//!
//! See Notion document for full architecture design.

use streamlib::core::{ProcessorUniqueId, RuntimeUniqueId};
use streamlib::{Result, StreamError, VideoFrame};

use crate::venv_manager::VenvManager;

/// Configuration for Python subprocess processors.
pub use crate::python_processor_core::PythonProcessorConfig;

/// Shared core for Python subprocess processors.
///
/// TODO: Redesign to use XPC-based SubprocessRhi instead of Unix sockets.
pub struct PythonCore {
    /// Processor configuration.
    pub config: PythonProcessorConfig,
    /// Virtual environment manager.
    venv_manager: Option<VenvManager>,
    /// Runtime ID for unique naming.
    runtime_id: Option<RuntimeUniqueId>,
    /// Processor ID for unique naming.
    processor_id: Option<ProcessorUniqueId>,
}

impl Default for PythonCore {
    fn default() -> Self {
        Self {
            config: PythonProcessorConfig::default(),
            venv_manager: None,
            runtime_id: None,
            processor_id: None,
        }
    }
}

impl PythonCore {
    /// Create a new subprocess core with the given config.
    pub fn new(config: PythonProcessorConfig) -> Self {
        Self {
            config,
            venv_manager: None,
            runtime_id: None,
            processor_id: None,
        }
    }

    /// Common setup logic for all subprocess processors.
    ///
    /// TODO: Implement using XPC-based SubprocessRhi.
    pub fn setup_common(
        &mut self,
        config: PythonProcessorConfig,
        runtime_id: &RuntimeUniqueId,
        processor_id: &ProcessorUniqueId,
    ) -> Result<()> {
        self.config = config;
        self.runtime_id = Some(runtime_id.clone());
        self.processor_id = Some(processor_id.clone());

        tracing::info!(
            "PythonCore: Setting up processor {} in runtime {}",
            processor_id.as_str(),
            runtime_id.as_str()
        );

        // Create and setup venv
        let mut venv_manager = VenvManager::new(runtime_id, processor_id)?;
        let venv_path = venv_manager.ensure_venv(&self.config.project_path)?;

        tracing::info!("PythonCore: Venv ready at '{}'", venv_path.display());

        self.venv_manager = Some(venv_manager);

        // TODO: Implement XPC-based subprocess spawning and bridge setup
        // - Ensure broker is running
        // - Create XpcChannel as runtime host
        // - Spawn Python subprocess with XPC connection info
        // - Wait for subprocess to register with broker
        // - Establish direct XPC connection
        // - Wait for bridge_ready signal

        Err(StreamError::NotSupported(
            "Python subprocess needs redesign for XPC-based architecture".into(),
        ))
    }

    /// Send an input VideoFrame to the subprocess.
    ///
    /// TODO: Implement using XPC frame transport.
    pub fn send_input_frame(&mut self, _port: &str, _frame: &VideoFrame) -> Result<()> {
        Err(StreamError::NotSupported(
            "Python subprocess needs redesign for XPC-based architecture".into(),
        ))
    }

    /// Receive an output VideoFrame from the subprocess.
    ///
    /// TODO: Implement using XPC frame transport.
    pub fn recv_output_frame(&mut self, _port: &str) -> Result<Option<VideoFrame>> {
        Err(StreamError::NotSupported(
            "Python subprocess needs redesign for XPC-based architecture".into(),
        ))
    }

    /// Trigger a process cycle in the subprocess.
    ///
    /// TODO: Implement using XPC control messages.
    pub fn process(&mut self) -> Result<()> {
        Err(StreamError::NotSupported(
            "Python subprocess needs redesign for XPC-based architecture".into(),
        ))
    }

    /// Send pause message to subprocess.
    pub fn on_pause(&mut self) -> Result<()> {
        tracing::debug!("PythonCore: on_pause (not implemented)");
        Ok(())
    }

    /// Send resume message to subprocess.
    pub fn on_resume(&mut self) -> Result<()> {
        tracing::debug!("PythonCore: on_resume (not implemented)");
        Ok(())
    }

    /// Common teardown logic.
    pub fn teardown_common(&mut self) -> Result<()> {
        // Cleanup venv
        if let Some(ref mut venv_manager) = self.venv_manager {
            if let Err(e) = venv_manager.cleanup() {
                tracing::warn!("PythonCore: Venv cleanup failed: {}", e);
            }
        }
        self.venv_manager = None;

        tracing::info!(
            "PythonCore: Teardown complete for '{}'",
            self.config.class_name
        );

        Ok(())
    }
}
