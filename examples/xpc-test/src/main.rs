// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! XPC Broker Architecture Test
//!
//! Tests the broker pattern for XPC communication:
//! 1. Broker: launchd-registered service for signaling only
//! 2. Runtime: Creates anonymous listener, sends endpoint to broker
//! 3. Subprocess: Gets endpoint from broker, creates direct connection
//!
//! This enables multiple runtimes while only requiring ONE launchd popup.

use std::collections::HashMap;
use std::ffi::{c_char, c_void, CString};
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use tracing::{debug, error, info, warn};

// XPC bindings
use xpc_bindgen::{
    _xpc_error_connection_interrupted, _xpc_error_connection_invalid,
    _xpc_error_termination_imminent, _xpc_type_connection, _xpc_type_dictionary,
    _xpc_type_endpoint, _xpc_type_error, _xpc_type_shmem, dispatch_queue_create,
    xpc_connection_cancel, xpc_connection_create, xpc_connection_create_from_endpoint,
    xpc_connection_create_mach_service, xpc_connection_resume, xpc_connection_send_message,
    xpc_connection_send_message_with_reply_sync, xpc_connection_set_event_handler,
    xpc_connection_t, xpc_dictionary_create, xpc_dictionary_create_reply,
    xpc_dictionary_get_string, xpc_dictionary_get_value, xpc_dictionary_set_string,
    xpc_dictionary_set_value, xpc_endpoint_create, xpc_endpoint_t, xpc_get_type, xpc_object_t,
    xpc_release, xpc_retain, xpc_shmem_create, xpc_shmem_map, xpc_type_t,
};

// IOSurface XPC functions
#[link(name = "IOSurface", kind = "framework")]
extern "C" {
    fn IOSurfaceCreateXPCObject(surface: *mut c_void) -> xpc_object_t;
    fn IOSurfaceLookupFromXPCObject(xobj: xpc_object_t) -> *mut c_void;
}

// Launchd plist generation
use launchd::{Launchd, MachServiceEntry};

const XPC_CONNECTION_MACH_SERVICE_LISTENER: u64 = 1;
const BROKER_SERVICE_NAME: &str = "com.streamlib.broker";

// ============================================================================
// Block ABI for XPC event handlers
// ============================================================================

#[repr(C)]
struct BlockDescriptor {
    reserved: usize,
    size: usize,
}

#[repr(C)]
struct BlockLiteral<T> {
    isa: *const c_void,
    flags: i32,
    reserved: i32,
    invoke: unsafe extern "C" fn(*mut BlockLiteral<T>, xpc_object_t),
    descriptor: *const BlockDescriptor,
    context: T,
}

fn get_ns_concrete_stack_block() -> *const c_void {
    use std::ffi::CStr;
    extern "C" {
        fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    }
    const RTLD_DEFAULT: *mut c_void = -2isize as *mut c_void;

    unsafe {
        let symbol = CStr::from_bytes_with_nul_unchecked(b"_NSConcreteStackBlock\0");
        dlsym(RTLD_DEFAULT, symbol.as_ptr())
    }
}

extern "C" {
    fn _Block_copy(block: *const c_void) -> *mut c_void;
}

// ============================================================================
// XPC type checking
// ============================================================================

unsafe fn get_type_name(obj: xpc_object_t) -> &'static str {
    if obj.is_null() {
        return "NULL";
    }
    let obj_type = xpc_get_type(obj);
    let dict_type = std::ptr::addr_of!(_xpc_type_dictionary) as xpc_type_t;
    let err_type = std::ptr::addr_of!(_xpc_type_error) as xpc_type_t;
    let conn_type = std::ptr::addr_of!(_xpc_type_connection) as xpc_type_t;
    let endpoint_type = std::ptr::addr_of!(_xpc_type_endpoint) as xpc_type_t;
    let shmem_type = std::ptr::addr_of!(_xpc_type_shmem) as xpc_type_t;

    if obj_type == dict_type {
        "DICTIONARY"
    } else if obj_type == err_type {
        "ERROR"
    } else if obj_type == conn_type {
        "CONNECTION"
    } else if obj_type == endpoint_type {
        "ENDPOINT"
    } else if obj_type == shmem_type {
        "SHMEM"
    } else {
        "UNKNOWN"
    }
}

unsafe fn identify_error(obj: xpc_object_t) -> &'static str {
    let invalid = std::ptr::addr_of!(_xpc_error_connection_invalid) as xpc_object_t;
    let interrupted = std::ptr::addr_of!(_xpc_error_connection_interrupted) as xpc_object_t;
    let terminating = std::ptr::addr_of!(_xpc_error_termination_imminent) as xpc_object_t;

    if obj == invalid {
        "CONNECTION_INVALID"
    } else if obj == interrupted {
        "CONNECTION_INTERRUPTED"
    } else if obj == terminating {
        "TERMINATION_IMMINENT"
    } else {
        "UNKNOWN_ERROR"
    }
}

