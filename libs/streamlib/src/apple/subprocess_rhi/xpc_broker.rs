// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! XPC broker for runtime endpoint exchange.
//!
//! The broker is a launchd service that allows runtimes to register their
//! XPC endpoints and subprocesses to look them up for direct connection.

use std::collections::HashMap;
use std::ffi::{c_void, CString};
use std::fs;
use std::path::PathBuf;
use std::ptr::{null, null_mut};
use std::sync::Arc;

use parking_lot::RwLock;
use tracing::{debug, error, info, trace, warn};

use launchd::{Launchd, MachServiceEntry};
use xpc_bindgen::{
    _xpc_type_connection, _xpc_type_dictionary, _xpc_type_endpoint, _xpc_type_error,
    xpc_connection_create_mach_service, xpc_connection_resume, xpc_connection_send_message,
    xpc_connection_send_message_with_reply_sync, xpc_connection_set_event_handler,
    xpc_connection_t, xpc_dictionary_create, xpc_dictionary_create_reply,
    xpc_dictionary_get_string, xpc_dictionary_get_value, xpc_dictionary_set_string,
    xpc_dictionary_set_value, xpc_get_type, xpc_object_t, xpc_release, xpc_retain, xpc_type_t,
};

use crate::core::error::StreamError;
use crate::core::subprocess_rhi::{BrokerInstallStatus, SubprocessRhiBroker};

use super::block_helpers::{
    get_ns_concrete_stack_block, BlockDescriptor, BlockLiteral, _Block_copy,
};

/// Broker service name for launchd registration.
pub const BROKER_SERVICE_NAME: &str = "com.tatolab.streamlib.runtime";

/// XPC broker implementation for macOS.
pub struct XpcBroker {
    /// Connection to the broker service.
    connection: xpc_connection_t,
}

impl XpcBroker {
    /// Create a new broker client connection.
    pub fn connect() -> Result<Self, StreamError> {
        unsafe {
            let service_name = CString::new(BROKER_SERVICE_NAME)
                .map_err(|e| StreamError::Configuration(format!("Invalid service name: {}", e)))?;

            let conn = xpc_connection_create_mach_service(service_name.as_ptr(), null_mut(), 0);

            if conn.is_null() {
                return Err(StreamError::Configuration(
                    "Failed to connect to broker service".to_string(),
                ));
            }

            // Set up error handler
            static DESCRIPTOR: BlockDescriptor = BlockDescriptor {
                reserved: 0,
                size: std::mem::size_of::<BlockLiteral<()>>(),
            };

            unsafe extern "C" fn handler(_block: *mut BlockLiteral<()>, event: xpc_object_t) {
                let obj_type = xpc_get_type(event);
                let err_type = std::ptr::addr_of!(_xpc_type_error) as xpc_type_t;

                if obj_type == err_type {
                    debug!("[XpcBroker] Connection error event");
                }
            }

            let block = BlockLiteral {
                isa: get_ns_concrete_stack_block(),
                flags: 0,
                reserved: 0,
                invoke: handler,
                descriptor: &DESCRIPTOR,
                context: (),
            };

            let heap_block = _Block_copy(&block as *const _ as *const c_void);
            xpc_connection_set_event_handler(conn, heap_block);
            xpc_connection_resume(conn);

            trace!("[XpcBroker] Connected to broker service");
            Ok(Self { connection: conn })
        }
    }

    /// Get the plist path for the broker service.
    fn get_plist_path() -> PathBuf {
        let home = std::env::var("HOME").expect("HOME not set");
        PathBuf::from(home)
            .join("Library")
            .join("LaunchAgents")
            .join(format!("{}.plist", BROKER_SERVICE_NAME))
    }

    /// Get the launchctl domain target (gui/{uid}).
    fn get_domain_target() -> String {
        let uid = unsafe { libc::getuid() };
        format!("gui/{}", uid)
    }

    /// Check if the broker is already running.
    fn is_broker_running() -> bool {
        let output = std::process::Command::new("launchctl")
            .args(["list", BROKER_SERVICE_NAME])
            .output();

        match output {
            Ok(result) => result.status.success(),
            Err(_) => false,
        }
    }

    /// Get the global streamlib bin directory (~/.streamlib/bin).
    fn get_streamlib_bin_dir() -> PathBuf {
        dirs::home_dir()
            .expect("HOME directory not found")
            .join(".streamlib")
            .join("bin")
    }

