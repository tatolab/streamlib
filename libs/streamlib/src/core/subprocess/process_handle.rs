// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Subprocess lifecycle management.

use crate::core::{Result, StreamError};
use std::process::{Child, Command, ExitStatus};
use std::time::Duration;

/// Handle to a running subprocess.
pub struct ProcessHandle {
    child: Child,
    name: String,
}

impl ProcessHandle {
    /// Spawn a subprocess from a command.
    pub fn spawn(mut command: Command, name: &str) -> Result<Self> {
        let child = command.spawn().map_err(|e| {
            StreamError::Configuration(format!("Failed to spawn subprocess '{}': {}", name, e))
        })?;

        tracing::info!("Spawned subprocess '{}' with PID {}", name, child.id());

        Ok(Self {
            child,
            name: name.to_string(),
        })
    }

    /// Get the process ID.
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Get the process name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Check if the process is still running.
    pub fn is_running(&mut self) -> bool {
        self.child.try_wait().ok().flatten().is_none()
    }

    /// Wait for the process to exit with optional timeout.
    pub fn wait(&mut self) -> Result<ExitStatus> {
        self.child.wait().map_err(|e| {
            StreamError::Configuration(format!(
                "Failed to wait for subprocess '{}': {}",
                self.name, e
            ))
        })
    }

    /// Try to wait for the process without blocking.
    pub fn try_wait(&mut self) -> Result<Option<ExitStatus>> {
        self.child.try_wait().map_err(|e| {
            StreamError::Configuration(format!(
                "Failed to check subprocess '{}' status: {}",
                self.name, e
            ))
        })
    }

    /// Request graceful shutdown.
    pub fn request_shutdown(&mut self) {
        tracing::info!("Requesting shutdown of subprocess '{}'", self.name);
        // The actual shutdown signal is sent via IPC (Shutdown message)
        // This method is just for logging/state tracking
    }

    /// Force kill the process.
    pub fn kill(&mut self) -> Result<()> {
        tracing::warn!("Force killing subprocess '{}'", self.name);
        self.child.kill().map_err(|e| {
            StreamError::Configuration(format!("Failed to kill subprocess '{}': {}", self.name, e))
        })
    }

    /// Graceful shutdown with timeout, then force kill.
    pub fn shutdown(&mut self, timeout: Duration) -> Result<ExitStatus> {
        // First, try graceful shutdown (wait for process to exit)
        let start = std::time::Instant::now();

        while start.elapsed() < timeout {
            if let Some(status) = self.try_wait()? {
                tracing::info!(
                    "Subprocess '{}' exited with status: {:?}",
                    self.name,
                    status
                );
                return Ok(status);
            }
            std::thread::sleep(Duration::from_millis(10));
        }

        // Timeout reached, force kill
        tracing::warn!(
            "Subprocess '{}' did not exit within {:?}, force killing",
            self.name,
            timeout
        );
        self.kill()?;
        self.wait()
    }
}

impl Drop for ProcessHandle {
    fn drop(&mut self) {
        // Try to clean up the process
        if self.is_running() {
            tracing::warn!(
                "ProcessHandle for '{}' dropped while still running, killing",
                self.name
            );
            self.kill().ok();
        }
    }
}

/// Configuration for subprocess spawning.
#[derive(Debug, Clone)]
pub struct SubprocessConfig {
    /// Processor name.
    pub processor_name: String,
    /// Path to the processor project/script.
    pub project_path: std::path::PathBuf,
    /// Environment variables to set.
    pub env: std::collections::HashMap<String, String>,
    /// Working directory for the subprocess.
    pub working_dir: Option<std::path::PathBuf>,
    /// Shutdown timeout.
    pub shutdown_timeout: Duration,
}

impl SubprocessConfig {
    /// Create a new subprocess config.
    pub fn new(processor_name: &str, project_path: impl Into<std::path::PathBuf>) -> Self {
        Self {
            processor_name: processor_name.to_string(),
            project_path: project_path.into(),
            env: std::collections::HashMap::new(),
            working_dir: None,
            shutdown_timeout: Duration::from_secs(5),
        }
    }

    /// Set an environment variable.
    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(key.into(), value.into());
        self
    }

    /// Set the working directory.
    pub fn with_working_dir(mut self, dir: impl Into<std::path::PathBuf>) -> Self {
        self.working_dir = Some(dir.into());
        self
    }

    /// Set the shutdown timeout.
    pub fn with_shutdown_timeout(mut self, timeout: Duration) -> Self {
        self.shutdown_timeout = timeout;
        self
    }
}