// ============================================================================
// Paths and service management
// ============================================================================

fn get_plist_path(service_name: &str) -> PathBuf {
    let home = std::env::var("HOME").expect("HOME not set");
    PathBuf::from(home)
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{}.plist", service_name))
}

fn get_domain_target() -> String {
    let uid = unsafe { libc::getuid() };
    format!("gui/{}", uid)
}

// ============================================================================
// Broker Service
// ============================================================================

/// Stored endpoint for a runtime
struct RuntimeRegistration {
    endpoint: xpc_object_t,
}

/// Simple in-memory storage for registered runtimes
/// In production, this would be more sophisticated
static mut REGISTERED_RUNTIMES: Option<HashMap<String, RuntimeRegistration>> = None;

unsafe fn init_runtime_registry() {
    REGISTERED_RUNTIMES = Some(HashMap::new());
}

unsafe fn register_runtime(runtime_id: &str, endpoint: xpc_object_t) {
    if let Some(ref mut map) = REGISTERED_RUNTIMES {
        // Retain the endpoint since we're storing it
        xpc_retain(endpoint);
        map.insert(runtime_id.to_string(), RuntimeRegistration { endpoint });
        info!("[Broker] Registered runtime: {}", runtime_id);
    }
}

unsafe fn get_runtime_endpoint(runtime_id: &str) -> Option<xpc_object_t> {
    if let Some(ref map) = REGISTERED_RUNTIMES {
        map.get(runtime_id).map(|r| {
            // Retain for the caller
            xpc_retain(r.endpoint);
            r.endpoint
        })
    } else {
        None
    }
}

/// Handle incoming broker connection
unsafe extern "C" fn broker_connection_handler(_block: *mut BlockLiteral<()>, event: xpc_object_t) {
    let type_name = get_type_name(event);
    debug!("[Broker] Connection event: {}", type_name);

    if type_name == "CONNECTION" {
        // New client connection - set up handler for this client
        let client_conn = event as xpc_connection_t;
        xpc_retain(event);

        static DESCRIPTOR: BlockDescriptor = BlockDescriptor {
            reserved: 0,
            size: std::mem::size_of::<BlockLiteral<xpc_connection_t>>(),
        };

        unsafe extern "C" fn client_handler(
            block: *mut BlockLiteral<xpc_connection_t>,
            msg: xpc_object_t,
        ) {
            let client_conn = (*block).context;
            let type_name = get_type_name(msg);

            if type_name == "ERROR" {
                let err = identify_error(msg);
                debug!("[Broker] Client error: {}", err);
                return;
            }

            if type_name != "DICTIONARY" {
                return;
            }

            // Check message type
            let msg_type_key = CString::new("type").unwrap();
            let msg_type = xpc_dictionary_get_string(msg, msg_type_key.as_ptr());

            if msg_type.is_null() {
                warn!("[Broker] Message missing 'type' field");
                return;
            }

            let msg_type_str = std::ffi::CStr::from_ptr(msg_type).to_str().unwrap_or("");
            debug!("[Broker] Received message type: {}", msg_type_str);

            match msg_type_str {
                "register_runtime" => {
                    // Runtime is registering with its endpoint
                    let runtime_id_key = CString::new("runtime_id").unwrap();
                    let endpoint_key = CString::new("endpoint").unwrap();

                    let runtime_id = xpc_dictionary_get_string(msg, runtime_id_key.as_ptr());
                    let endpoint = xpc_dictionary_get_value(msg, endpoint_key.as_ptr());

                    if !runtime_id.is_null() && !endpoint.is_null() {
                        let runtime_id_str =
                            std::ffi::CStr::from_ptr(runtime_id).to_str().unwrap_or("");
                        register_runtime(runtime_id_str, endpoint);
                    }
                }
                "get_endpoint" => {
                    // Subprocess requesting endpoint for a runtime
                    let runtime_id_key = CString::new("runtime_id").unwrap();
                    let runtime_id = xpc_dictionary_get_string(msg, runtime_id_key.as_ptr());

                    if !runtime_id.is_null() {
                        let runtime_id_str =
                            std::ffi::CStr::from_ptr(runtime_id).to_str().unwrap_or("");
                        info!(
                            "[Broker] Subprocess requesting endpoint for: {}",
                            runtime_id_str
                        );

                        // Create reply
                        let reply = xpc_dictionary_create_reply(msg);
                        if reply.is_null() {
                            warn!("[Broker] Failed to create reply (message may not expect reply)");
                            return;
                        }

                        if let Some(endpoint) = get_runtime_endpoint(runtime_id_str) {
                            info!("[Broker] Found endpoint for runtime: {}", runtime_id_str);

                            // Add endpoint to reply
                            let endpoint_key = CString::new("endpoint").unwrap();
                            xpc_dictionary_set_value(
                                reply,
                                endpoint_key.as_ptr(),
                                endpoint as *mut c_void,
                            );

                            // Release our reference (get_runtime_endpoint retained it)
                            xpc_release(endpoint as *mut c_void);
                        } else {
                            warn!("[Broker] No endpoint found for runtime: {}", runtime_id_str);

                            // Add error to reply
                            let error_key = CString::new("error").unwrap();
                            let error_val = CString::new("not_found").unwrap();
                            xpc_dictionary_set_string(
                                reply,
                                error_key.as_ptr(),
                                error_val.as_ptr(),
                            );
                        }

                        // Send reply
                        xpc_connection_send_message(client_conn, reply);
                        xpc_release(reply);
                    }
                }
                _ => {
                    warn!("[Broker] Unknown message type: {}", msg_type_str);
                }
            }
        }

        let block = BlockLiteral {
            isa: get_ns_concrete_stack_block(),
            flags: 0,
            reserved: 0,
            invoke: client_handler,
            descriptor: &DESCRIPTOR,
            context: client_conn,
        };

        let heap_block = _Block_copy(&block as *const _ as *const c_void);
        xpc_connection_set_event_handler(client_conn, heap_block);
        xpc_connection_resume(client_conn);
        debug!("[Broker] Client connection set up");
    } else if type_name == "ERROR" {
        let err = identify_error(event);
        error!("[Broker] Listener error: {}", err);
    }
}

