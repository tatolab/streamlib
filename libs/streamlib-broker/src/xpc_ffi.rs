// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! XPC FFI bindings for the broker's surface store service.

#![allow(
    dead_code,
    non_snake_case,
    non_upper_case_globals,
    non_camel_case_types
)]

use std::ffi::{c_char, c_void};

// =============================================================================
// XPC Object Types
// =============================================================================

/// Opaque XPC object type.
pub type xpc_object_t = *mut c_void;

/// XPC connection type (subtype of xpc_object_t).
pub type xpc_connection_t = *mut c_void;

/// Mach port type.
pub type mach_port_t = u32;

// =============================================================================
// XPC Type Constants
// =============================================================================

/// XPC type for dictionary objects.
pub const XPC_TYPE_DICTIONARY: *const c_void =
    unsafe { &_xpc_type_dictionary as *const _ as *const c_void };

/// XPC type for error objects.
pub const XPC_TYPE_ERROR: *const c_void = unsafe { &_xpc_type_error as *const _ as *const c_void };

// External type symbols (opaque, just for pointer identity)
extern "C" {
    static _xpc_type_dictionary: c_void;
    static _xpc_type_error: c_void;
}

// =============================================================================
// XPC Error Constants
// =============================================================================

extern "C" {
    /// Error returned when the connection is interrupted.
    pub static _xpc_error_connection_interrupted: c_void;

    /// Error returned when the connection is invalid.
    pub static _xpc_error_connection_invalid: c_void;
}

/// Get the connection interrupted error object.
pub fn xpc_error_connection_interrupted() -> xpc_object_t {
    unsafe { &_xpc_error_connection_interrupted as *const _ as xpc_object_t }
}

/// Get the connection invalid error object.
pub fn xpc_error_connection_invalid() -> xpc_object_t {
    unsafe { &_xpc_error_connection_invalid as *const _ as xpc_object_t }
}

// =============================================================================
// XPC Functions
// =============================================================================

#[link(name = "System", kind = "dylib")]
extern "C" {
    // =========================================================================
    // Connection Management
    // =========================================================================

    /// Create a connection to a Mach service.
    pub fn xpc_connection_create_mach_service(
        name: *const c_char,
        target_queue: *mut c_void, // dispatch_queue_t
        flags: u64,
    ) -> xpc_connection_t;

    /// Set the event handler for a connection.
    pub fn xpc_connection_set_event_handler(connection: xpc_connection_t, handler: *mut c_void);

    /// Resume (activate) a connection.
    pub fn xpc_connection_resume(connection: xpc_connection_t);

    /// Cancel a connection.
    pub fn xpc_connection_cancel(connection: xpc_connection_t);

    /// Send a message and wait for a reply (synchronous).
    pub fn xpc_connection_send_message_with_reply_sync(
        connection: xpc_connection_t,
        message: xpc_object_t,
    ) -> xpc_object_t;

    /// Send a message without waiting for a reply.
    pub fn xpc_connection_send_message(connection: xpc_connection_t, message: xpc_object_t);

    // =========================================================================
    // Dictionary Operations
    // =========================================================================

    /// Create a new XPC dictionary.
    pub fn xpc_dictionary_create(
        keys: *const *const c_char,
        values: *const xpc_object_t,
        count: usize,
    ) -> xpc_object_t;

    /// Create a reply dictionary for a request.
    pub fn xpc_dictionary_create_reply(original: xpc_object_t) -> xpc_object_t;

    /// Set a string value in a dictionary.
    pub fn xpc_dictionary_set_string(
        dictionary: xpc_object_t,
        key: *const c_char,
        value: *const c_char,
    );

    /// Set an int64 value in a dictionary.
    pub fn xpc_dictionary_set_int64(dictionary: xpc_object_t, key: *const c_char, value: i64);

    /// Set a mach send right in a dictionary.
    pub fn xpc_dictionary_set_mach_send(
        dictionary: xpc_object_t,
        key: *const c_char,
        port: mach_port_t,
    );

    /// Get a string value from a dictionary.
    pub fn xpc_dictionary_get_string(dictionary: xpc_object_t, key: *const c_char)
        -> *const c_char;

    /// Get an int64 value from a dictionary.
    pub fn xpc_dictionary_get_int64(dictionary: xpc_object_t, key: *const c_char) -> i64;

    /// Copy a mach send right from a dictionary.
    pub fn xpc_dictionary_copy_mach_send(
        dictionary: xpc_object_t,
        key: *const c_char,
    ) -> mach_port_t;

    /// Get the remote connection that sent a message.
    pub fn xpc_dictionary_get_remote_connection(dictionary: xpc_object_t) -> xpc_connection_t;

    // =========================================================================
    // Object Lifecycle
    // =========================================================================

    /// Retain an XPC object (increment reference count).
    pub fn xpc_retain(object: xpc_object_t) -> xpc_object_t;

    /// Release an XPC object (decrement reference count).
    pub fn xpc_release(object: xpc_object_t);

    /// Get the type of an XPC object.
    pub fn xpc_get_type(object: xpc_object_t) -> *const c_void;

    /// Create a string representation of an XPC object (for debugging).
    pub fn xpc_copy_description(object: xpc_object_t) -> *mut c_char;
}

// =============================================================================
// Helper Functions
// =============================================================================

/// Check if an XPC object is an error.
pub fn xpc_is_error(object: xpc_object_t) -> bool {
    if object.is_null() {
        return false;
    }
    unsafe { xpc_get_type(object) == XPC_TYPE_ERROR }
}

/// Check if an XPC object is a dictionary.
pub fn xpc_is_dictionary(object: xpc_object_t) -> bool {
    if object.is_null() {
        return false;
    }
    unsafe { xpc_get_type(object) == XPC_TYPE_DICTIONARY }
}

// =============================================================================
// Block Support (for handlers)
// =============================================================================

/// Block literal structure for XPC handlers.
#[repr(C)]
pub struct Block<F> {
    pub isa: *const c_void,
    pub flags: i32,
    pub reserved: i32,
    pub invoke: *const c_void,
    pub descriptor: *const BlockDescriptor,
    pub context: F,
}

#[repr(C)]
pub struct BlockDescriptor {
    pub reserved: u64,
    pub size: u64,
}

// Block class symbols
extern "C" {
    /// Stack block class (for blocks allocated on stack).
    pub static _NSConcreteStackBlock: c_void;
    /// Malloc block class (for blocks allocated on heap).
    pub static _NSConcreteMallocBlock: c_void;
    /// Global block class (for blocks with no captures).
    pub static _NSConcreteGlobalBlock: c_void;
}

/// Flags for a stack block.
pub const BLOCK_FLAGS_STACK: i32 = 1 << 25;
/// Flags for a heap-allocated block that needs release.
pub const BLOCK_FLAGS_NEEDS_FREE: i32 = 1 << 24;

/// Flags indicating the block has a signature.
pub const BLOCK_HAS_SIGNATURE: i32 = 1 << 30;
