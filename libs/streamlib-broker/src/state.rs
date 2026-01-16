// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Thread-safe state for broker diagnostics.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use parking_lot::RwLock;

/// Metadata about a registered runtime.
#[derive(Clone, Debug)]
pub struct RuntimeMetadata {
    pub runtime_id: String,
    pub name: String,
    pub api_endpoint: String,
    pub log_path: String,
    pub pid: i32,
    pub registered_at: Instant,
}

/// Metadata about a registered subprocess/processor.
#[derive(Clone, Debug)]
pub struct SubprocessMetadata {
    pub subprocess_key: String,
    pub runtime_id: String,
    pub processor_id: String,
    pub registered_at: Instant,
}

/// Metadata about an established connection.
#[derive(Clone, Debug)]
pub struct ConnectionMetadata {
    pub connection_id: String,
    pub runtime_id: String,
    pub processor_id: String,
    pub role: String,
    pub established_at: Instant,
}

/// Thread-safe state for broker diagnostics.
#[derive(Clone)]
pub struct BrokerState {
    inner: Arc<BrokerStateInner>,
}

struct BrokerStateInner {
    runtimes: RwLock<HashMap<String, RuntimeMetadata>>,
    subprocesses: RwLock<HashMap<String, SubprocessMetadata>>,
    connections: RwLock<HashMap<String, ConnectionMetadata>>,
    started_at: Instant,
    connection_counter: std::sync::atomic::AtomicU64,
}

