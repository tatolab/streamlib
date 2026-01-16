// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! XPC broker listener for runtime endpoint exchange.
//!
//! This module contains the server-side broker listener that runs as a launchd service.
//! Runtimes and subprocesses connect to this listener to register and look up endpoints.

use std::collections::HashMap;
use std::ffi::{c_void, CString};
use std::ptr::null_mut;
use std::sync::Arc;

use parking_lot::RwLock;
use tracing::{debug, error, info, warn};

use xpc_bindgen::{
    _xpc_type_connection, _xpc_type_dictionary, _xpc_type_error,
    xpc_connection_create_mach_service, xpc_connection_resume, xpc_connection_send_message,
    xpc_connection_set_event_handler, xpc_connection_t, xpc_dictionary_create_reply,
    xpc_dictionary_get_string, xpc_dictionary_get_value, xpc_dictionary_set_string,
    xpc_dictionary_set_value, xpc_get_type, xpc_object_t, xpc_release, xpc_retain, xpc_type_t,
};

use crate::block_helpers::{
    get_ns_concrete_stack_block, BlockDescriptor, BlockLiteral, _Block_copy,
};
use crate::state::BrokerState;

/// Broker service name for launchd registration.
pub const BROKER_SERVICE_NAME: &str = "com.tatolab.streamlib.runtime";

/// Broker listener state for when running as the broker service.
pub struct XpcBrokerListener {
    /// Registered runtime endpoints.
    registered_runtimes: Arc<RwLock<HashMap<String, xpc_object_t>>>,
    /// Registered subprocess endpoints.
    /// Key is "runtime_id:processor_id" to allow multiple subprocesses per runtime.
    registered_subprocesses: Arc<RwLock<HashMap<String, xpc_object_t>>>,
    /// XPC bridge connection endpoints (Phase 4).
    /// Key is connection_id from AllocateConnection gRPC call.
    /// Host stores endpoint here, client retrieves it.
    registered_connection_endpoints: Arc<RwLock<HashMap<String, xpc_object_t>>>,
    /// Shared state for diagnostics (gRPC).
    pub state: BrokerState,
}

