// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! XPC channel for direct runtime-subprocess communication.

use std::ffi::{c_void, CString};
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crossbeam_channel::{bounded, Receiver, Sender};
use parking_lot::Mutex;
use tracing::{debug, info, trace, warn};

use xpc_bindgen::{
    _xpc_type_dictionary, _xpc_type_error, xpc_connection_cancel, xpc_connection_create,
    xpc_connection_create_from_endpoint, xpc_connection_resume, xpc_connection_send_message,
    xpc_connection_set_event_handler, xpc_connection_t, xpc_dictionary_create,
    xpc_dictionary_get_int64, xpc_dictionary_get_string, xpc_dictionary_get_value,
    xpc_dictionary_set_int64, xpc_dictionary_set_string, xpc_dictionary_set_value,
    xpc_endpoint_create, xpc_endpoint_t, xpc_get_type, xpc_object_t, xpc_release, xpc_retain,
    xpc_type_t,
};

use crate::core::error::StreamError;
use crate::core::subprocess_rhi::{ChannelRole, FrameTransportHandle, SubprocessRhiChannel};

use super::block_helpers::{
    get_ns_concrete_stack_block, BlockDescriptor, BlockLiteral, _Block_copy,
};
use super::xpc_broker::XpcBroker;

/// Global lock to serialize listener creation.
/// Workaround for potential race conditions in XPC block setup when creating multiple listeners.
static LISTENER_CREATION_LOCK: Mutex<()> = Mutex::new(());
use crate::core::subprocess_rhi::SubprocessRhiBroker;

/// Received message from XPC channel.
enum XpcMessage {
    Frame {
        handle: FrameTransportHandle,
        frame_id: u64,
    },
    Control {
        message_type: String,
        payload: Vec<u8>,
    },
}

/// Context for the XPC channel event handler.
struct XpcChannelContext {
    runtime_id: String,
    role: ChannelRole,
    connected: Arc<AtomicBool>,
    message_tx: Sender<XpcMessage>,
    /// Shared peer connection - set when subprocess connects to listener
    peer_connection: Arc<Mutex<Option<xpc_connection_t>>>,
}

/// XPC channel implementation for macOS.
pub struct XpcChannel {
    /// Role of this channel.
    role: ChannelRole,
    /// Runtime ID.
    pub runtime_id: String,
    /// Anonymous listener connection (runtime side only).
    listener: Option<xpc_connection_t>,
    /// XPC endpoint (runtime side only).
    endpoint: Option<xpc_endpoint_t>,
    /// Direct peer connection - shared Arc for runtime side so handler can set it.
    peer_connection: Arc<Mutex<Option<xpc_connection_t>>>,
    /// Connected flag.
    connected: Arc<AtomicBool>,
    /// Message receiver.
    message_rx: Receiver<XpcMessage>,
    /// Message sender (for context).
    message_tx: Sender<XpcMessage>,
}

