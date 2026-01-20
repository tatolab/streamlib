// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! XPC service listener for the broker's surface store.
//!
//! Provides a Mach service that handles check_in, check_out, and release
//! operations for cross-process IOSurface sharing.

use std::ffi::{c_void, CStr, CString};

use crate::state::BrokerState;
use crate::xpc_ffi::*;

/// XPC service for surface store operations.
pub struct XpcSurfaceService {
    state: BrokerState,
    service_name: String,
    connection: Option<xpc_connection_t>,
}

impl XpcSurfaceService {
    /// Create a new XPC surface service.
    pub fn new(state: BrokerState, service_name: String) -> Self {
        Self {
            state,
            service_name,
            connection: None,
        }
    }

    /// Start listening for XPC connections.
    ///
    /// This function spawns the XPC listener and returns immediately.
    /// The listener runs on a separate dispatch queue managed by XPC.
    pub fn start(&mut self) -> Result<(), String> {
        let service_name_cstr = CString::new(self.service_name.as_str())
            .map_err(|e| format!("Invalid service name: {}", e))?;

        // Create a listener for the Mach service
        // XPC_CONNECTION_MACH_SERVICE_LISTENER flag (1 << 0) creates a listener
        let connection = unsafe {
            xpc_connection_create_mach_service(
                service_name_cstr.as_ptr(),
                std::ptr::null_mut(), // default queue
                1,                    // XPC_CONNECTION_MACH_SERVICE_LISTENER
            )
        };

        if connection.is_null() {
            return Err(format!(
                "Failed to create XPC listener for '{}'",
                self.service_name
            ));
        }

        // Create a handler context that will be passed to the block
        let state = self.state.clone();
        let handler_context = Box::into_raw(Box::new(HandlerContext { state }));

        // Set up the event handler for new connections
        unsafe {
            let handler = create_connection_handler(handler_context);
            xpc_connection_set_event_handler(connection, handler as *mut c_void);
            xpc_connection_resume(connection);
        }

        self.connection = Some(connection);

        tracing::info!(
            "[Broker] XPC surface service listening on '{}'",
            self.service_name
        );

        Ok(())
    }

    /// Stop the XPC service.
    pub fn stop(&mut self) {
        if let Some(connection) = self.connection.take() {
            unsafe {
                xpc_connection_cancel(connection);
            }
            tracing::info!("[Broker] XPC surface service stopped");
        }
    }
}