    /// Install the broker service plist and bootstrap it.
    fn install_broker() -> Result<(), StreamError> {
        let plist_path = Self::get_plist_path();

        info!(
            "[XpcBroker] Installing broker plist at: {}",
            plist_path.display()
        );

        // Create MachServices map
        let mut mach_services = HashMap::new();
        mach_services.insert(
            BROKER_SERVICE_NAME.to_string(),
            MachServiceEntry::Boolean(true),
        );

        // Find the broker binary - it could be in same dir or parent dir (for tests in deps/)
        let current_exe = std::env::current_exe().map_err(|e| {
            StreamError::Configuration(format!("Failed to get current exe path: {}", e))
        })?;

        let exe_dir = current_exe.parent().ok_or_else(|| {
            StreamError::Configuration("Current exe has no parent directory".to_string())
        })?;

        // Try same directory first, then parent (for test binaries in deps/)
        let source_broker_path = {
            let same_dir = exe_dir.join("streamlib-broker");
            if same_dir.exists() {
                same_dir
            } else if let Some(parent) = exe_dir.parent() {
                let parent_dir = parent.join("streamlib-broker");
                if parent_dir.exists() {
                    parent_dir
                } else {
                    return Err(StreamError::Configuration(format!(
                        "Broker binary not found at: {} or {}. Build with: cargo build --bin streamlib-broker",
                        same_dir.display(),
                        parent_dir.display()
                    )));
                }
            } else {
                return Err(StreamError::Configuration(format!(
                    "Broker binary not found at: {}. Build with: cargo build --bin streamlib-broker",
                    same_dir.display()
                )));
            }
        };

        // Copy broker to global ~/.streamlib/bin directory
        let bin_dir = Self::get_streamlib_bin_dir();
        fs::create_dir_all(&bin_dir).map_err(|e| {
            StreamError::Configuration(format!("Failed to create streamlib bin dir: {}", e))
        })?;

        let installed_broker_path = bin_dir.join("streamlib-broker");

        // Copy if not exists or if source is newer
        let should_copy = if installed_broker_path.exists() {
            // Check if source is newer
            let source_meta = fs::metadata(&source_broker_path).ok();
            let dest_meta = fs::metadata(&installed_broker_path).ok();
            match (source_meta, dest_meta) {
                (Some(s), Some(d)) => s.modified().ok() > d.modified().ok(),
                _ => true,
            }
        } else {
            true
        };

        if should_copy {
            info!(
                "[XpcBroker] Installing broker binary to: {}",
                installed_broker_path.display()
            );
            fs::copy(&source_broker_path, &installed_broker_path).map_err(|e| {
                StreamError::Configuration(format!("Failed to copy broker binary: {}", e))
            })?;
        }

        let broker_path_str = installed_broker_path.to_str().ok_or_else(|| {
            StreamError::Configuration("Broker path contains invalid UTF-8".to_string())
        })?;

        let plist = Launchd::new(BROKER_SERVICE_NAME, broker_path_str)
            .map_err(|e| StreamError::Configuration(format!("Failed to create plist: {}", e)))?
            .with_mach_services(mach_services)
            .with_program_arguments(vec![
                broker_path_str.to_string(),
                "--subprocess-broker".to_string(),
            ]);

        // Ensure LaunchAgents directory exists
        if let Some(parent) = plist_path.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                StreamError::Configuration(format!("Failed to create LaunchAgents dir: {}", e))
            })?;
        }

        let mut file = fs::File::create(&plist_path).map_err(|e| {
            StreamError::Configuration(format!("Failed to create plist file: {}", e))
        })?;

        plist
            .to_writer_xml(&mut file)
            .map_err(|e| StreamError::Configuration(format!("Failed to write plist XML: {}", e)))?;

        drop(file);

        // Bootstrap the service
        let domain_target = Self::get_domain_target();

        // First try to unload if it exists (ignore errors)
        let _ = std::process::Command::new("launchctl")
            .args(["bootout", &domain_target, plist_path.to_str().unwrap()])
            .output();

        let output = std::process::Command::new("launchctl")
            .args(["bootstrap", &domain_target, plist_path.to_str().unwrap()])
            .output()
            .map_err(|e| {
                StreamError::Configuration(format!("Failed to run launchctl bootstrap: {}", e))
            })?;

        if output.status.success() {
            info!("[XpcBroker] Broker service bootstrapped successfully");
            info!("[XpcBroker] User may see 'Streamlib Runtime' authorization popup");
            Ok(())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            Err(StreamError::Configuration(format!(
                "Failed to bootstrap broker service: {}",
                stderr
            )))
        }
    }

    /// Register a subprocess endpoint with the broker (subprocess-listener pattern).
    /// Called by the subprocess after creating its listener.
    /// Uses synchronous send to ensure broker confirms registration.
    pub fn register_subprocess_endpoint(
        &self,
        subprocess_key: &str,
        endpoint: *mut c_void,
    ) -> Result<(), StreamError> {
        unsafe {
            trace!(
                "[XpcBroker] Registering subprocess endpoint: {}",
                subprocess_key
            );

            let msg = xpc_dictionary_create(null_mut(), null_mut(), 0);

            let type_key = CString::new("type").unwrap();
            let type_val = CString::new("register_subprocess").unwrap();
            xpc_dictionary_set_string(msg, type_key.as_ptr(), type_val.as_ptr());

            let key_cstr = CString::new("subprocess_key").unwrap();
            let key_val = CString::new(subprocess_key).map_err(|e| {
                StreamError::Configuration(format!("Invalid subprocess_key: {}", e))
            })?;
            xpc_dictionary_set_string(msg, key_cstr.as_ptr(), key_val.as_ptr());

            let endpoint_key = CString::new("endpoint").unwrap();
            xpc_dictionary_set_value(msg, endpoint_key.as_ptr(), endpoint);

            let reply = xpc_connection_send_message_with_reply_sync(self.connection, msg);
            xpc_release(msg);

            if reply.is_null() {
                return Err(StreamError::Configuration(
                    "No reply from broker for subprocess registration".to_string(),
                ));
            }

            let dict_type = std::ptr::addr_of!(_xpc_type_dictionary) as xpc_type_t;
            let reply_type = xpc_get_type(reply);

            if reply_type != dict_type {
                xpc_release(reply);
                return Err(StreamError::Configuration(
                    "Invalid reply type from broker for subprocess registration".to_string(),
                ));
            }

            let error_key = CString::new("error").unwrap();
            let error_val = xpc_dictionary_get_string(reply, error_key.as_ptr());

            if !error_val.is_null() {
                let error_str = std::ffi::CStr::from_ptr(error_val)
                    .to_str()
                    .unwrap_or("unknown");
                xpc_release(reply);
                return Err(StreamError::Configuration(format!(
                    "Broker subprocess registration error: {}",
                    error_str
                )));
            }

            xpc_release(reply);
            info!(
                "[XpcBroker] Registered subprocess endpoint: {}",
                subprocess_key
            );
            Ok(())
        }
    }

    /// Get a subprocess endpoint from the broker (host polls for this).
    /// Returns Ok(Some(endpoint)) if found, Ok(None) if not yet registered, Err on failure.
    pub fn get_subprocess_endpoint(
        &self,
        subprocess_key: &str,
    ) -> Result<Option<*mut c_void>, StreamError> {
        unsafe {
            trace!(
                "[XpcBroker] Requesting subprocess endpoint: {}",
                subprocess_key
            );

            let msg = xpc_dictionary_create(null_mut(), null_mut(), 0);

            let type_key = CString::new("type").unwrap();
            let type_val = CString::new("get_subprocess_endpoint").unwrap();
            xpc_dictionary_set_string(msg, type_key.as_ptr(), type_val.as_ptr());

            let key_cstr = CString::new("subprocess_key").unwrap();
            let key_val = CString::new(subprocess_key).map_err(|e| {
                StreamError::Configuration(format!("Invalid subprocess_key: {}", e))
            })?;
            xpc_dictionary_set_string(msg, key_cstr.as_ptr(), key_val.as_ptr());

            let reply = xpc_connection_send_message_with_reply_sync(self.connection, msg);
            xpc_release(msg);

            if reply.is_null() {
                return Err(StreamError::Configuration(
                    "No reply from broker".to_string(),
                ));
            }

            let dict_type = std::ptr::addr_of!(_xpc_type_dictionary) as xpc_type_t;
            let reply_type = xpc_get_type(reply);

            if reply_type != dict_type {
                xpc_release(reply);
                return Err(StreamError::Configuration(
                    "Invalid reply type from broker".to_string(),
                ));
            }

            // Check for "not_found" - this is expected when subprocess hasn't registered yet
            let error_key = CString::new("error").unwrap();
            let error_val = xpc_dictionary_get_string(reply, error_key.as_ptr());

            if !error_val.is_null() {
                let error_str = std::ffi::CStr::from_ptr(error_val)
                    .to_str()
                    .unwrap_or("unknown");
                xpc_release(reply);

                if error_str == "not_found" {
                    // Subprocess not yet registered - caller should retry
                    return Ok(None);
                }

                return Err(StreamError::Configuration(format!(
                    "Broker error: {}",
                    error_str
                )));
            }

            // Get endpoint
            let endpoint_key = CString::new("endpoint").unwrap();
            let endpoint = xpc_dictionary_get_value(reply, endpoint_key.as_ptr());

            if endpoint.is_null() {
                xpc_release(reply);
                return Err(StreamError::Configuration(
                    "No endpoint in broker reply".to_string(),
                ));
            }

            let endpoint_type = std::ptr::addr_of!(_xpc_type_endpoint) as xpc_type_t;
            if xpc_get_type(endpoint) != endpoint_type {
                xpc_release(reply);
                return Err(StreamError::Configuration(
                    "Invalid endpoint type in broker reply".to_string(),
                ));
            }

            xpc_retain(endpoint);
            xpc_release(reply);

            info!("[XpcBroker] Got subprocess endpoint: {}", subprocess_key);
            Ok(Some(endpoint as *mut c_void))
        }
    }

    /// Unregister a subprocess endpoint from the broker.
    pub fn unregister_subprocess_endpoint(&self, subprocess_key: &str) -> Result<(), StreamError> {
        unsafe {
            trace!(
                "[XpcBroker] Unregistering subprocess endpoint: {}",
                subprocess_key
            );

            let msg = xpc_dictionary_create(null_mut(), null_mut(), 0);

            let type_key = CString::new("type").unwrap();
            let type_val = CString::new("unregister_subprocess").unwrap();
            xpc_dictionary_set_string(msg, type_key.as_ptr(), type_val.as_ptr());

            let key_cstr = CString::new("subprocess_key").unwrap();
            let key_val = CString::new(subprocess_key).map_err(|e| {
                StreamError::Configuration(format!("Invalid subprocess_key: {}", e))
            })?;
            xpc_dictionary_set_string(msg, key_cstr.as_ptr(), key_val.as_ptr());

            xpc_connection_send_message(self.connection, msg);
            xpc_release(msg);

            info!(
                "[XpcBroker] Unregistered subprocess endpoint: {}",
                subprocess_key
            );
            Ok(())
        }
    }
}

