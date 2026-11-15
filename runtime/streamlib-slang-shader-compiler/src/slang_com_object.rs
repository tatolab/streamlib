// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::ffi::c_void;
use std::ptr::NonNull;

use crate::slang_compiler_abi;

/// One reference to a Slang COM object, released on drop.
pub(crate) struct SlangComObject(NonNull<c_void>);

impl SlangComObject {
    pub(crate) fn from_owned(raw_object: *mut c_void) -> Option<Self> {
        NonNull::new(raw_object).map(Self)
    }

    pub(crate) fn as_ptr(&self) -> *mut c_void {
        self.0.as_ptr()
    }

    /// The method at `slot` of this object's vtable.
    ///
    /// # Safety
    /// `slot` must be a method of this object's interface with signature `F`.
    pub(crate) unsafe fn method<F: Copy>(&self, slot: usize) -> F {
        // SAFETY: forwarded to the caller.
        unsafe { vtable_method(self.as_ptr(), slot) }
    }

    /// The bytes of this object, which must be an `ISlangBlob`.
    pub(crate) fn blob_bytes(&self) -> &[u8] {
        // SAFETY: the object is a live blob, whose pointer and size methods
        // describe one buffer it owns for as long as it lives.
        unsafe {
            let pointer = self.method::<slang_compiler_abi::GetBufferPointer>(
                slang_compiler_abi::IBLOB_GET_BUFFER_POINTER,
            )(self.as_ptr());
            let size = self.method::<slang_compiler_abi::GetBufferSize>(
                slang_compiler_abi::IBLOB_GET_BUFFER_SIZE,
            )(self.as_ptr());
            if pointer.is_null() || size == 0 {
                return &[];
            }
            std::slice::from_raw_parts(pointer.cast::<u8>(), size)
        }
    }
}

impl Drop for SlangComObject {
    fn drop(&mut self) {
        // SAFETY: every Slang interface starts with ISlangUnknown, and this
        // value holds exactly one reference.
        unsafe {
            self.method::<slang_compiler_abi::Release>(slang_compiler_abi::IUNKNOWN_RELEASE)(
                self.as_ptr(),
            );
        }
    }
}

/// The method at `slot` of the vtable `object` points at.
///
/// # Safety
/// `object` must be a live Slang COM object whose interface has a method of
/// signature `F` at `slot`.
pub(crate) unsafe fn vtable_method<F: Copy>(object: *mut c_void, slot: usize) -> F {
    // SAFETY: a COM object's first word points at its vtable, an array of
    // function pointers; the caller vouches for the slot and its signature.
    unsafe {
        let vtable = *object.cast::<*const *const c_void>();
        let function_pointer = *vtable.add(slot);
        std::mem::transmute_copy::<*const c_void, F>(&function_pointer)
    }
}
