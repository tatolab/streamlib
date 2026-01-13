// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Block ABI helpers for XPC event handlers.
//!
//! Apple's XPC requires Objective-C blocks for event handlers. These helpers
//! provide the low-level ABI structures to create blocks from Rust closures.

use std::ffi::{c_char, c_void};

use xpc_bindgen::xpc_object_t;

/// Block descriptor for the Objective-C block ABI.
#[repr(C)]
pub struct BlockDescriptor {
    pub reserved: usize,
    pub size: usize,
}

/// Block literal structure for the Objective-C block ABI.
///
/// This represents a stack-allocated block that can hold context data.
/// Use `_Block_copy` to move it to the heap before storing.
#[repr(C)]
pub struct BlockLiteral<T> {
    pub isa: *const c_void,
    pub flags: i32,
    pub reserved: i32,
    pub invoke: unsafe extern "C" fn(*mut BlockLiteral<T>, xpc_object_t),
    pub descriptor: *const BlockDescriptor,
    pub context: T,
}

/// Get the `_NSConcreteStackBlock` class pointer.
///
/// This is required for the `isa` field of stack-allocated blocks.
pub fn get_ns_concrete_stack_block() -> *const c_void {
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
    /// Copy a block from the stack to the heap.
    pub fn _Block_copy(block: *const c_void) -> *mut c_void;
}