impl Drop for XpcSurfaceService {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Context passed to the XPC handler blocks.
struct HandlerContext {
    state: BrokerState,
}

// Safety: BrokerState is Clone and uses Arc internally
unsafe impl Send for HandlerContext {}
unsafe impl Sync for HandlerContext {}

/// Create a block that handles new XPC connections.
///
/// This uses the Objective-C block ABI directly since we can't use the
/// block crate without adding dependencies.
unsafe fn create_connection_handler(context: *mut HandlerContext) -> *mut c_void {
    // The block trampoline that handles incoming connections
    extern "C" fn connection_handler_trampoline(
        block: *mut Block<*mut HandlerContext>,
        peer: xpc_connection_t,
    ) {
        unsafe {
            let context = (*block).context;
            if context.is_null() {
                return;
            }

            // Check if this is an error
            if xpc_is_error(peer as xpc_object_t) {
                if peer as xpc_object_t == xpc_error_connection_invalid() {
                    tracing::debug!("[Broker] XPC connection invalid");
                } else if peer as xpc_object_t == xpc_error_connection_interrupted() {
                    tracing::debug!("[Broker] XPC connection interrupted");
                }
                return;
            }

            // Set up handler for messages from this peer
            let message_handler = create_message_handler(context);
            xpc_connection_set_event_handler(peer, message_handler as *mut c_void);
            xpc_connection_resume(peer);

            tracing::trace!("[Broker] XPC accepted new peer connection");
        }
    }

    // Create the block structure
    static DESCRIPTOR: BlockDescriptor = BlockDescriptor {
        reserved: 0,
        size: std::mem::size_of::<Block<*mut HandlerContext>>() as u64,
    };

    let block = Box::new(Block {
        isa: &_NSConcreteStackBlock as *const _ as *const c_void,
        flags: BLOCK_FLAGS_STACK,
        reserved: 0,
        invoke: connection_handler_trampoline as *const c_void,
        descriptor: &DESCRIPTOR,
        context,
    });

    Box::into_raw(block) as *mut c_void
}

/// Create a block that handles messages from a peer connection.
unsafe fn create_message_handler(context: *mut HandlerContext) -> *mut c_void {
    extern "C" fn message_handler_trampoline(
        block: *mut Block<*mut HandlerContext>,
        event: xpc_object_t,
    ) {
        unsafe {
            let context = (*block).context;
            if context.is_null() {
                return;
            }

            // Check if this is an error
            if xpc_is_error(event) {
                if event == xpc_error_connection_invalid() {
                    tracing::trace!("[Broker] XPC peer connection closed");
                } else if event == xpc_error_connection_interrupted() {
                    tracing::trace!("[Broker] XPC peer connection interrupted");
                }
                return;
            }

            // Handle the message
            if xpc_is_dictionary(event) {
                handle_message(&*context, event);
            }
        }
    }

    static DESCRIPTOR: BlockDescriptor = BlockDescriptor {
        reserved: 0,
        size: std::mem::size_of::<Block<*mut HandlerContext>>() as u64,
    };

    let block = Box::new(Block {
        isa: &_NSConcreteStackBlock as *const _ as *const c_void,
        flags: BLOCK_FLAGS_STACK,
        reserved: 0,
        invoke: message_handler_trampoline as *const c_void,
        descriptor: &DESCRIPTOR,
        context,
    });

    Box::into_raw(block) as *mut c_void
}

/// Handle an incoming XPC message.
unsafe fn handle_message(context: &HandlerContext, message: xpc_object_t) {
    // Get the operation type
    let op_key = CString::new("op").unwrap();
    let op_ptr = xpc_dictionary_get_string(message, op_key.as_ptr());

    if op_ptr.is_null() {
        tracing::warn!("[Broker] XPC message missing 'op' field");
        return;
    }

    let op = CStr::from_ptr(op_ptr).to_string_lossy();

    match op.as_ref() {
        "check_in" => handle_check_in(context, message),
        "check_out" => handle_check_out(context, message),
        "release" => handle_release(context, message),
        _ => {
            tracing::warn!("[Broker] XPC unknown operation: {}", op);
        }
    }
}

/// Handle a check_in request: register a surface and return its ID.
unsafe fn handle_check_in(context: &HandlerContext, message: xpc_object_t) {
    let runtime_id_key = CString::new("runtime_id").unwrap();
    let port_key = CString::new("mach_port").unwrap();
    let surface_id_key = CString::new("surface_id").unwrap();

    // Get runtime_id
    let runtime_id_ptr = xpc_dictionary_get_string(message, runtime_id_key.as_ptr());
    let runtime_id = if !runtime_id_ptr.is_null() {
        CStr::from_ptr(runtime_id_ptr)
            .to_string_lossy()
            .into_owned()
    } else {
        "unknown".to_string()
    };

    // Get mach port
    let mach_port = xpc_dictionary_get_mach_send(message, port_key.as_ptr());

    if mach_port == 0 {
        tracing::warn!("[Broker] XPC check_in: invalid mach port");
        // Send error reply
        let reply = xpc_dictionary_create_reply(message);
        if !reply.is_null() {
            let error_key = CString::new("error").unwrap();
            let error_value = CString::new("invalid mach port").unwrap();
            xpc_dictionary_set_string(reply, error_key.as_ptr(), error_value.as_ptr());

            let remote = xpc_dictionary_get_remote_connection(message);
            if !remote.is_null() {
                xpc_connection_send_message(remote, reply);
            }
            xpc_release(reply);
        }
        return;
    }

    // Register the surface
    let surface_id = context.state.register_surface(&runtime_id, mach_port);

    tracing::debug!(
        "[Broker] XPC check_in: registered surface '{}' for runtime '{}' (port {})",
        surface_id,
        runtime_id,
        mach_port
    );

    // Send reply with surface_id
    let reply = xpc_dictionary_create_reply(message);
    if !reply.is_null() {
        let surface_id_cstr = CString::new(surface_id.as_str()).unwrap();
        xpc_dictionary_set_string(reply, surface_id_key.as_ptr(), surface_id_cstr.as_ptr());

        let remote = xpc_dictionary_get_remote_connection(message);
        if !remote.is_null() {
            xpc_connection_send_message(remote, reply);
        }
        xpc_release(reply);
    }
}

/// Handle a check_out request: return the mach port for a surface ID.
unsafe fn handle_check_out(context: &HandlerContext, message: xpc_object_t) {
    let surface_id_key = CString::new("surface_id").unwrap();
    let port_key = CString::new("mach_port").unwrap();

    // Get surface_id
    let surface_id_ptr = xpc_dictionary_get_string(message, surface_id_key.as_ptr());
    if surface_id_ptr.is_null() {
        tracing::warn!("[Broker] XPC check_out: missing surface_id");
        return;
    }

    let surface_id = CStr::from_ptr(surface_id_ptr).to_string_lossy();

    // Look up the mach port
    let mach_port = context.state.get_surface_mach_port(&surface_id);

    let reply = xpc_dictionary_create_reply(message);
    if reply.is_null() {
        return;
    }

    match mach_port {
        Some(port) => {
            tracing::trace!(
                "[Broker] XPC check_out: returning port {} for surface '{}'",
                port,
                surface_id
            );
            xpc_dictionary_set_mach_send(reply, port_key.as_ptr(), port);
        }
        None => {
            tracing::warn!("[Broker] XPC check_out: surface '{}' not found", surface_id);
            let error_key = CString::new("error").unwrap();
            let error_value = CString::new("surface not found").unwrap();
            xpc_dictionary_set_string(reply, error_key.as_ptr(), error_value.as_ptr());
        }
    }

    let remote = xpc_dictionary_get_remote_connection(message);
    if !remote.is_null() {
        xpc_connection_send_message(remote, reply);
    }
    xpc_release(reply);
}

/// Handle a release request: unregister a surface.
unsafe fn handle_release(context: &HandlerContext, message: xpc_object_t) {
    let surface_id_key = CString::new("surface_id").unwrap();
    let runtime_id_key = CString::new("runtime_id").unwrap();

    // Get surface_id
    let surface_id_ptr = xpc_dictionary_get_string(message, surface_id_key.as_ptr());
    if surface_id_ptr.is_null() {
        tracing::warn!("[Broker] XPC release: missing surface_id");
        return;
    }

    let surface_id = CStr::from_ptr(surface_id_ptr).to_string_lossy();

    // Get runtime_id
    let runtime_id_ptr = xpc_dictionary_get_string(message, runtime_id_key.as_ptr());
    let runtime_id = if !runtime_id_ptr.is_null() {
        CStr::from_ptr(runtime_id_ptr)
            .to_string_lossy()
            .into_owned()
    } else {
        "unknown".to_string()
    };

    // Release the surface
    let released = context.state.release_surface(&surface_id, &runtime_id);

    if released {
        tracing::debug!(
            "[Broker] XPC release: released surface '{}' for runtime '{}'",
            surface_id,
            runtime_id
        );
    } else {
        tracing::trace!(
            "[Broker] XPC release: surface '{}' not found or not owned by runtime '{}'",
            surface_id,
            runtime_id
        );
    }

    // Release is fire-and-forget, no reply needed
}

// Safety: XPC connections are thread-safe
unsafe impl Send for XpcSurfaceService {}
unsafe impl Sync for XpcSurfaceService {}