impl SubprocessRhiBroker for XpcBroker {
    fn ensure_running() -> Result<BrokerInstallStatus, StreamError> {
        if Self::is_broker_running() {
            trace!("[XpcBroker] Broker already running");
            return Ok(BrokerInstallStatus::AlreadyRunning);
        }

        info!("[XpcBroker] Broker not running, installing...");
        Self::install_broker()?;

        // Give broker time to start
        std::thread::sleep(std::time::Duration::from_millis(500));

        if Self::is_broker_running() {
            info!("[XpcBroker] Broker installed and running");
            Ok(BrokerInstallStatus::Installed)
        } else {
            Err(StreamError::Configuration(
                "Broker failed to start after installation".to_string(),
            ))
        }
    }

    fn register_endpoint(
        &self,
        runtime_id: &str,
        endpoint: *mut c_void,
    ) -> Result<(), StreamError> {
        unsafe {
            trace!(
                "[XpcBroker] Registering endpoint for runtime: {}",
                runtime_id
            );

            let msg = xpc_dictionary_create(null_mut(), null_mut(), 0);

            let type_key = CString::new("type").unwrap();
            let type_val = CString::new("register_runtime").unwrap();
            xpc_dictionary_set_string(msg, type_key.as_ptr(), type_val.as_ptr());

            let id_key = CString::new("runtime_id").unwrap();
            let id_val = CString::new(runtime_id)
                .map_err(|e| StreamError::Configuration(format!("Invalid runtime_id: {}", e)))?;
            xpc_dictionary_set_string(msg, id_key.as_ptr(), id_val.as_ptr());

            let endpoint_key = CString::new("endpoint").unwrap();
            xpc_dictionary_set_value(msg, endpoint_key.as_ptr(), endpoint);

            // Use synchronous send to ensure broker has processed registration
            // before we return. This eliminates race conditions where subprocess
            // tries to get_endpoint before broker has stored it.
            let reply = xpc_connection_send_message_with_reply_sync(self.connection, msg);
            xpc_release(msg);

            if reply.is_null() {
                return Err(StreamError::Configuration(
                    "No reply from broker for registration".to_string(),
                ));
            }

            let dict_type = std::ptr::addr_of!(_xpc_type_dictionary) as xpc_type_t;
            let reply_type = xpc_get_type(reply);

            if reply_type != dict_type {
                xpc_release(reply);
                return Err(StreamError::Configuration(
                    "Invalid reply type from broker for registration".to_string(),
                ));
            }

            // Check for error in reply
            let error_key = CString::new("error").unwrap();
            let error_val = xpc_dictionary_get_string(reply, error_key.as_ptr());

            if !error_val.is_null() {
                let error_str = std::ffi::CStr::from_ptr(error_val)
                    .to_str()
                    .unwrap_or("unknown");
                xpc_release(reply);
                return Err(StreamError::Configuration(format!(
                    "Broker registration error: {}",
                    error_str
                )));
            }

            xpc_release(reply);

            info!(
                "[XpcBroker] Registered endpoint for runtime: {}",
                runtime_id
            );
            Ok(())
        }
    }