impl XpcChannel {
    /// Create the runtime-side listener and endpoint.
    /// Returns (listener, endpoint, connected_flag, peer_connection_arc)
    fn create_listener(
        runtime_id: &str,
        message_tx: Sender<XpcMessage>,
    ) -> Result<
        (
            xpc_connection_t,
            xpc_endpoint_t,
            Arc<AtomicBool>,
            Arc<Mutex<Option<xpc_connection_t>>>,
        ),
        StreamError,
    > {
        // Serialize listener creation to avoid potential race conditions in block setup
        let _guard = LISTENER_CREATION_LOCK.lock();

        unsafe {
            trace!(
                "[XpcChannel::create_listener] START for runtime: {}",
                runtime_id
            );

            // Use NULL for queue to use the default target queue.
            // This avoids potential issues with custom dispatch queues not being serviced.
            let listener = xpc_connection_create(null(), null_mut());

            trace!(
                "[XpcChannel::create_listener] Created listener {:p} with default queue for runtime: {}",
                listener,
                runtime_id
            );

            if listener.is_null() {
                return Err(StreamError::Configuration(
                    "Failed to create anonymous XPC listener".to_string(),
                ));
            }

            let connected = Arc::new(AtomicBool::new(false));
            let peer_connection = Arc::new(Mutex::new(None));

            // Create context for handler - includes shared peer_connection for storage
            let ctx = Box::leak(Box::new(XpcChannelContext {
                runtime_id: runtime_id.to_string(),
                role: ChannelRole::Runtime,
                connected: connected.clone(),
                message_tx,
                peer_connection: peer_connection.clone(),
            }));

            static DESCRIPTOR: BlockDescriptor = BlockDescriptor {
                reserved: 0,
                size: std::mem::size_of::<BlockLiteral<*const XpcChannelContext>>(),
            };

            unsafe extern "C" fn listener_handler(
                block: *mut BlockLiteral<*const XpcChannelContext>,
                event: xpc_object_t,
            ) {
                let ctx = &*(*block).context;
                let obj_type = xpc_get_type(event);
                let dict_type = std::ptr::addr_of!(_xpc_type_dictionary) as xpc_type_t;
                let err_type = std::ptr::addr_of!(_xpc_type_error) as xpc_type_t;
                let conn_type = std::ptr::addr_of!(xpc_bindgen::_xpc_type_connection) as xpc_type_t;

                // Log EVERY event type for debugging
                let type_name = if obj_type == dict_type {
                    "DICTIONARY"
                } else if obj_type == err_type {
                    "ERROR"
                } else if obj_type == conn_type {
                    "CONNECTION"
                } else {
                    "UNKNOWN"
                };

                info!(
                    "[XpcChannel] listener_handler: ENTRY runtime={}, obj_type={:p} ({}), event={:p}",
                    ctx.runtime_id,
                    obj_type,
                    type_name,
                    event
                );

                // Check if this is a new connection
                // XPC delivers new connections as CONNECTION type events
                if obj_type != dict_type && obj_type != err_type {
                    // This is a new peer connection
                    let peer_conn = event as xpc_connection_t;
                    xpc_retain(event);

                    info!(
                        "[XpcChannel] Subprocess connected to runtime: {} (peer_conn={:p})",
                        ctx.runtime_id, peer_conn
                    );

                    // Store the peer connection for send_frame() to use
                    {
                        let mut conn_guard = ctx.peer_connection.lock();
                        *conn_guard = Some(peer_conn);
                        trace!(
                            "[XpcChannel] Stored peer_connection for runtime: {}",
                            ctx.runtime_id
                        );
                    }

                    ctx.connected.store(true, Ordering::Release);

                    // Set up peer message handler
                    let message_tx = ctx.message_tx.clone();

                    static PEER_DESCRIPTOR: BlockDescriptor = BlockDescriptor {
                        reserved: 0,
                        size: std::mem::size_of::<BlockLiteral<Sender<XpcMessage>>>(),
                    };

                    unsafe extern "C" fn peer_handler(
                        block: *mut BlockLiteral<Sender<XpcMessage>>,
                        msg: xpc_object_t,
                    ) {
                        let tx = &(*block).context;
                        let obj_type = xpc_get_type(msg);
                        let dict_type = std::ptr::addr_of!(_xpc_type_dictionary) as xpc_type_t;
                        let err_type = std::ptr::addr_of!(_xpc_type_error) as xpc_type_t;

                        trace!(
                            "[XpcChannel::peer_handler] Received message, type={:p}",
                            obj_type
                        );

                        if obj_type == err_type {
                            warn!("[XpcChannel::peer_handler] Peer error/disconnection");
                            return;
                        }

                        if obj_type == dict_type {
                            // Check message type
                            let msg_type_key = CString::new("msg_type").unwrap();
                            let msg_type = xpc_dictionary_get_string(msg, msg_type_key.as_ptr());

                            if !msg_type.is_null() {
                                let msg_type_str =
                                    std::ffi::CStr::from_ptr(msg_type).to_str().unwrap_or("");

                                trace!("[XpcChannel::peer_handler] msg_type={}", msg_type_str);

                                match msg_type_str {
                                    "frame" => {
                                        // Handle frame message
                                        let frame_id_key = CString::new("frame_id").unwrap();
                                        let frame_id =
                                            xpc_dictionary_get_int64(msg, frame_id_key.as_ptr())
                                                as u64;

                                        let handle_key = CString::new("handle").unwrap();
                                        let handle_obj =
                                            xpc_dictionary_get_value(msg, handle_key.as_ptr());

                                        trace!(
                                            "[XpcChannel::peer_handler] FRAME received: frame_id={}, handle_obj={:p}",
                                            frame_id,
                                            handle_obj
                                        );

                                        if !handle_obj.is_null() {
                                            xpc_retain(handle_obj);
                                            // Determine if GPU or CPU frame based on XPC type
                                            // For now, assume GpuSurface
                                            let handle = FrameTransportHandle::GpuSurface {
                                                xpc_object: handle_obj as *mut c_void,
                                            };
                                            trace!(
                                                "[XpcChannel::peer_handler] Sending frame to channel, frame_id={}",
                                                frame_id
                                            );
                                            let _ = tx.send(XpcMessage::Frame { handle, frame_id });
                                            info!(
                                                "[XpcChannel::peer_handler] RECEIVED frame_id={} via XPC",
                                                frame_id
                                            );
                                        } else {
                                            warn!(
                                                "[XpcChannel::peer_handler] Frame handle is NULL for frame_id={}",
                                                frame_id
                                            );
                                        }
                                    }
                                    "control" => {
                                        // Handle control message
                                        let _payload_key = CString::new("payload").unwrap();
                                        let ctrl_type_key = CString::new("ctrl_type").unwrap();
                                        let ctrl_type =
                                            xpc_dictionary_get_string(msg, ctrl_type_key.as_ptr());

                                        if !ctrl_type.is_null() {
                                            let ctrl_type_str = std::ffi::CStr::from_ptr(ctrl_type)
                                                .to_str()
                                                .unwrap_or("")
                                                .to_string();
                                            trace!(
                                                "[XpcChannel::peer_handler] CONTROL received: type={}",
                                                ctrl_type_str
                                            );
                                            // TODO: Extract payload bytes
                                            let _ = tx.send(XpcMessage::Control {
                                                message_type: ctrl_type_str,
                                                payload: vec![],
                                            });
                                        }
                                    }
                                    _ => {
                                        warn!(
                                            "[XpcChannel::peer_handler] Unknown message type: {}",
                                            msg_type_str
                                        );
                                    }
                                }
                            } else {
                                trace!("[XpcChannel::peer_handler] msg_type key is null");
                            }
                        }
                    }

                    let peer_block = BlockLiteral {
                        isa: get_ns_concrete_stack_block(),
                        flags: 0,
                        reserved: 0,
                        invoke: peer_handler,
                        descriptor: &PEER_DESCRIPTOR,
                        context: message_tx,
                    };

                    let heap_block = _Block_copy(&peer_block as *const _ as *const c_void);
                    xpc_connection_set_event_handler(peer_conn, heap_block);
                    xpc_connection_resume(peer_conn);
                } else if obj_type == err_type {
                    // Identify the specific error
                    let interrupted =
                        std::ptr::addr_of!(xpc_bindgen::_xpc_error_connection_interrupted)
                            as *const c_void;
                    let invalid = std::ptr::addr_of!(xpc_bindgen::_xpc_error_connection_invalid)
                        as *const c_void;
                    let termination =
                        std::ptr::addr_of!(xpc_bindgen::_xpc_error_termination_imminent)
                            as *const c_void;

                    let error_name = if event as *const c_void == interrupted {
                        "CONNECTION_INTERRUPTED"
                    } else if event as *const c_void == invalid {
                        "CONNECTION_INVALID"
                    } else if event as *const c_void == termination {
                        "TERMINATION_IMMINENT"
                    } else {
                        "UNKNOWN_ERROR"
                    };

                    warn!(
                        "[XpcChannel] Listener ERROR for runtime={}: {} (event={:p})",
                        ctx.runtime_id, error_name, event
                    );
                }
            }

            let block = BlockLiteral {
                isa: get_ns_concrete_stack_block(),
                flags: 0,
                reserved: 0,
                invoke: listener_handler,
                descriptor: &DESCRIPTOR,
                context: ctx as *const XpcChannelContext,
            };

            trace!(
                "[XpcChannel::create_listener] Block created with ctx={:p} for runtime: {}",
                ctx as *const _,
                runtime_id
            );

            let heap_block = _Block_copy(&block as *const _ as *const c_void);
            trace!(
                "[XpcChannel::create_listener] Heap block={:p} for runtime: {}",
                heap_block,
                runtime_id
            );

            xpc_connection_set_event_handler(listener, heap_block);
            trace!(
                "[XpcChannel::create_listener] Event handler set for runtime: {}",
                runtime_id
            );

            xpc_connection_resume(listener);
            trace!(
                "[XpcChannel::create_listener] Listener resumed for runtime: {}",
                runtime_id
            );

            // Small delay to let XPC fully initialize the listener before creating endpoint.
            // This helps avoid race conditions where endpoint is created before listener is ready.
            std::thread::sleep(std::time::Duration::from_millis(10));

            // Create endpoint from listener
            let endpoint = xpc_endpoint_create(listener);

            trace!(
                "[XpcChannel::create_listener] Endpoint {:p} created for runtime: {}",
                endpoint,
                runtime_id
            );

            if endpoint.is_null() {
                xpc_connection_cancel(listener);
                return Err(StreamError::Configuration(
                    "Failed to create XPC endpoint from listener".to_string(),
                ));
            }

            info!(
                "[XpcChannel::create_listener] COMPLETE: listener={:p}, endpoint={:p} for runtime: {}",
                listener,
                endpoint,
                runtime_id
            );
            Ok((listener, endpoint, connected, peer_connection))
        }
    }