/// Start the broker as an XPC listener
unsafe fn start_broker_listener() -> xpc_connection_t {
    info!("[Broker] Starting listener on '{}'", BROKER_SERVICE_NAME);

    let service_name = CString::new(BROKER_SERVICE_NAME).unwrap();
    let conn = xpc_connection_create_mach_service(
        service_name.as_ptr(),
        null_mut(),
        XPC_CONNECTION_MACH_SERVICE_LISTENER,
    );

    if conn.is_null() {
        error!("[Broker] Failed to create listener");
        return null_mut();
    }

    static DESCRIPTOR: BlockDescriptor = BlockDescriptor {
        reserved: 0,
        size: std::mem::size_of::<BlockLiteral<()>>(),
    };

    let block = BlockLiteral {
        isa: get_ns_concrete_stack_block(),
        flags: 0,
        reserved: 0,
        invoke: broker_connection_handler,
        descriptor: &DESCRIPTOR,
        context: (),
    };

    let heap_block = _Block_copy(&block as *const _ as *const c_void);
    xpc_connection_set_event_handler(conn, heap_block);
    xpc_connection_resume(conn);

    info!("[Broker] Listener started");
    conn
}

// ============================================================================
// Runtime (creates anonymous listener, registers with broker)
// ============================================================================

/// Context for runtime's anonymous listener
struct RuntimeContext {
    runtime_id: String,
    message_received: AtomicBool,
}

/// Start runtime's anonymous listener and return endpoint
unsafe fn create_runtime_listener(runtime_id: &str) -> (xpc_connection_t, xpc_endpoint_t) {
    info!("[Runtime {}] Creating anonymous listener", runtime_id);

    // Create dispatch queue for the listener
    let queue_name = CString::new(format!("com.streamlib.runtime.{}", runtime_id)).unwrap();
    let queue = dispatch_queue_create(queue_name.as_ptr(), null_mut());

    // Create anonymous listener (passing NULL for name)
    let listener = xpc_connection_create(null(), queue);

    if listener.is_null() {
        error!(
            "[Runtime {}] Failed to create anonymous listener",
            runtime_id
        );
        return (null_mut(), null_mut());
    }

    static DESCRIPTOR: BlockDescriptor = BlockDescriptor {
        reserved: 0,
        size: std::mem::size_of::<BlockLiteral<*const RuntimeContext>>(),
    };

    // Leak the context for simplicity in this test
    let ctx = Box::leak(Box::new(RuntimeContext {
        runtime_id: runtime_id.to_string(),
        message_received: AtomicBool::new(false),
    }));

    unsafe extern "C" fn runtime_listener_handler(
        block: *mut BlockLiteral<*const RuntimeContext>,
        event: xpc_object_t,
    ) {
        let ctx = &*(*block).context;
        let type_name = get_type_name(event);

        debug!("[Runtime {}] Listener event: {}", ctx.runtime_id, type_name);

        if type_name == "CONNECTION" {
            // Incoming connection from subprocess
            let peer_conn = event as xpc_connection_t;
            xpc_retain(event);

            info!(
                "[Runtime {}] Subprocess connected directly!",
                ctx.runtime_id
            );

            // Set up handler for messages from subprocess
            static PEER_DESCRIPTOR: BlockDescriptor = BlockDescriptor {
                reserved: 0,
                size: std::mem::size_of::<BlockLiteral<String>>(),
            };

            let runtime_id = ctx.runtime_id.clone();

            unsafe extern "C" fn peer_handler(block: *mut BlockLiteral<String>, msg: xpc_object_t) {
                let runtime_id = &(*block).context;
                let type_name = get_type_name(msg);

                if type_name == "ERROR" {
                    let err = identify_error(msg);
                    debug!("[Runtime {}] Peer error: {}", runtime_id, err);
                    return;
                }

                if type_name == "DICTIONARY" {
                    let msg_key = CString::new("message").unwrap();
                    let msg_val = xpc_dictionary_get_string(msg, msg_key.as_ptr());

                    if !msg_val.is_null() {
                        let msg_str = std::ffi::CStr::from_ptr(msg_val).to_str().unwrap_or("");
                        info!(
                            "[Runtime {}] Received from subprocess: '{}'",
                            runtime_id, msg_str
                        );
                    }
                }
            }

            let block = BlockLiteral {
                isa: get_ns_concrete_stack_block(),
                flags: 0,
                reserved: 0,
                invoke: peer_handler,
                descriptor: &PEER_DESCRIPTOR,
                context: runtime_id,
            };

            let heap_block = _Block_copy(&block as *const _ as *const c_void);
            xpc_connection_set_event_handler(peer_conn, heap_block);
            xpc_connection_resume(peer_conn);
        } else if type_name == "ERROR" {
            let err = identify_error(event);
            error!("[Runtime {}] Listener error: {}", ctx.runtime_id, err);
        }
    }

    let block = BlockLiteral {
        isa: get_ns_concrete_stack_block(),
        flags: 0,
        reserved: 0,
        invoke: runtime_listener_handler,
        descriptor: &DESCRIPTOR,
        context: ctx as *const RuntimeContext,
    };

    let heap_block = _Block_copy(&block as *const _ as *const c_void);
    xpc_connection_set_event_handler(listener, heap_block);
    xpc_connection_resume(listener);

    // Create endpoint from the listener
    let endpoint = xpc_endpoint_create(listener);
    info!("[Runtime {}] Created endpoint: {:p}", runtime_id, endpoint);

    (listener, endpoint)
}