    fn get_endpoint(&self, runtime_id: &str) -> Result<*mut c_void, StreamError> {
        unsafe {
            trace!(
                "[XpcBroker] Requesting endpoint for runtime: {}",
                runtime_id
            );

            let msg = xpc_dictionary_create(null_mut(), null_mut(), 0);

            let type_key = CString::new("type").unwrap();
            let type_val = CString::new("get_endpoint").unwrap();
            xpc_dictionary_set_string(msg, type_key.as_ptr(), type_val.as_ptr());

            let id_key = CString::new("runtime_id").unwrap();
            let id_val = CString::new(runtime_id)
                .map_err(|e| StreamError::Configuration(format!("Invalid runtime_id: {}", e)))?;
            xpc_dictionary_set_string(msg, id_key.as_ptr(), id_val.as_ptr());

            // Send with synchronous reply
            let reply = xpc_connection_send_message_with_reply_sync(self.connection, msg);
            xpc_release(msg);

            if reply.is_null() {
                return Err(StreamError::Configuration(
                    "No reply from broker".to_string(),
                ));
            }

            let dict_type = std::ptr::addr_of!(_xpc_type_dictionary) as xpc_type_t;
            let reply_type = xpc_get_type(reply);

            if reply_type != dict_type {
                xpc_release(reply);
                return Err(StreamError::Configuration(
                    "Invalid reply type from broker".to_string(),
                ));
            }

            // Check for error
            let error_key = CString::new("error").unwrap();
            let error_val = xpc_dictionary_get_string(reply, error_key.as_ptr());

            if !error_val.is_null() {
                let error_str = std::ffi::CStr::from_ptr(error_val)
                    .to_str()
                    .unwrap_or("unknown");
                xpc_release(reply);
                return Err(StreamError::Configuration(format!(
                    "Broker error: {}",
                    error_str
                )));
            }

            // Get endpoint
            let endpoint_key = CString::new("endpoint").unwrap();
            let endpoint = xpc_dictionary_get_value(reply, endpoint_key.as_ptr());

            if endpoint.is_null() {
                xpc_release(reply);
                return Err(StreamError::Configuration(
                    "No endpoint in broker reply".to_string(),
                ));
            }

            let endpoint_type = std::ptr::addr_of!(_xpc_type_endpoint) as xpc_type_t;
            if xpc_get_type(endpoint) != endpoint_type {
                xpc_release(reply);
                return Err(StreamError::Configuration(
                    "Invalid endpoint type in broker reply".to_string(),
                ));
            }

            // Retain endpoint before releasing reply
            xpc_retain(endpoint);
            xpc_release(reply);

            info!("[XpcBroker] Got endpoint for runtime: {}", runtime_id);
            Ok(endpoint as *mut c_void)
        }
    }