    /// Create a subprocess-side listener (subprocess-listener pattern).
    /// The subprocess creates the listener and waits for the host to connect.
    ///
    /// This method blocks until the host connects and sends a "bridge_ready" message.
    ///
    /// # Arguments
    /// * `subprocess_key` - Unique key for this subprocess (e.g., "runtime_id:processor_id")
    /// * `connection_timeout` - How long to wait for the host to connect
    pub fn create_as_subprocess_listener(
        subprocess_key: &str,
        connection_timeout: Duration,
    ) -> Result<Self, StreamError> {
        let broker_status = XpcBroker::ensure_running()?;
        debug!(
            "[XpcChannel] Broker status for subprocess listener: {:?}",
            broker_status
        );

        let (message_tx, message_rx) = bounded(64);

        // Create listener (same as runtime side)
        let (listener, endpoint, connected, peer_connection) =
            Self::create_listener(subprocess_key, message_tx.clone())?;

        // Register our endpoint with broker so host can find us
        let broker = XpcBroker::connect()?;
        broker.register_subprocess_endpoint(subprocess_key, endpoint as *mut c_void)?;

        info!(
            "[XpcChannel] Subprocess listener registered, waiting for host connection: {}",
            subprocess_key
        );

        // Wait for host to connect (CONNECTION event sets connected to true)
        let start = std::time::Instant::now();
        while !connected.load(Ordering::Acquire) {
            if start.elapsed() > connection_timeout {
                // Cleanup on timeout
                unsafe {
                    xpc_connection_cancel(listener);
                }
                let _ = broker.unregister_subprocess_endpoint(subprocess_key);
                return Err(StreamError::Configuration(format!(
                    "Timeout waiting for host to connect to subprocess: {}",
                    subprocess_key
                )));
            }
            std::thread::sleep(Duration::from_millis(10));
        }

        info!(
            "[XpcChannel] Host connected to subprocess listener: {}",
            subprocess_key
        );

        Ok(Self {
            role: ChannelRole::Subprocess,
            runtime_id: subprocess_key.to_string(),
            listener: Some(listener),
            endpoint: Some(endpoint),
            peer_connection,
            connected,
            message_rx,
            message_tx,
        })
    }