impl BrokerState {
    /// Create a new broker state.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(BrokerStateInner {
                runtimes: RwLock::new(HashMap::new()),
                subprocesses: RwLock::new(HashMap::new()),
                connections: RwLock::new(HashMap::new()),
                started_at: Instant::now(),
                connection_counter: std::sync::atomic::AtomicU64::new(0),
            }),
        }
    }

    /// Get broker uptime in seconds.
    pub fn uptime_secs(&self) -> i64 {
        self.inner.started_at.elapsed().as_secs() as i64
    }

    /// Register a runtime with minimal metadata (legacy, for backwards compatibility).
    pub fn register_runtime(&self, runtime_id: &str) {
        self.register_runtime_with_metadata(runtime_id, runtime_id, "", "", 0);
    }

    /// Register a runtime with full metadata.
    pub fn register_runtime_with_metadata(
        &self,
        runtime_id: &str,
        name: &str,
        api_endpoint: &str,
        log_path: &str,
        pid: i32,
    ) {
        let metadata = RuntimeMetadata {
            runtime_id: runtime_id.to_string(),
            name: name.to_string(),
            api_endpoint: api_endpoint.to_string(),
            log_path: log_path.to_string(),
            pid,
            registered_at: Instant::now(),
        };
        self.inner
            .runtimes
            .write()
            .insert(runtime_id.to_string(), metadata);
    }

    /// Get a runtime by name.
    pub fn get_runtime_by_name(&self, name: &str) -> Option<RuntimeMetadata> {
        self.inner
            .runtimes
            .read()
            .values()
            .find(|r| r.name == name)
            .cloned()
    }

    /// Get a runtime by ID.
    pub fn get_runtime_by_id(&self, runtime_id: &str) -> Option<RuntimeMetadata> {
        self.inner.runtimes.read().get(runtime_id).cloned()
    }

    /// Unregister a runtime.
    pub fn unregister_runtime(&self, runtime_id: &str) {
        self.inner.runtimes.write().remove(runtime_id);
    }

    /// Get all registered runtimes.
    pub fn get_runtimes(&self) -> Vec<RuntimeMetadata> {
        self.inner.runtimes.read().values().cloned().collect()
    }

    /// Get runtime count.
    pub fn runtime_count(&self) -> usize {
        self.inner.runtimes.read().len()
    }

    /// Register a subprocess.
    pub fn register_subprocess(&self, subprocess_key: &str) {
        // Parse subprocess_key format: "runtime_id:processor_id"
        let parts: Vec<&str> = subprocess_key.splitn(2, ':').collect();
        let (runtime_id, processor_id) = if parts.len() == 2 {
            (parts[0].to_string(), parts[1].to_string())
        } else {
            (subprocess_key.to_string(), String::new())
        };

        let metadata = SubprocessMetadata {
            subprocess_key: subprocess_key.to_string(),
            runtime_id,
            processor_id,
            registered_at: Instant::now(),
        };
        self.inner
            .subprocesses
            .write()
            .insert(subprocess_key.to_string(), metadata);
    }

    /// Unregister a subprocess.
    pub fn unregister_subprocess(&self, subprocess_key: &str) {
        self.inner.subprocesses.write().remove(subprocess_key);
    }

    /// Get all registered subprocesses.
    pub fn get_subprocesses(&self) -> Vec<SubprocessMetadata> {
        self.inner.subprocesses.read().values().cloned().collect()
    }

    /// Get subprocesses for a specific runtime.
    pub fn get_subprocesses_for_runtime(&self, runtime_id: &str) -> Vec<SubprocessMetadata> {
        self.inner
            .subprocesses
            .read()
            .values()
            .filter(|s| s.runtime_id == runtime_id)
            .cloned()
            .collect()
    }

    /// Get subprocess count for a runtime.
    pub fn subprocess_count_for_runtime(&self, runtime_id: &str) -> usize {
        self.inner
            .subprocesses
            .read()
            .values()
            .filter(|s| s.runtime_id == runtime_id)
            .count()
    }

    /// Record a connection when an endpoint is retrieved.
    pub fn record_connection(&self, runtime_id: &str, processor_id: &str, role: &str) -> String {
        let counter = self
            .inner
            .connection_counter
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let connection_id = format!("conn-{}", counter);

        let metadata = ConnectionMetadata {
            connection_id: connection_id.clone(),
            runtime_id: runtime_id.to_string(),
            processor_id: processor_id.to_string(),
            role: role.to_string(),
            established_at: Instant::now(),
        };
        self.inner
            .connections
            .write()
            .insert(connection_id.clone(), metadata);

        connection_id
    }

    /// Remove a connection.
    pub fn remove_connection(&self, connection_id: &str) {
        self.inner.connections.write().remove(connection_id);
    }

    /// Get all connections.
    pub fn get_connections(&self) -> Vec<ConnectionMetadata> {
        self.inner.connections.read().values().cloned().collect()
    }

    /// Get connections for a specific runtime.
    pub fn get_connections_for_runtime(&self, runtime_id: &str) -> Vec<ConnectionMetadata> {
        self.inner
            .connections
            .read()
            .values()
            .filter(|c| c.runtime_id == runtime_id)
            .cloned()
            .collect()
    }

    /// Get connection count for a runtime.
    pub fn connection_count_for_runtime(&self, runtime_id: &str) -> usize {
        self.inner
            .connections
            .read()
            .values()
            .filter(|c| c.runtime_id == runtime_id)
            .count()
    }

    /// Prune dead runtimes by checking if their PIDs still exist.
    /// Returns the names of pruned runtimes.
    pub fn prune_dead_runtimes(&self) -> Vec<String> {
        let mut pruned = Vec::new();
        let mut runtimes = self.inner.runtimes.write();

        runtimes.retain(|_id, metadata| {
            let alive = is_process_alive(metadata.pid);
            if !alive {
                pruned.push(metadata.name.clone());
            }
            alive
        });

        pruned
    }
}

/// Check if a process is alive using kill(pid, 0).
/// Signal 0 doesn't send any signal - it just checks if the process exists.
fn is_process_alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    // SAFETY: kill with signal 0 is safe - it only checks process existence
    unsafe { libc::kill(pid, 0) == 0 }
}

impl Default for BrokerState {
    fn default() -> Self {
        Self::new()
    }
}