    fn unregister_endpoint(&self, runtime_id: &str) -> Result<(), StreamError> {
        unsafe {
            trace!(
                "[XpcBroker] Unregistering endpoint for runtime: {}",
                runtime_id
            );

            let msg = xpc_dictionary_create(null_mut(), null_mut(), 0);

            let type_key = CString::new("type").unwrap();
            let type_val = CString::new("unregister_runtime").unwrap();
            xpc_dictionary_set_string(msg, type_key.as_ptr(), type_val.as_ptr());

            let id_key = CString::new("runtime_id").unwrap();
            let id_val = CString::new(runtime_id)
                .map_err(|e| StreamError::Configuration(format!("Invalid runtime_id: {}", e)))?;
            xpc_dictionary_set_string(msg, id_key.as_ptr(), id_val.as_ptr());

            xpc_connection_send_message(self.connection, msg);
            xpc_release(msg);

            info!(
                "[XpcBroker] Unregistered endpoint for runtime: {}",
                runtime_id
            );
            Ok(())
        }
    }
}

// Safety: XPC connections are thread-safe
unsafe impl Send for XpcBroker {}
unsafe impl Sync for XpcBroker {}

/// Broker listener state for when running as the broker service.
pub struct XpcBrokerListener {
    /// Registered runtime endpoints (host-side, old pattern - kept for backwards compat).
    registered_runtimes: Arc<RwLock<HashMap<String, xpc_object_t>>>,
    /// Registered subprocess endpoints (subprocess-listener pattern).
    /// Key is "runtime_id:processor_id" to allow multiple subprocesses per runtime.
    registered_subprocesses: Arc<RwLock<HashMap<String, xpc_object_t>>>,
    /// Shared state for diagnostics (gRPC).
    state: super::broker_state::BrokerState,
}

impl XpcBrokerListener {
    /// Create a new broker listener with shared diagnostics state.
    pub fn new(state: super::broker_state::BrokerState) -> Self {
        Self {
            registered_runtimes: Arc::new(RwLock::new(HashMap::new())),
            registered_subprocesses: Arc::new(RwLock::new(HashMap::new())),
            state,
        }
    }

    /// Register a runtime endpoint.
    pub fn register_runtime(&self, runtime_id: &str, endpoint: xpc_object_t) {
        unsafe {
            xpc_retain(endpoint);
        }
        self.registered_runtimes
            .write()
            .insert(runtime_id.to_string(), endpoint);
        self.state.register_runtime(runtime_id);
        info!("[BrokerListener] Registered runtime: {}", runtime_id);
    }