    /// Connect to a subprocess listener (host-side, subprocess-listener pattern).
    /// The host polls the broker for the subprocess endpoint, then connects.
    ///
    /// # Arguments
    /// * `subprocess_key` - Unique key for the subprocess (e.g., "runtime_id:processor_id")
    /// * `poll_interval` - How often to poll the broker
    /// * `poll_timeout` - How long to poll before giving up
    pub fn connect_to_subprocess(
        subprocess_key: &str,
        poll_interval: Duration,
        poll_timeout: Duration,
    ) -> Result<Self, StreamError> {
        let broker_status = XpcBroker::ensure_running()?;
        debug!(
            "[XpcChannel] Broker status for host connection: {:?}",
            broker_status
        );

        let broker = XpcBroker::connect()?;

        // Poll for subprocess endpoint
        let start = std::time::Instant::now();
        let endpoint = loop {
            match broker.get_subprocess_endpoint(subprocess_key)? {
                Some(ep) => break ep,
                None => {
                    if start.elapsed() > poll_timeout {
                        return Err(StreamError::Configuration(format!(
                            "Timeout waiting for subprocess to register: {}",
                            subprocess_key
                        )));
                    }
                    trace!(
                        "[XpcChannel] Subprocess not yet registered, retrying: {}",
                        subprocess_key
                    );
                    std::thread::sleep(poll_interval);
                }
            }
        };

        info!(
            "[XpcChannel] Got subprocess endpoint, connecting: {}",
            subprocess_key
        );

        let (message_tx, message_rx) = bounded(64);

        unsafe {
            let conn = xpc_connection_create_from_endpoint(endpoint as xpc_endpoint_t);

            if conn.is_null() {
                xpc_release(endpoint);
                return Err(StreamError::Configuration(
                    "Failed to create connection to subprocess".to_string(),
                ));
            }

            let connected = Arc::new(AtomicBool::new(true));

            // Set up message handler for receiving frames from subprocess
            static DESCRIPTOR: BlockDescriptor = BlockDescriptor {
                reserved: 0,
                size: std::mem::size_of::<BlockLiteral<Sender<XpcMessage>>>(),
            };

            unsafe extern "C" fn handler(
                block: *mut BlockLiteral<Sender<XpcMessage>>,
                event: xpc_object_t,
            ) {
                let tx = &(*block).context;
                let obj_type = xpc_get_type(event);
                let dict_type = std::ptr::addr_of!(_xpc_type_dictionary) as xpc_type_t;
                let err_type = std::ptr::addr_of!(_xpc_type_error) as xpc_type_t;

                if obj_type == err_type {
                    debug!("[XpcChannel] Host-to-subprocess connection error");
                    return;
                }

                if obj_type == dict_type {
                    let msg_type_key = CString::new("msg_type").unwrap();
                    let msg_type = xpc_dictionary_get_string(event, msg_type_key.as_ptr());

                    if !msg_type.is_null() {
                        let msg_type_str =
                            std::ffi::CStr::from_ptr(msg_type).to_str().unwrap_or("");

                        match msg_type_str {
                            "frame" => {
                                let frame_id_key = CString::new("frame_id").unwrap();
                                let frame_id =
                                    xpc_dictionary_get_int64(event, frame_id_key.as_ptr()) as u64;

                                let handle_key = CString::new("handle").unwrap();
                                let handle_obj =
                                    xpc_dictionary_get_value(event, handle_key.as_ptr());

                                if !handle_obj.is_null() {
                                    xpc_retain(handle_obj);
                                    let handle = FrameTransportHandle::GpuSurface {
                                        xpc_object: handle_obj as *mut c_void,
                                    };
                                    let _ = tx.send(XpcMessage::Frame { handle, frame_id });
                                }
                            }
                            "control" => {
                                let ctrl_type_key = CString::new("ctrl_type").unwrap();
                                let ctrl_type =
                                    xpc_dictionary_get_string(event, ctrl_type_key.as_ptr());

                                if !ctrl_type.is_null() {
                                    let ctrl_type_str = std::ffi::CStr::from_ptr(ctrl_type)
                                        .to_str()
                                        .unwrap_or("")
                                        .to_string();
                                    let _ = tx.send(XpcMessage::Control {
                                        message_type: ctrl_type_str,
                                        payload: vec![],
                                    });
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }

            let block = BlockLiteral {
                isa: get_ns_concrete_stack_block(),
                flags: 0,
                reserved: 0,
                invoke: handler,
                descriptor: &DESCRIPTOR,
                context: message_tx.clone(),
            };

            let heap_block = _Block_copy(&block as *const _ as *const c_void);
            xpc_connection_set_event_handler(conn, heap_block);
            xpc_connection_resume(conn);

            xpc_release(endpoint);

            info!(
                "[XpcChannel] Host connected to subprocess: {}",
                subprocess_key
            );

            Ok(Self {
                role: ChannelRole::Runtime,
                runtime_id: subprocess_key.to_string(),
                listener: None,
                endpoint: None,
                peer_connection: Arc::new(Mutex::new(Some(conn))),
                connected,
                message_rx,
                message_tx,
            })
        }
    }

    /// Wait for a "bridge_ready" control message from the peer.
    /// Used by host after connecting to subprocess to confirm subprocess is ready.
    pub fn wait_for_bridge_ready(&self, timeout: Duration) -> Result<(), StreamError> {
        info!(
            "[XpcChannel] Waiting for bridge_ready from: {}",
            self.runtime_id
        );

        match self.message_rx.recv_timeout(timeout) {
            Ok(XpcMessage::Control { message_type, .. }) if message_type == "bridge_ready" => {
                info!(
                    "[XpcChannel] Received bridge_ready from: {}",
                    self.runtime_id
                );
                Ok(())
            }
            Ok(XpcMessage::Control { message_type, .. }) => {
                Err(StreamError::Configuration(format!(
                    "Expected bridge_ready, got control message: {}",
                    message_type
                )))
            }
            Ok(XpcMessage::Frame { .. }) => Err(StreamError::Configuration(
                "Expected bridge_ready, got frame".to_string(),
            )),
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => Err(StreamError::Configuration(
                format!("Timeout waiting for bridge_ready from: {}", self.runtime_id),
            )),
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                Err(StreamError::Configuration(
                    "Channel disconnected while waiting for bridge_ready".to_string(),
                ))
            }
        }
    }

    /// Send a "bridge_ready" control message to the peer.
    /// Called by subprocess after initialization is complete.
    pub fn send_bridge_ready(&self) -> Result<(), StreamError> {
        info!("[XpcChannel] Sending bridge_ready to: {}", self.runtime_id);
        self.send_control("bridge_ready", &[])
    }
}

impl SubprocessRhiChannel for XpcChannel {
    fn create_as_runtime(runtime_id: &str) -> Result<Self, StreamError> {
        // Ensure broker service is running (auto-installs on first use)
        let broker_status = XpcBroker::ensure_running()?;
        debug!("[XpcChannel] Broker status: {:?}", broker_status);

        let (message_tx, message_rx) = bounded(64);
        let (listener, endpoint, connected, peer_connection) =
            Self::create_listener(runtime_id, message_tx.clone())?;

        // Register with broker
        let broker = XpcBroker::connect()?;
        broker.register_endpoint(runtime_id, endpoint as *mut c_void)?;

        // The peer_connection Arc is shared with the listener handler context
        // When a subprocess connects, the handler will store the connection in this Arc
        Ok(Self {
            role: ChannelRole::Runtime,
            runtime_id: runtime_id.to_string(),
            listener: Some(listener),
            endpoint: Some(endpoint),
            peer_connection, // Share the Arc with the handler
            connected,
            message_rx,
            message_tx,
        })
    }

    fn connect_as_subprocess(runtime_id: &str) -> Result<Self, StreamError> {
        let (message_tx, message_rx) = bounded(64);

        // Get endpoint from broker
        let broker = XpcBroker::connect()?;
        let endpoint = broker.get_endpoint(runtime_id)?;

        unsafe {
            trace!(
                "[XpcChannel] Creating direct connection from endpoint for runtime: {}",
                runtime_id
            );

            let conn = xpc_connection_create_from_endpoint(endpoint as xpc_endpoint_t);

            if conn.is_null() {
                xpc_release(endpoint);
                return Err(StreamError::Configuration(
                    "Failed to create connection from endpoint".to_string(),
                ));
            }

            let connected = Arc::new(AtomicBool::new(true));

            // Set up message handler
            static DESCRIPTOR: BlockDescriptor = BlockDescriptor {
                reserved: 0,
                size: std::mem::size_of::<BlockLiteral<Sender<XpcMessage>>>(),
            };

            unsafe extern "C" fn handler(
                block: *mut BlockLiteral<Sender<XpcMessage>>,
                event: xpc_object_t,
            ) {
                let tx = &(*block).context;
                let obj_type = xpc_get_type(event);
                let dict_type = std::ptr::addr_of!(_xpc_type_dictionary) as xpc_type_t;
                let err_type = std::ptr::addr_of!(_xpc_type_error) as xpc_type_t;

                if obj_type == err_type {
                    debug!("[XpcChannel] Subprocess connection error");
                    return;
                }

                if obj_type == dict_type {
                    // Handle incoming message (same as peer_handler above)
                    let msg_type_key = CString::new("msg_type").unwrap();
                    let msg_type = xpc_dictionary_get_string(event, msg_type_key.as_ptr());

                    if !msg_type.is_null() {
                        let msg_type_str =
                            std::ffi::CStr::from_ptr(msg_type).to_str().unwrap_or("");

                        if msg_type_str == "frame" {
                            let frame_id_key = CString::new("frame_id").unwrap();
                            let frame_id =
                                xpc_dictionary_get_int64(event, frame_id_key.as_ptr()) as u64;

                            let handle_key = CString::new("handle").unwrap();
                            let handle_obj = xpc_dictionary_get_value(event, handle_key.as_ptr());

                            if !handle_obj.is_null() {
                                xpc_retain(handle_obj);
                                let handle = FrameTransportHandle::GpuSurface {
                                    xpc_object: handle_obj as *mut c_void,
                                };
                                let _ = tx.send(XpcMessage::Frame { handle, frame_id });
                            }
                        }
                    }
                }
            }

            let block = BlockLiteral {
                isa: get_ns_concrete_stack_block(),
                flags: 0,
                reserved: 0,
                invoke: handler,
                descriptor: &DESCRIPTOR,
                context: message_tx.clone(),
            };

            let heap_block = _Block_copy(&block as *const _ as *const c_void);
            xpc_connection_set_event_handler(conn, heap_block);
            xpc_connection_resume(conn);

            xpc_release(endpoint);

            info!(
                "[XpcChannel] Connected as subprocess to runtime: {}",
                runtime_id
            );

            Ok(Self {
                role: ChannelRole::Subprocess,
                runtime_id: runtime_id.to_string(),
                listener: None,
                endpoint: None,
                peer_connection: Arc::new(Mutex::new(Some(conn))),
                connected,
                message_rx,
                message_tx,
            })
        }
    }

    fn role(&self) -> ChannelRole {
        self.role
    }

    fn endpoint(&self) -> Option<*mut c_void> {
        self.endpoint.map(|e| e as *mut c_void)
    }

    fn send_frame(&self, handle: FrameTransportHandle, frame_id: u64) -> Result<(), StreamError> {
        trace!(
            "[XpcChannel::send_frame] runtime_id={}, role={:?}, frame_id={}, handle_type={}",
            self.runtime_id,
            self.role,
            frame_id,
            match &handle {
                FrameTransportHandle::GpuSurface { .. } => "GpuSurface",
                FrameTransportHandle::SharedMemory { .. } => "SharedMemory",
            }
        );

        let conn = self.peer_connection.lock();
        let conn = conn.as_ref().ok_or_else(|| {
            warn!(
                "[XpcChannel::send_frame] NO PEER CONNECTION for runtime_id={}, frame_id={}",
                self.runtime_id, frame_id
            );
            StreamError::Configuration("No peer connection available".to_string())
        })?;

        trace!(
            "[XpcChannel::send_frame] Have peer connection {:p} for runtime_id={}",
            *conn,
            self.runtime_id
        );

        unsafe {
            let msg = xpc_dictionary_create(null_mut(), null_mut(), 0);

            let msg_type_key = CString::new("msg_type").unwrap();
            let msg_type_val = CString::new("frame").unwrap();
            xpc_dictionary_set_string(msg, msg_type_key.as_ptr(), msg_type_val.as_ptr());

            let frame_id_key = CString::new("frame_id").unwrap();
            xpc_dictionary_set_int64(msg, frame_id_key.as_ptr(), frame_id as i64);

            let handle_key = CString::new("handle").unwrap();
            match &handle {
                FrameTransportHandle::GpuSurface { xpc_object } => {
                    trace!(
                        "[XpcChannel::send_frame] Attaching GpuSurface xpc_object={:p} to message",
                        *xpc_object
                    );
                    xpc_dictionary_set_value(msg, handle_key.as_ptr(), *xpc_object);
                }
                FrameTransportHandle::SharedMemory { xpc_shmem, length } => {
                    trace!(
                        "[XpcChannel::send_frame] Attaching SharedMemory xpc_shmem={:p}, length={} to message",
                        *xpc_shmem,
                        length
                    );
                    xpc_dictionary_set_value(msg, handle_key.as_ptr(), *xpc_shmem);
                }
            }

            trace!(
                "[XpcChannel::send_frame] Sending XPC message to peer, frame_id={}",
                frame_id
            );
            xpc_connection_send_message(*conn, msg);
            xpc_release(msg);

            info!(
                "[XpcChannel::send_frame] SENT frame_id={} via XPC to runtime_id={}",
                frame_id, self.runtime_id
            );
        }

        Ok(())
    }

    fn recv_frame(&self, timeout: Duration) -> Result<(FrameTransportHandle, u64), StreamError> {
        match self.message_rx.recv_timeout(timeout) {
            Ok(XpcMessage::Frame { handle, frame_id }) => {
                trace!("[XpcChannel] Received frame_id={}", frame_id);
                Ok((handle, frame_id))
            }
            Ok(XpcMessage::Control { .. }) => Err(StreamError::Configuration(
                "Received control message instead of frame".to_string(),
            )),
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => Err(StreamError::Configuration(
                "Timeout waiting for frame".to_string(),
            )),
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => Err(
                StreamError::Configuration("Channel disconnected".to_string()),
            ),
        }
    }

    fn send_control(&self, message_type: &str, _payload: &[u8]) -> Result<(), StreamError> {
        let conn = self.peer_connection.lock();
        let conn = conn.as_ref().ok_or_else(|| {
            StreamError::Configuration("No peer connection available".to_string())
        })?;

        unsafe {
            let msg = xpc_dictionary_create(null_mut(), null_mut(), 0);

            let msg_type_key = CString::new("msg_type").unwrap();
            let msg_type_val = CString::new("control").unwrap();
            xpc_dictionary_set_string(msg, msg_type_key.as_ptr(), msg_type_val.as_ptr());

            let ctrl_type_key = CString::new("ctrl_type").unwrap();
            let ctrl_type_val = CString::new(message_type)
                .map_err(|e| StreamError::Configuration(format!("Invalid control type: {}", e)))?;
            xpc_dictionary_set_string(msg, ctrl_type_key.as_ptr(), ctrl_type_val.as_ptr());

            // TODO: Add payload bytes

            trace!("[XpcChannel] Sending control message: {}", message_type);
            xpc_connection_send_message(*conn, msg);
            xpc_release(msg);
        }

        Ok(())
    }

    fn recv_control(&self, timeout: Duration) -> Result<(String, Vec<u8>), StreamError> {
        match self.message_rx.recv_timeout(timeout) {
            Ok(XpcMessage::Control {
                message_type,
                payload,
            }) => {
                trace!("[XpcChannel] Received control message: {}", message_type);
                Ok((message_type, payload))
            }
            Ok(XpcMessage::Frame { .. }) => Err(StreamError::Configuration(
                "Received frame instead of control message".to_string(),
            )),
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => Err(StreamError::Configuration(
                "Timeout waiting for control message".to_string(),
            )),
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => Err(
                StreamError::Configuration("Channel disconnected".to_string()),
            ),
        }
    }

    fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Acquire)
    }

    fn close(&self) {
        unsafe {
            if let Some(conn) = self.peer_connection.lock().take() {
                xpc_connection_cancel(conn);
            }
            if let Some(listener) = self.listener {
                xpc_connection_cancel(listener);
            }
        }
        info!(
            "[XpcChannel] Closed channel for runtime: {}",
            self.runtime_id
        );
    }
}

// Safety: XPC connections are thread-safe
unsafe impl Send for XpcChannel {}
unsafe impl Sync for XpcChannel {}

impl Drop for XpcChannel {
    fn drop(&mut self) {
        self.close();
    }
}