/// Register runtime with broker
unsafe fn register_with_broker(runtime_id: &str, endpoint: xpc_endpoint_t) -> bool {
    info!("[Runtime {}] Connecting to broker", runtime_id);

    let service_name = CString::new(BROKER_SERVICE_NAME).unwrap();
    let conn = xpc_connection_create_mach_service(service_name.as_ptr(), null_mut(), 0);

    if conn.is_null() {
        error!("[Runtime {}] Failed to connect to broker", runtime_id);
        return false;
    }

    static DESCRIPTOR: BlockDescriptor = BlockDescriptor {
        reserved: 0,
        size: std::mem::size_of::<BlockLiteral<()>>(),
    };

    unsafe extern "C" fn handler(_block: *mut BlockLiteral<()>, event: xpc_object_t) {
        let type_name = get_type_name(event);
        if type_name == "ERROR" {
            let err = identify_error(event);
            debug!("[Runtime→Broker] Error: {}", err);
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

    // Send registration message with endpoint
    let msg = xpc_dictionary_create(null_mut(), null_mut(), 0);

    let type_key = CString::new("type").unwrap();
    let type_val = CString::new("register_runtime").unwrap();
    xpc_dictionary_set_string(msg, type_key.as_ptr(), type_val.as_ptr());

    let id_key = CString::new("runtime_id").unwrap();
    let id_val = CString::new(runtime_id).unwrap();
    xpc_dictionary_set_string(msg, id_key.as_ptr(), id_val.as_ptr());

    let endpoint_key = CString::new("endpoint").unwrap();
    xpc_dictionary_set_value(msg, endpoint_key.as_ptr(), endpoint as *mut c_void);

    info!("[Runtime {}] Sending registration to broker", runtime_id);
    xpc_connection_send_message(conn, msg);
    xpc_release(msg);

    // Give broker time to process
    thread::sleep(Duration::from_millis(100));

    info!("[Runtime {}] Registration sent", runtime_id);
    true
}

// ============================================================================
// Subprocess (gets endpoint from broker, connects directly to runtime)
// ============================================================================

/// Subprocess connects to runtime via broker
unsafe fn subprocess_connect(runtime_id: &str) -> xpc_connection_t {
    info!(
        "[Subprocess] Connecting to broker to get endpoint for runtime: {}",
        runtime_id
    );

    let service_name = CString::new(BROKER_SERVICE_NAME).unwrap();
    let conn = xpc_connection_create_mach_service(service_name.as_ptr(), null_mut(), 0);

    if conn.is_null() {
        error!("[Subprocess] Failed to connect to broker");
        return null_mut();
    }

    static DESCRIPTOR: BlockDescriptor = BlockDescriptor {
        reserved: 0,
        size: std::mem::size_of::<BlockLiteral<()>>(),
    };

    unsafe extern "C" fn handler(_block: *mut BlockLiteral<()>, event: xpc_object_t) {
        let type_name = get_type_name(event);
        if type_name == "ERROR" {
            let err = identify_error(event);
            debug!("[Subprocess→Broker] Error: {}", err);
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

    // Request endpoint from broker
    let msg = xpc_dictionary_create(null_mut(), null_mut(), 0);

    let type_key = CString::new("type").unwrap();
    let type_val = CString::new("get_endpoint").unwrap();
    xpc_dictionary_set_string(msg, type_key.as_ptr(), type_val.as_ptr());

    let id_key = CString::new("runtime_id").unwrap();
    let id_val = CString::new(runtime_id).unwrap();
    xpc_dictionary_set_string(msg, id_key.as_ptr(), id_val.as_ptr());

    info!("[Subprocess] Requesting endpoint from broker");

    // Send with synchronous reply to get the endpoint back
    let reply = xpc_connection_send_message_with_reply_sync(conn, msg);
    xpc_release(msg);

    if reply.is_null() {
        error!("[Subprocess] No reply from broker");
        return null_mut();
    }

    let reply_type = get_type_name(reply);
    info!("[Subprocess] Broker reply type: {}", reply_type);

    if reply_type == "DICTIONARY" {
        let endpoint_key = CString::new("endpoint").unwrap();
        let endpoint = xpc_dictionary_get_value(reply, endpoint_key.as_ptr());

        if !endpoint.is_null() && get_type_name(endpoint) == "ENDPOINT" {
            info!("[Subprocess] Got endpoint from broker!");

            // Create connection from endpoint
            let direct_conn = xpc_connection_create_from_endpoint(endpoint as xpc_endpoint_t);

            if !direct_conn.is_null() {
                info!("[Subprocess] Created direct connection to runtime!");

                unsafe extern "C" fn direct_handler(
                    _block: *mut BlockLiteral<()>,
                    event: xpc_object_t,
                ) {
                    let type_name = get_type_name(event);
                    if type_name == "ERROR" {
                        let err = identify_error(event);
                        debug!("[Subprocess→Runtime] Error: {}", err);
                    } else if type_name == "DICTIONARY" {
                        info!("[Subprocess] Received message from runtime!");
                    }
                }

                let block = BlockLiteral {
                    isa: get_ns_concrete_stack_block(),
                    flags: 0,
                    reserved: 0,
                    invoke: direct_handler,
                    descriptor: &DESCRIPTOR,
                    context: (),
                };

                let heap_block = _Block_copy(&block as *const _ as *const c_void);
                xpc_connection_set_event_handler(direct_conn, heap_block);
                xpc_connection_resume(direct_conn);

                return direct_conn;
            }
        }
    }

    error!("[Subprocess] Failed to get endpoint from broker");
    null_mut()
}

/// Send test message from subprocess to runtime
unsafe fn subprocess_send_message(conn: xpc_connection_t, message: &str) {
    let msg = xpc_dictionary_create(null_mut(), null_mut(), 0);

    let key = CString::new("message").unwrap();
    let val = CString::new(message).unwrap();
    xpc_dictionary_set_string(msg, key.as_ptr(), val.as_ptr());

    info!("[Subprocess] Sending message: '{}'", message);
    xpc_connection_send_message(conn, msg);
    xpc_release(msg);
}

// ============================================================================
// Test Setup
// ============================================================================

fn setup_broker_service() -> bool {
    let plist_path = get_plist_path(BROKER_SERVICE_NAME);

    info!(
        "Setting up broker service plist at: {}",
        plist_path.display()
    );

    // Create MachServices map
    let mut mach_services = HashMap::new();
    mach_services.insert(
        BROKER_SERVICE_NAME.to_string(),
        MachServiceEntry::Boolean(true),
    );

    // For this test, we'll use our own binary as the "broker"
    let exe_path = std::env::current_exe().expect("Failed to get current exe path");

    let plist = Launchd::new(BROKER_SERVICE_NAME, exe_path.to_str().unwrap())
        .expect("Failed to create Launchd config")
        .with_mach_services(mach_services)
        .with_program_arguments(vec![
            exe_path.to_str().unwrap().to_string(),
            "--broker".to_string(),
        ]);

    // Ensure LaunchAgents directory exists
    if let Some(parent) = plist_path.parent() {
        fs::create_dir_all(parent).expect("Failed to create LaunchAgents directory");
    }

    let mut file = fs::File::create(&plist_path).expect("Failed to create plist file");
    plist
        .to_writer_xml(&mut file)
        .expect("Failed to write plist XML");
    drop(file);

    // Bootstrap the service
    let domain_target = get_domain_target();

    // First try to unload if it exists
    let _ = std::process::Command::new("launchctl")
        .args(["bootout", &domain_target, plist_path.to_str().unwrap()])
        .output();

    let output = std::process::Command::new("launchctl")
        .args(["bootstrap", &domain_target, plist_path.to_str().unwrap()])
        .output()
        .expect("Failed to run launchctl bootstrap");

    if output.status.success() {
        info!("Broker service bootstrapped successfully");
        true
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        error!("Failed to bootstrap broker service: {}", stderr);
        false
    }
}

fn cleanup_broker_service() {
    let plist_path = get_plist_path(BROKER_SERVICE_NAME);
    let domain_target = get_domain_target();

    info!("Cleaning up broker service");

    let _ = std::process::Command::new("launchctl")
        .args(["bootout", &domain_target, plist_path.to_str().unwrap()])
        .output();

    let _ = fs::remove_file(&plist_path);
}

// ============================================================================
// Frame Data Tests
// ============================================================================

/// Test xpc_shmem for zero-copy shared memory (AudioFrame/DataFrame)
unsafe fn test_xpc_shmem() -> bool {
    use mach2::kern_return::KERN_SUCCESS;
    use mach2::traps::mach_task_self;
    use mach2::vm::{mach_vm_allocate, mach_vm_deallocate};
    use mach2::vm_types::mach_vm_address_t;

    info!("\n=== Testing xpc_shmem (shared memory for CPU frames) ===");

    // Simulate an AudioFrame buffer (e.g., 1024 samples * 2 channels * 4 bytes)
    let buffer_size: usize = 1024 * 2 * 4; // 8KB

    // Page-align the size
    let page_size = 4096usize;
    let alloc_size = (buffer_size + page_size - 1) & !(page_size - 1);

    // Allocate using mach_vm_allocate (required for XPC shmem)
    let mut region: mach_vm_address_t = 0;
    let kr = mach_vm_allocate(mach_task_self(), &mut region, alloc_size as u64, 1); // VM_FLAGS_ANYWHERE = 1

    if kr != KERN_SUCCESS {
        error!("[xpc_shmem] Failed to allocate mach VM memory: {}", kr);
        return false;
    }
    info!(
        "[xpc_shmem] Allocated mach VM at 0x{:x}, size: {}",
        region, alloc_size
    );

    // Fill with test pattern (simulating audio samples)
    let data = std::slice::from_raw_parts_mut(region as *mut u8, buffer_size);
    for (i, byte) in data.iter_mut().enumerate() {
        *byte = (i % 256) as u8;
    }
    info!("[xpc_shmem] Created test buffer: {} bytes", buffer_size);

    // Create XPC shmem object from the region
    info!("[xpc_shmem] Calling xpc_shmem_create...");
    let shmem = xpc_shmem_create(region as *mut c_void, alloc_size);
    if shmem.is_null() {
        error!("[xpc_shmem] Failed to create xpc_shmem");
        mach_vm_deallocate(mach_task_self(), region, alloc_size as u64);
        return false;
    }
    info!("[xpc_shmem] Created xpc_shmem object: {:p}", shmem);

    // Verify it's actually a shmem type
    let type_name = get_type_name(shmem);
    if type_name != "SHMEM" {
        error!("[xpc_shmem] Object is not SHMEM type, got: {}", type_name);
        xpc_release(shmem);
        mach_vm_deallocate(mach_task_self(), region, alloc_size as u64);
        return false;
    }

    // Simulate receiving side: map the shmem
    let mut mapped_region: *mut c_void = null_mut();
    let mapped_size = xpc_shmem_map(shmem, &mut mapped_region);

    if mapped_size == 0 || mapped_region.is_null() {
        error!("[xpc_shmem] Failed to map shmem");
        xpc_release(shmem);
        mach_vm_deallocate(mach_task_self(), region, alloc_size as u64);
        return false;
    }
    info!(
        "[xpc_shmem] Mapped shmem at {:p}, size: {} bytes",
        mapped_region, mapped_size
    );

    // Verify data integrity
    let mapped_data = std::slice::from_raw_parts(mapped_region as *const u8, buffer_size);
    let mut mismatches = 0;
    for (i, &byte) in mapped_data.iter().enumerate() {
        let expected = (i % 256) as u8;
        if byte != expected {
            mismatches += 1;
            if mismatches <= 3 {
                error!(
                    "[xpc_shmem] Mismatch at offset {}: expected {}, got {}",
                    i, expected, byte
                );
            }
        }
    }

    if mismatches == 0 {
        info!(
            "[xpc_shmem] SUCCESS: All {} bytes verified correctly!",
            buffer_size
        );
    } else {
        error!("[xpc_shmem] FAILED: {} mismatches found", mismatches);
    }

    // Cleanup
    // Note: xpc_shmem_map returns memory that should be unmapped with munmap
    libc::munmap(mapped_region, mapped_size);
    xpc_release(shmem);
    mach_vm_deallocate(mach_task_self(), region, alloc_size as u64);

    mismatches == 0
}

/// Test IOSurface XPC transfer for zero-copy GPU frames (VideoFrame)
unsafe fn test_iosurface_xpc() -> bool {
    info!("\n=== Testing IOSurface XPC (GPU frames) ===");

    // IOSurface C API
    #[link(name = "IOSurface", kind = "framework")]
    extern "C" {
        fn IOSurfaceCreate(properties: *const c_void) -> *mut c_void;
        fn IOSurfaceGetID(surface: *mut c_void) -> u32;
        fn IOSurfaceGetWidth(surface: *mut c_void) -> usize;
        fn IOSurfaceGetHeight(surface: *mut c_void) -> usize;
        fn IOSurfaceLock(surface: *mut c_void, options: u32, seed: *mut u32) -> i32;
        fn IOSurfaceUnlock(surface: *mut c_void, options: u32, seed: *mut u32) -> i32;
        fn IOSurfaceGetBaseAddress(surface: *mut c_void) -> *mut c_void;
    }

    // CoreFoundation for dictionary
    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFDictionaryCreate(
            allocator: *const c_void,
            keys: *const *const c_void,
            values: *const *const c_void,
            num_values: isize,
            key_callbacks: *const c_void,
            value_callbacks: *const c_void,
        ) -> *mut c_void;
        fn CFNumberCreate(
            allocator: *const c_void,
            the_type: isize,
            value_ptr: *const c_void,
        ) -> *mut c_void;
        fn CFRelease(cf: *mut c_void);

        static kCFTypeDictionaryKeyCallBacks: c_void;
        static kCFTypeDictionaryValueCallBacks: c_void;
    }

    const kCFNumberSInt32Type: isize = 3;

    // IOSurface property keys
    #[link(name = "IOSurface", kind = "framework")]
    extern "C" {
        static kIOSurfaceWidth: *const c_void;
        static kIOSurfaceHeight: *const c_void;
        static kIOSurfaceBytesPerElement: *const c_void;
        static kIOSurfaceBytesPerRow: *const c_void;
        static kIOSurfacePixelFormat: *const c_void;
    }

    let width: i32 = 1920;
    let height: i32 = 1080;
    let bytes_per_element: i32 = 4; // BGRA
    let bytes_per_row: i32 = width * bytes_per_element;
    let pixel_format: i32 = 0x42475241u32 as i32; // 'BGRA'

    info!("[IOSurface] Creating {}x{} BGRA surface", width, height);

    // Create CFNumber values
    let width_num = CFNumberCreate(
        null(),
        kCFNumberSInt32Type,
        &width as *const i32 as *const c_void,
    );
    let height_num = CFNumberCreate(
        null(),
        kCFNumberSInt32Type,
        &height as *const i32 as *const c_void,
    );
    let bpe_num = CFNumberCreate(
        null(),
        kCFNumberSInt32Type,
        &bytes_per_element as *const i32 as *const c_void,
    );
    let bpr_num = CFNumberCreate(
        null(),
        kCFNumberSInt32Type,
        &bytes_per_row as *const i32 as *const c_void,
    );
    let pf_num = CFNumberCreate(
        null(),
        kCFNumberSInt32Type,
        &pixel_format as *const i32 as *const c_void,
    );

    // Build dictionary
    let keys: [*const c_void; 5] = [
        kIOSurfaceWidth,
        kIOSurfaceHeight,
        kIOSurfaceBytesPerElement,
        kIOSurfaceBytesPerRow,
        kIOSurfacePixelFormat,
    ];
    let values: [*const c_void; 5] = [
        width_num as *const c_void,
        height_num as *const c_void,
        bpe_num as *const c_void,
        bpr_num as *const c_void,
        pf_num as *const c_void,
    ];

    let properties = CFDictionaryCreate(
        null(),
        keys.as_ptr(),
        values.as_ptr(),
        5,
        &kCFTypeDictionaryKeyCallBacks as *const c_void,
        &kCFTypeDictionaryValueCallBacks as *const c_void,
    );

    // Create IOSurface
    let surface = IOSurfaceCreate(properties);

    // Cleanup dictionary and numbers
    CFRelease(properties);
    CFRelease(width_num);
    CFRelease(height_num);
    CFRelease(bpe_num);
    CFRelease(bpr_num);
    CFRelease(pf_num);

    if surface.is_null() {
        error!("[IOSurface] Failed to create IOSurface");
        return false;
    }

    let surface_id = IOSurfaceGetID(surface);
    info!(
        "[IOSurface] Created IOSurface ID: {}, size: {}x{}",
        surface_id, width, height
    );

    // Write test pattern to surface
    IOSurfaceLock(surface, 0, null_mut());
    let base_addr = IOSurfaceGetBaseAddress(surface);
    if !base_addr.is_null() {
        let data =
            std::slice::from_raw_parts_mut(base_addr as *mut u8, (bytes_per_row * height) as usize);
        // Fill with a gradient pattern (just first row for speed)
        for x in 0..width as usize {
            let offset = x * 4;
            data[offset] = (x % 256) as u8; // B
            data[offset + 1] = 0; // G
            data[offset + 2] = 128; // R
            data[offset + 3] = 255; // A
        }
        info!("[IOSurface] Wrote test pattern to surface");
    }
    IOSurfaceUnlock(surface, 0, null_mut());

    // Create XPC object from IOSurface
    let xpc_obj = IOSurfaceCreateXPCObject(surface);

    if xpc_obj.is_null() {
        error!("[IOSurface] Failed to create XPC object from IOSurface");
        CFRelease(surface);
        return false;
    }
    info!("[IOSurface] Created XPC object: {:p}", xpc_obj);

    // Simulate receiving side: lookup IOSurface from XPC object
    let received_surface_ptr = IOSurfaceLookupFromXPCObject(xpc_obj);

    if received_surface_ptr.is_null() {
        error!("[IOSurface] Failed to lookup IOSurface from XPC object");
        xpc_release(xpc_obj);
        CFRelease(surface);
        return false;
    }

    let received_id = IOSurfaceGetID(received_surface_ptr);
    let received_width = IOSurfaceGetWidth(received_surface_ptr);
    let received_height = IOSurfaceGetHeight(received_surface_ptr);

    info!(
        "[IOSurface] Received IOSurface ID: {}, size: {}x{}",
        received_id, received_width, received_height
    );

    // Verify it's the same surface
    let success = received_id == surface_id
        && received_width == width as usize
        && received_height == height as usize;

    if success {
        info!("[IOSurface] SUCCESS: IOSurface transferred via XPC correctly!");
    } else {
        error!(
            "[IOSurface] FAILED: Surface mismatch! Expected ID {}, got {}",
            surface_id, received_id
        );
    }

    // Cleanup
    xpc_release(xpc_obj);
    CFRelease(received_surface_ptr);
    CFRelease(surface);

    success
}

// ============================================================================
// Main
// ============================================================================

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::DEBUG.into()),
        )
        .with_target(false)
        .init();

    // Check if we're running as the broker service
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 && args[1] == "--broker" {
        run_as_broker();
        return;
    }

    info!("############################################");
    info!("# XPC BROKER ARCHITECTURE TEST");
    info!("# PID: {}", std::process::id());
    info!("############################################\n");

    // Run the test
    run_broker_test();

    // Test frame data transfer mechanisms
    unsafe {
        info!("\n############################################");
        info!("# FRAME DATA TRANSFER TESTS");
        info!("############################################");

        let shmem_ok = test_xpc_shmem();
        let iosurface_ok = test_iosurface_xpc();

        info!("\n############################################");
        info!("# RESULTS");
        info!("############################################");
        info!(
            "xpc_shmem (AudioFrame/DataFrame): {}",
            if shmem_ok { "PASS" } else { "FAIL" }
        );
        info!(
            "IOSurface XPC (VideoFrame): {}",
            if iosurface_ok { "PASS" } else { "FAIL" }
        );
    }

    info!("\n############################################");
    info!("# TEST COMPLETE");
    info!("############################################");
}