    /// Get a runtime endpoint.
    pub fn get_runtime_endpoint(&self, runtime_id: &str) -> Option<xpc_object_t> {
        self.registered_runtimes.read().get(runtime_id).map(|&ep| {
            unsafe {
                xpc_retain(ep);
            }
            ep
        })
    }

    /// Unregister a runtime endpoint.
    pub fn unregister_runtime(&self, runtime_id: &str) {
        if let Some(endpoint) = self.registered_runtimes.write().remove(runtime_id) {
            unsafe {
                xpc_release(endpoint);
            }
            self.state.unregister_runtime(runtime_id);
            info!("[BrokerListener] Unregistered runtime: {}", runtime_id);
        }
    }

    /// Register a subprocess endpoint (subprocess-listener pattern).
    /// The subprocess creates a listener and registers its endpoint here.
    /// The host will poll for this endpoint to connect.
    pub fn register_subprocess(&self, subprocess_key: &str, endpoint: xpc_object_t) {
        unsafe {
            xpc_retain(endpoint);
        }
        self.registered_subprocesses
            .write()
            .insert(subprocess_key.to_string(), endpoint);
        self.state.register_subprocess(subprocess_key);
        info!("[BrokerListener] Registered subprocess: {}", subprocess_key);
    }

    /// Get a subprocess endpoint (for host to connect to subprocess).
    /// Returns None if subprocess hasn't registered yet.
    pub fn get_subprocess_endpoint(&self, subprocess_key: &str) -> Option<xpc_object_t> {
        self.registered_subprocesses
            .read()
            .get(subprocess_key)
            .map(|&ep| {
                unsafe {
                    xpc_retain(ep);
                }
                ep
            })
    }

    /// Unregister a subprocess endpoint.
    pub fn unregister_subprocess(&self, subprocess_key: &str) {
        if let Some(endpoint) = self.registered_subprocesses.write().remove(subprocess_key) {
            unsafe {
                xpc_release(endpoint);
            }
            self.state.unregister_subprocess(subprocess_key);
            info!(
                "[BrokerListener] Unregistered subprocess: {}",
                subprocess_key
            );
        }
    }