impl XpcBrokerListener {
    /// Create a new broker listener with shared diagnostics state.
    pub fn new(state: BrokerState) -> Self {
        Self {
            registered_runtimes: Arc::new(RwLock::new(HashMap::new())),
            registered_subprocesses: Arc::new(RwLock::new(HashMap::new())),
            registered_connection_endpoints: Arc::new(RwLock::new(HashMap::new())),
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

    // ─────────────────────────────────────────────────────────────────────────
    // XPC Bridge Connection Endpoints (Phase 4)
    // ─────────────────────────────────────────────────────────────────────────

    /// Store an XPC endpoint for a connection (called by host processor).
    pub fn store_connection_endpoint(&self, connection_id: &str, endpoint: xpc_object_t) {
        unsafe {
            xpc_retain(endpoint);
        }
        self.registered_connection_endpoints
            .write()
            .insert(connection_id.to_string(), endpoint);
        info!(
            "[BrokerListener] Stored endpoint for connection: {}",
            connection_id
        );
    }

    /// Get an XPC endpoint for a connection (called by client processor).
    /// Returns None if not yet stored by host.
    pub fn get_connection_endpoint(&self, connection_id: &str) -> Option<xpc_object_t> {
        self.registered_connection_endpoints
            .read()
            .get(connection_id)
            .map(|&ep| {
                unsafe {
                    xpc_retain(ep);
                }
                ep
            })
    }

    /// Remove an XPC endpoint for a connection.
    pub fn remove_connection_endpoint(&self, connection_id: &str) {
        if let Some(endpoint) = self
            .registered_connection_endpoints
            .write()
            .remove(connection_id)
        {
            unsafe {
                xpc_release(endpoint);
            }
            info!(
                "[BrokerListener] Removed endpoint for connection: {}",
                connection_id
            );
        }
    }

    /// Start the broker listener as an XPC mach service.
    ///
    /// This method blocks forever, handling incoming connections and messages.
    /// It should only be called when running as the broker service process.
    pub fn start_listener(self: Arc<Self>) -> Result<(), anyhow::Error> {
        const XPC_CONNECTION_MACH_SERVICE_LISTENER: u64 = 1;

        unsafe {
            info!(
                "[BrokerListener] Starting listener on '{}'",
                BROKER_SERVICE_NAME
            );

            let service_name = CString::new(BROKER_SERVICE_NAME)?;

            let conn = xpc_connection_create_mach_service(
                service_name.as_ptr(),
                null_mut(),
                XPC_CONNECTION_MACH_SERVICE_LISTENER,
            );

            if conn.is_null() {
                return Err(anyhow::anyhow!("Failed to create broker listener"));
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

                                let reply = xpc_dictionary_create_reply(msg);

                                if !runtime_id.is_null() && !endpoint.is_null() {
                                    let runtime_id_str =
                                        std::ffi::CStr::from_ptr(runtime_id).to_str().unwrap_or("");
                                    listener.register_runtime(runtime_id_str, endpoint);

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
                                        listener.state.record_connection(
                                            runtime_id_str,
                                            "",
                                            "subprocess",
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
                                        let parts: Vec<&str> =
                                            subprocess_key_str.splitn(2, ':').collect();
                                        let (runtime_id, processor_id) = if parts.len() == 2 {
                                            (parts[0], parts[1])
                                        } else {
                                            (subprocess_key_str, "")
                                        };
                                        listener.state.record_connection(
                                            runtime_id,
                                            processor_id,
                                            "runtime",
                                        );
                                        let endpoint_key = CString::new("endpoint").unwrap();
                                        xpc_dictionary_set_value(
                                            reply,
                                            endpoint_key.as_ptr(),
                                            endpoint as *mut c_void,
                                        );
                                        xpc_release(endpoint as *mut c_void);
                                    } else {
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
                            // ─────────────────────────────────────────────────────────────
                            // XPC Bridge Connection Handlers (Phase 4)
                            // ─────────────────────────────────────────────────────────────
                            "store_endpoint" => {
                                // Host processor stores its XPC endpoint by connection_id
                                let connection_id_key = CString::new("connection_id").unwrap();
                                let endpoint_key = CString::new("endpoint").unwrap();

                                let connection_id =
                                    xpc_dictionary_get_string(msg, connection_id_key.as_ptr());
                                let endpoint = xpc_dictionary_get_value(msg, endpoint_key.as_ptr());

                                let reply = xpc_dictionary_create_reply(msg);

                                if !connection_id.is_null() && !endpoint.is_null() {
                                    let connection_id_str = std::ffi::CStr::from_ptr(connection_id)
                                        .to_str()
                                        .unwrap_or("");
                                    listener.store_connection_endpoint(connection_id_str, endpoint);

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
                            "get_endpoint_for_connection" => {
                                // Client processor retrieves XPC endpoint by connection_id
                                let connection_id_key = CString::new("connection_id").unwrap();
                                let connection_id =
                                    xpc_dictionary_get_string(msg, connection_id_key.as_ptr());

                                if !connection_id.is_null() {
                                    let connection_id_str = std::ffi::CStr::from_ptr(connection_id)
                                        .to_str()
                                        .unwrap_or("");

                                    debug!(
                                        "[BrokerListener] Client requesting endpoint for connection: {}",
                                        connection_id_str
                                    );

                                    let reply = xpc_dictionary_create_reply(msg);
                                    if reply.is_null() {
                                        warn!("[BrokerListener] Failed to create reply");
                                        return;
                                    }

                                    if let Some(endpoint) =
                                        listener.get_connection_endpoint(connection_id_str)
                                    {
                                        info!(
                                            "[BrokerListener] Found endpoint for connection: {}",
                                            connection_id_str
                                        );
                                        let endpoint_key = CString::new("endpoint").unwrap();
                                        xpc_dictionary_set_value(
                                            reply,
                                            endpoint_key.as_ptr(),
                                            endpoint as *mut c_void,
                                        );
                                        xpc_release(endpoint as *mut c_void);
                                    } else {
                                        debug!(
                                            "[BrokerListener] Endpoint not yet stored for connection: {}",
                                            connection_id_str
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

// Safety: XpcBrokerListener uses Arc<RwLock<>> for thread-safe state
unsafe impl Send for XpcBrokerListener {}
unsafe impl Sync for XpcBrokerListener {}
