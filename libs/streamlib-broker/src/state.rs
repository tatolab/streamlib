// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Thread-safe state for broker diagnostics.

use std::collections::HashMap;
use std::sync::atomic::AtomicU64;
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

/// Metadata about a registered surface for cross-process GPU sharing.
#[derive(Clone, Debug)]
pub struct SurfaceMetadata {
    /// The surface ID (UUID).
    pub surface_id: String,
    /// The runtime that registered this surface.
    pub runtime_id: String,
    /// The mach port send right for the IOSurface.
    pub mach_port: u32,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Pixel format (e.g., "BGRA", "NV12").
    pub format: String,
    /// When the surface was registered.
    pub registered_at: Instant,
    /// Number of times this surface has been checked out.
    pub checkout_count: u64,
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
    surfaces: RwLock<HashMap<String, SurfaceMetadata>>,
    started_at: Instant,
    connection_counter: AtomicU64,
    surface_counter: AtomicU64,
}

impl BrokerState {
    /// Create a new broker state.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(BrokerStateInner {
                runtimes: RwLock::new(HashMap::new()),
                subprocesses: RwLock::new(HashMap::new()),
                connections: RwLock::new(HashMap::new()),
                surfaces: RwLock::new(HashMap::new()),
                started_at: Instant::now(),
                connection_counter: AtomicU64::new(0),
                surface_counter: AtomicU64::new(0),
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

    /// Unregister a runtime and release its surfaces.
    pub fn unregister_runtime(&self, runtime_id: &str) {
        self.inner.runtimes.write().remove(runtime_id);

        #[cfg(target_os = "macos")]
        {
            let released = self.release_surfaces_for_runtime(runtime_id);
            if released > 0 {
                tracing::info!(
                    "Released {} surface(s) for unregistered runtime {}",
                    released,
                    runtime_id
                );
            }
        }
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
        let mut pruned_ids = Vec::new();

        {
            let mut runtimes = self.inner.runtimes.write();
            runtimes.retain(|_id, metadata| {
                let alive = is_process_alive(metadata.pid);
                if !alive {
                    pruned.push(metadata.name.clone());
                    pruned_ids.push(metadata.runtime_id.clone());
                }
                alive
            });
        }

        // Release surfaces for pruned runtimes (after dropping runtimes lock)
        #[cfg(target_os = "macos")]
        for runtime_id in &pruned_ids {
            let released = self.release_surfaces_for_runtime(runtime_id);
            if released > 0 {
                tracing::info!(
                    "Released {} surface(s) for pruned runtime {}",
                    released,
                    runtime_id
                );
            }
        }

        // Also clean up orphaned surfaces whose runtime_id doesn't match
        // any currently registered runtime (e.g. from runtimes that were
        // removed in a previous prune or unregister before surface cleanup
        // was added). Skip when no runtimes are registered — standalone
        // pipelines (without api_server) register surfaces via XPC but never
        // register the runtime via gRPC, so an empty set would remove everything.
        #[cfg(target_os = "macos")]
        {
            let registered_ids: std::collections::HashSet<String> =
                self.inner.runtimes.read().keys().cloned().collect();
            if !registered_ids.is_empty() {
                let mut surfaces = self.inner.surfaces.write();
                let before = surfaces.len();
                surfaces.retain(|_, metadata| registered_ids.contains(&metadata.runtime_id));
                let orphaned = before - surfaces.len();
                if orphaned > 0 {
                    tracing::info!("Released {} orphaned surface(s)", orphaned);
                }
            }
        }

        pruned
    }

    // =========================================================================
    // Surface Store (Cross-Process GPU Surface Sharing)
    // =========================================================================

    /// Register a surface with client-provided ID.
    ///
    /// The client generates the surface_id (UUID) and provides it along with the mach_port.
    /// Returns true if registration succeeded, false if surface_id already exists.
    #[cfg(target_os = "macos")]
    pub fn register_surface(
        &self,
        surface_id: &str,
        runtime_id: &str,
        mach_port: u32,
        width: u32,
        height: u32,
        format: &str,
    ) -> bool {
        use std::sync::atomic::Ordering;

        let mut surfaces = self.inner.surfaces.write();

        // Don't overwrite existing registrations
        if surfaces.contains_key(surface_id) {
            return false;
        }

        self.inner.surface_counter.fetch_add(1, Ordering::Relaxed);

        let metadata = SurfaceMetadata {
            surface_id: surface_id.to_string(),
            runtime_id: runtime_id.to_string(),
            mach_port,
            width,
            height,
            format: format.to_string(),
            registered_at: Instant::now(),
            checkout_count: 0,
        };

        surfaces.insert(surface_id.to_string(), metadata);
        true
    }

    /// Get the mach port for a surface ID (for check_out).
    #[cfg(target_os = "macos")]
    pub fn get_surface_mach_port(&self, surface_id: &str) -> Option<u32> {
        let mut surfaces = self.inner.surfaces.write();
        if let Some(metadata) = surfaces.get_mut(surface_id) {
            metadata.checkout_count += 1;
            Some(metadata.mach_port)
        } else {
            None
        }
    }

    /// Release a surface by ID.
    #[cfg(target_os = "macos")]
    pub fn release_surface(&self, surface_id: &str, runtime_id: &str) -> bool {
        let mut surfaces = self.inner.surfaces.write();
        if let Some(metadata) = surfaces.get(surface_id) {
            if metadata.runtime_id == runtime_id {
                surfaces.remove(surface_id);
                return true;
            }
        }
        false
    }

    /// Release all surfaces for a runtime.
    #[cfg(target_os = "macos")]
    pub fn release_surfaces_for_runtime(&self, runtime_id: &str) -> usize {
        let mut surfaces = self.inner.surfaces.write();
        let before = surfaces.len();
        surfaces.retain(|_, metadata| metadata.runtime_id != runtime_id);
        before - surfaces.len()
    }

    /// Get all surfaces.
    #[cfg(target_os = "macos")]
    pub fn get_surfaces(&self) -> Vec<SurfaceMetadata> {
        self.inner.surfaces.read().values().cloned().collect()
    }

    /// Get surface count.
    #[cfg(target_os = "macos")]
    pub fn surface_count(&self) -> usize {
        self.inner.surfaces.read().len()
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