    /// Start the broker listener as an XPC mach service.
    ///
    /// This method blocks forever, handling incoming connections and messages.
    /// It should only be called when running as the broker service process.
    pub fn start_listener(self: Arc<Self>) -> Result<(), StreamError> {
        const XPC_CONNECTION_MACH_SERVICE_LISTENER: u64 = 1;

        unsafe {
            info!(
                "[BrokerListener] Starting listener on '{}'",
                BROKER_SERVICE_NAME
            );

            let service_name = CString::new(BROKER_SERVICE_NAME)
                .map_err(|e| StreamError::Configuration(format!("Invalid service name: {}", e)))?;

            let conn = xpc_connection_create_mach_service(
                service_name.as_ptr(),
                null_mut(),
                XPC_CONNECTION_MACH_SERVICE_LISTENER,
            );

            if conn.is_null() {
                return Err(StreamError::Configuration(
                    "Failed to create broker listener".to_string(),
                ));
            }

            // Leak self for the handler (will live forever as broker)
            let listener_ptr = Arc::into_raw(self);

            static DESCRIPTOR: BlockDescriptor = BlockDescriptor {
                reserved: 0,
                size: std::mem::size_of::<BlockLiteral<*const XpcBrokerListener>>(),
            };

            unsafe extern "C" fn connection_handler(
                block: *mut BlockLiteral<*const XpcBrokerListener>,
                event: xpc_object_t,
            ) {
                let listener = &*(*block).context;
                let obj_type = xpc_get_type(event);
                let conn_type = std::ptr::addr_of!(_xpc_type_connection) as xpc_type_t;
                let err_type = std::ptr::addr_of!(_xpc_type_error) as xpc_type_t;

                if obj_type == conn_type {
                    // New client connection
                    let client_conn = event as xpc_connection_t;
                    xpc_retain(event);

                    debug!("[BrokerListener] New client connection");

                    // Set up handler for this client
                    static CLIENT_DESCRIPTOR: BlockDescriptor = BlockDescriptor {
                        reserved: 0,
                        size: std::mem::size_of::<
                            BlockLiteral<(*const XpcBrokerListener, xpc_connection_t)>,
                        >(),
                    };

                    unsafe extern "C" fn client_handler(
                        block: *mut BlockLiteral<(*const XpcBrokerListener, xpc_connection_t)>,
                        msg: xpc_object_t,
                    ) {
                        let (listener, client_conn) = (*block).context;
                        let listener = &*listener;
                        let obj_type = xpc_get_type(msg);
                        let dict_type = std::ptr::addr_of!(_xpc_type_dictionary) as xpc_type_t;
                        let err_type = std::ptr::addr_of!(_xpc_type_error) as xpc_type_t;

                        if obj_type == err_type {
                            debug!("[BrokerListener] Client error");
                            return;
                        }

                        if obj_type != dict_type {
                            return;
                        }

                        // Check message type
                        let msg_type_key = CString::new("type").unwrap();
                        let msg_type = xpc_dictionary_get_string(msg, msg_type_key.as_ptr());

                        if msg_type.is_null() {
                            warn!("[BrokerListener] Message missing 'type' field");
                            return;
                        }

                        let msg_type_str =
                            std::ffi::CStr::from_ptr(msg_type).to_str().unwrap_or("");

                        debug!("[BrokerListener] Received message type: {}", msg_type_str);

                        match msg_type_str {
                            "register_runtime" => {
                                let runtime_id_key = CString::new("runtime_id").unwrap();
                                let endpoint_key = CString::new("endpoint").unwrap();

                                let runtime_id =
                                    xpc_dictionary_get_string(msg, runtime_id_key.as_ptr());
                                let endpoint = xpc_dictionary_get_value(msg, endpoint_key.as_ptr());

                                // Create reply - client waits synchronously for confirmation
                                let reply = xpc_dictionary_create_reply(msg);

                                if !runtime_id.is_null() && !endpoint.is_null() {
                                    let runtime_id_str =
                                        std::ffi::CStr::from_ptr(runtime_id).to_str().unwrap_or("");
                                    listener.register_runtime(runtime_id_str, endpoint);

                                    // Send success reply
                                    if !reply.is_null() {
                                        let status_key = CString::new("status").unwrap();
                                        let status_val = CString::new("ok").unwrap();
                                        xpc_dictionary_set_string(
                                            reply,
                                            status_key.as_ptr(),
                                            status_val.as_ptr(),
                                        );
                                    }
                                } else {
                                    // Send error reply
                                    if !reply.is_null() {
                                        let error_key = CString::new("error").unwrap();
                                        let error_val = CString::new("missing_fields").unwrap();
                                        xpc_dictionary_set_string(
                                            reply,
                                            error_key.as_ptr(),
                                            error_val.as_ptr(),
                                        );
                                    }
                                }

                                if !reply.is_null() {
                                    xpc_connection_send_message(client_conn, reply);
                                    xpc_release(reply);
                                }
                            }
                            "get_endpoint" => {
                                let runtime_id_key = CString::new("runtime_id").unwrap();
                                let runtime_id =
                                    xpc_dictionary_get_string(msg, runtime_id_key.as_ptr());

                                if !runtime_id.is_null() {
                                    let runtime_id_str =
                                        std::ffi::CStr::from_ptr(runtime_id).to_str().unwrap_or("");

                                    info!(
                                        "[BrokerListener] Subprocess requesting endpoint for: {}",
                                        runtime_id_str
                                    );

                                    let reply = xpc_dictionary_create_reply(msg);
                                    if reply.is_null() {
                                        warn!("[BrokerListener] Failed to create reply");
                                        return;
                                    }

                                    if let Some(endpoint) =
                                        listener.get_runtime_endpoint(runtime_id_str)
                                    {
                                        info!(
                                            "[BrokerListener] Found endpoint for runtime: {}",
                                            runtime_id_str
                                        );
                                        let endpoint_key = CString::new("endpoint").unwrap();
                                        xpc_dictionary_set_value(
                                            reply,
                                            endpoint_key.as_ptr(),
                                            endpoint as *mut c_void,
                                        );
                                        xpc_release(endpoint as *mut c_void);
                                    } else {
                                        warn!(
                                            "[BrokerListener] No endpoint found for runtime: {}",
                                            runtime_id_str
                                        );
                                        let error_key = CString::new("error").unwrap();
                                        let error_val = CString::new("not_found").unwrap();
                                        xpc_dictionary_set_string(
                                            reply,
                                            error_key.as_ptr(),
                                            error_val.as_ptr(),
                                        );
                                    }

                                    xpc_connection_send_message(client_conn, reply);
                                    xpc_release(reply);
                                }
                            }
                            "unregister_runtime" => {
                                let runtime_id_key = CString::new("runtime_id").unwrap();
                                let runtime_id =
                                    xpc_dictionary_get_string(msg, runtime_id_key.as_ptr());

                                if !runtime_id.is_null() {
                                    let runtime_id_str =
                                        std::ffi::CStr::from_ptr(runtime_id).to_str().unwrap_or("");
                                    listener.unregister_runtime(runtime_id_str);
                                }
                            }
                            "register_subprocess" => {
                                // Subprocess-listener pattern: subprocess registers its endpoint
                                let subprocess_key_cstr = CString::new("subprocess_key").unwrap();
                                let endpoint_key = CString::new("endpoint").unwrap();

                                let subprocess_key =
                                    xpc_dictionary_get_string(msg, subprocess_key_cstr.as_ptr());
                                let endpoint = xpc_dictionary_get_value(msg, endpoint_key.as_ptr());

                                let reply = xpc_dictionary_create_reply(msg);

                                if !subprocess_key.is_null() && !endpoint.is_null() {
                                    let subprocess_key_str =
                                        std::ffi::CStr::from_ptr(subprocess_key)
                                            .to_str()
                                            .unwrap_or("");
                                    listener.register_subprocess(subprocess_key_str, endpoint);

                                    if !reply.is_null() {
                                        let status_key = CString::new("status").unwrap();
                                        let status_val = CString::new("ok").unwrap();
                                        xpc_dictionary_set_string(
                                            reply,
                                            status_key.as_ptr(),
                                            status_val.as_ptr(),
                                        );
                                    }
                                } else {
                                    if !reply.is_null() {
                                        let error_key = CString::new("error").unwrap();
                                        let error_val = CString::new("missing_fields").unwrap();
                                        xpc_dictionary_set_string(
                                            reply,
                                            error_key.as_ptr(),
                                            error_val.as_ptr(),
                                        );
                                    }
                                }

                                if !reply.is_null() {
                                    xpc_connection_send_message(client_conn, reply);
                                    xpc_release(reply);
                                }
                            }
                            "get_subprocess_endpoint" => {
                                // Host polls for subprocess endpoint
                                let subprocess_key_cstr = CString::new("subprocess_key").unwrap();
                                let subprocess_key =
                                    xpc_dictionary_get_string(msg, subprocess_key_cstr.as_ptr());

                                if !subprocess_key.is_null() {
                                    let subprocess_key_str =
                                        std::ffi::CStr::from_ptr(subprocess_key)
                                            .to_str()
                                            .unwrap_or("");

                                    debug!(
                                        "[BrokerListener] Host requesting subprocess endpoint: {}",
                                        subprocess_key_str
                                    );

                                    let reply = xpc_dictionary_create_reply(msg);
                                    if reply.is_null() {
                                        warn!("[BrokerListener] Failed to create reply");
                                        return;
                                    }

                                    if let Some(endpoint) =
                                        listener.get_subprocess_endpoint(subprocess_key_str)
                                    {
                                        info!(
                                            "[BrokerListener] Found subprocess endpoint: {}",
                                            subprocess_key_str
                                        );
                                        let endpoint_key = CString::new("endpoint").unwrap();
                                        xpc_dictionary_set_value(
                                            reply,
                                            endpoint_key.as_ptr(),
                                            endpoint as *mut c_void,
                                        );
                                        xpc_release(endpoint as *mut c_void);
                                    } else {
                                        // Not found yet - host should retry
                                        debug!(
                                            "[BrokerListener] Subprocess not yet registered: {}",
                                            subprocess_key_str
                                        );
                                        let error_key = CString::new("error").unwrap();
                                        let error_val = CString::new("not_found").unwrap();
                                        xpc_dictionary_set_string(
                                            reply,
                                            error_key.as_ptr(),
                                            error_val.as_ptr(),
                                        );
                                    }

                                    xpc_connection_send_message(client_conn, reply);
                                    xpc_release(reply);
                                }
                            }
                            "unregister_subprocess" => {
                                let subprocess_key_cstr = CString::new("subprocess_key").unwrap();
                                let subprocess_key =
                                    xpc_dictionary_get_string(msg, subprocess_key_cstr.as_ptr());

                                if !subprocess_key.is_null() {
                                    let subprocess_key_str =
                                        std::ffi::CStr::from_ptr(subprocess_key)
                                            .to_str()
                                            .unwrap_or("");
                                    listener.unregister_subprocess(subprocess_key_str);
                                }
                            }
                            _ => {
                                warn!("[BrokerListener] Unknown message type: {}", msg_type_str);
                            }
                        }
                    }

                    let client_block = BlockLiteral {
                        isa: get_ns_concrete_stack_block(),
                        flags: 0,
                        reserved: 0,
                        invoke: client_handler,
                        descriptor: &CLIENT_DESCRIPTOR,
                        context: (listener as *const XpcBrokerListener, client_conn),
                    };

                    let heap_block = _Block_copy(&client_block as *const _ as *const c_void);
                    xpc_connection_set_event_handler(client_conn, heap_block);
                    xpc_connection_resume(client_conn);
                } else if obj_type == err_type {
                    error!("[BrokerListener] Listener error");
                }
            }

            let block = BlockLiteral {
                isa: get_ns_concrete_stack_block(),
                flags: 0,
                reserved: 0,
                invoke: connection_handler,
                descriptor: &DESCRIPTOR,
                context: listener_ptr,
            };

            let heap_block = _Block_copy(&block as *const _ as *const c_void);
            xpc_connection_set_event_handler(conn, heap_block);
            xpc_connection_resume(conn);

            info!("[BrokerListener] Listener started, running forever");

            // Run forever
            loop {
                std::thread::sleep(std::time::Duration::from_secs(3600));
            }
        }
    }
}