fn run_as_broker() {
    info!("[Broker Process] Starting as broker service");

    unsafe {
        init_runtime_registry();
        let listener = start_broker_listener();

        if listener.is_null() {
            error!("[Broker Process] Failed to start listener");
            return;
        }

        // Run forever
        loop {
            thread::sleep(Duration::from_secs(3600));
        }
    }
}

fn run_broker_test() {
    info!("=== Step 1: Setup broker service ===");

    // Note: For a proper test, the broker should run as a separate process
    // For now, we'll test the components individually

    // First, check if broker is already running or set it up
    if !setup_broker_service() {
        error!("Failed to setup broker service");
        return;
    }

    // Give broker time to start
    thread::sleep(Duration::from_secs(1));

    info!("\n=== Step 2: Create runtime with anonymous listener ===");

    let runtime_id = format!("runtime-{}", std::process::id());

    unsafe {
        let (listener, endpoint) = create_runtime_listener(&runtime_id);

        if listener.is_null() || endpoint.is_null() {
            error!("Failed to create runtime listener");
            cleanup_broker_service();
            return;
        }

        info!("\n=== Step 3: Register runtime with broker ===");

        if !register_with_broker(&runtime_id, endpoint) {
            error!("Failed to register with broker");
            cleanup_broker_service();
            return;
        }

        info!("\n=== Step 4: Subprocess connects via broker ===");

        let direct_conn = subprocess_connect(&runtime_id);

        if direct_conn.is_null() {
            warn!("Subprocess failed to get direct connection");
            warn!("This is expected if broker doesn't reply with endpoint yet");
        } else {
            info!("\n=== Step 5: Test direct communication ===");

            subprocess_send_message(direct_conn, "Hello from subprocess!");

            // Wait for message to be received
            thread::sleep(Duration::from_millis(500));
        }

        info!("\n=== Step 6: Cleanup ===");

        // Cancel connections
        if !listener.is_null() {
            xpc_connection_cancel(listener);
        }
        if !direct_conn.is_null() {
            xpc_connection_cancel(direct_conn);
        }
    }

    cleanup_broker_service();
}
