// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A Metal shared event's handle, carried across processes as one Mach send
//! right.
//!
//! `MTLSharedEventHandle` encodes only into an `NSXPCCoder`, and what it
//! encodes is one Mach send right under the key `Port` (plus a label, nil for
//! a MoltenVK-exported event). A subclass of the public `NSXPCCoder` with no
//! connection captures that right on encode and replays it on decode, so the
//! right rides the surface-share Mach channel beside the IOSurface port. The
//! route depends on that encoded shape; a shape it does not recognise is an
//! error, never a guess, and the caller falls back to host-side ordering.

use std::cell::RefCell;
use std::ffi::{CStr, c_char, c_void};
use std::io;

use mach2::port::{MACH_PORT_DEAD, MACH_PORT_NULL, mach_port_name_t};
use objc2::encode::{Encode, Encoding, RefEncode};
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Bool};
use objc2::{AllocAnyThread, DefinedClass, define_class, msg_send};
use objc2_foundation::{NSCoder, NSCoding, NSObject, NSSet, NSString, NSXPCCoder};
use objc2_metal::MTLSharedEventHandle;

use crate::OwnedMachSendRight;

/// The key `MTLSharedEventHandle` encodes its Mach send right under.
const METAL_SHARED_EVENT_HANDLE_PORT_KEY: &str = "Port";

fn is_the_port_key(key: &NSString) -> bool {
    key.isEqualToString(&NSString::from_str(METAL_SHARED_EVENT_HANDLE_PORT_KEY))
}

/// Scratch key the send right is parked under in a one-entry XPC dictionary.
const XPC_DICTIONARY_SCRATCH_KEY: &CStr = c"port";

// libxpc's public `<xpc/xpc.h>`, part of libSystem; objc2 generates no
// bindings for it.
unsafe extern "C" {
    fn xpc_dictionary_create(
        keys: *const *const c_char,
        values: *const *mut AnyObject,
        count: usize,
    ) -> *mut AnyObject;
    fn xpc_dictionary_set_value(
        dictionary: *mut AnyObject,
        key: *const c_char,
        value: *mut AnyObject,
    );
    fn xpc_dictionary_get_value(dictionary: *mut AnyObject, key: *const c_char) -> *mut AnyObject;
    fn xpc_dictionary_set_mach_send(
        dictionary: *mut AnyObject,
        key: *const c_char,
        port: mach_port_name_t,
    );
    fn xpc_dictionary_copy_mach_send(
        dictionary: *mut AnyObject,
        key: *const c_char,
    ) -> mach_port_name_t;
    fn xpc_get_type(object: *mut AnyObject) -> XpcType;
}

/// `xpc_type_t`: a pointer to libxpc's opaque `struct _xpc_type_s`.
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq)]
struct XpcType(*const c_void);

// SAFETY: a transparent pointer to the opaque struct the method signature
// names, encoded as `^{_xpc_type_s=}`.
unsafe impl Encode for XpcType {
    const ENCODING: Encoding = Encoding::Pointer(&Encoding::Struct("_xpc_type_s", &[]));
}

// SAFETY: as above, one pointer deeper.
unsafe impl RefEncode for XpcType {
    const ENCODING_REF: Encoding = Encoding::Pointer(&Self::ENCODING);
}

/// A fresh, empty XPC dictionary, released on drop.
fn new_xpc_dictionary() -> io::Result<Retained<AnyObject>> {
    // SAFETY: an empty dictionary; the create call returns +1.
    let dictionary = unsafe { xpc_dictionary_create(std::ptr::null(), std::ptr::null(), 0) };
    // SAFETY: XPC objects are Objective-C objects; `from_raw` adopts the +1.
    unsafe { Retained::from_raw(dictionary) }
        .ok_or_else(|| io::Error::other("xpc_dictionary_create returned NULL"))
}

#[derive(Default)]
struct MachPortCapturingXPCCoderIvars {
    captured_port_objects: RefCell<Vec<(String, Retained<AnyObject>)>>,
    replayed_port_object: RefCell<Option<Retained<AnyObject>>>,
}

define_class!(
    // SAFETY: every overridden selector keeps `NSXPCCoder`'s signature and
    // type encoding — `XpcType` encodes as `^{_xpc_type_s=}`, and the keys
    // are non-null `NSString`s by the `NSCoder` contract — and the subclass
    // does not implement `Drop`.
    #[unsafe(super(NSXPCCoder, NSCoder, NSObject))]
    #[ivars = MachPortCapturingXPCCoderIvars]
    #[name = "StreamlibMachPortCapturingXPCCoder"]
    struct MachPortCapturingXPCCoder;

    impl MachPortCapturingXPCCoder {
        #[unsafe(method(allowsKeyedCoding))]
        fn allows_keyed_coding(&self) -> Bool {
            Bool::YES
        }

        #[unsafe(method(requiresSecureCoding))]
        fn requires_secure_coding(&self) -> Bool {
            Bool::YES
        }

        #[unsafe(method(encodeXPCObject:forKey:))]
        fn encode_xpc_object(&self, object: *mut AnyObject, key: &NSString) {
            // SAFETY: the coder hands a live XPC object; retaining keeps it
            // past the call.
            if let Some(object) = unsafe { Retained::retain(object) } {
                self.ivars()
                    .captured_port_objects
                    .borrow_mut()
                    .push((key.to_string(), object));
            }
        }

        #[unsafe(method(decodeXPCObjectOfType:forKey:))]
        fn decode_xpc_object(&self, xpc_type: XpcType, key: &NSString) -> *mut AnyObject {
            if !is_the_port_key(key) {
                return std::ptr::null_mut();
            }
            let replayed = self.ivars().replayed_port_object.borrow();
            match replayed.as_ref() {
                Some(object) => {
                    let raw = Retained::as_ptr(object).cast_mut();
                    // SAFETY: `raw` is a live XPC object this coder holds.
                    if unsafe { xpc_get_type(raw) } == xpc_type {
                        raw
                    } else {
                        std::ptr::null_mut()
                    }
                }
                None => std::ptr::null_mut(),
            }
        }

        #[unsafe(method(encodeObject:forKey:))]
        fn encode_object(&self, _object: *mut AnyObject, _key: &NSString) {}

        #[unsafe(method(decodeObjectOfClass:forKey:))]
        fn decode_object_of_class(&self, _class: *const AnyClass, _key: &NSString) -> *mut AnyObject {
            std::ptr::null_mut()
        }

        #[unsafe(method(decodeObjectOfClasses:forKey:))]
        fn decode_object_of_classes(
            &self,
            _classes: *mut NSSet<AnyClass>,
            _key: &NSString,
        ) -> *mut AnyObject {
            std::ptr::null_mut()
        }

        #[unsafe(method(containsValueForKey:))]
        fn contains_value_for_key(&self, key: &NSString) -> Bool {
            Bool::new(is_the_port_key(key) && self.ivars().replayed_port_object.borrow().is_some())
        }
    }
);

impl MachPortCapturingXPCCoder {
    fn new_with_ivars(ivars: MachPortCapturingXPCCoderIvars) -> Retained<Self> {
        let this = Self::alloc().set_ivars(ivars);
        // SAFETY: `init` is `NSObject`'s designated initializer; the ivars
        // are set.
        unsafe { msg_send![super(this), init] }
    }
}

/// A send right to the Mach port behind `handle`, minted through its
/// `NSXPCCoder` encoding. Errors when the encoding is not the one send right
/// under `Port` this route relies on.
pub fn mach_send_right_of_metal_shared_event_handle(
    handle: &MTLSharedEventHandle,
) -> io::Result<OwnedMachSendRight> {
    let coder = MachPortCapturingXPCCoder::new_with_ivars(Default::default());
    objc2::exception::catch(std::panic::AssertUnwindSafe(|| {
        // SAFETY: the coder is a live `NSXPCCoder` subclass accepting keyed,
        // secure coding.
        unsafe { handle.encodeWithCoder(&coder) }
    }))
    .map_err(|exception| {
        io::Error::other(format!(
            "MTLSharedEventHandle refused to encode into the XPC coder: {exception:?}"
        ))
    })?;

    let captured = coder.ivars().captured_port_objects.take();
    let [(key, port_object)] = <[_; 1]>::try_from(captured).map_err(|captured| {
        io::Error::other(format!(
            "MTLSharedEventHandle encoded {} XPC objects; this route expects one send right",
            captured.len()
        ))
    })?;
    if key != METAL_SHARED_EVENT_HANDLE_PORT_KEY {
        return Err(io::Error::other(format!(
            "MTLSharedEventHandle encoded its XPC object under {key:?}, not \
             {METAL_SHARED_EVENT_HANDLE_PORT_KEY:?}"
        )));
    }
    let port_object_raw = Retained::as_ptr(&port_object).cast_mut();

    let dictionary = new_xpc_dictionary()?;
    let dictionary_raw = Retained::as_ptr(&dictionary).cast_mut();
    // SAFETY: both are live XPC objects; the dictionary retains the value, and
    // the copy returns a send right this task now owns one reference to.
    let port = unsafe {
        xpc_dictionary_set_value(
            dictionary_raw,
            XPC_DICTIONARY_SCRATCH_KEY.as_ptr(),
            port_object_raw,
        );
        xpc_dictionary_copy_mach_send(dictionary_raw, XPC_DICTIONARY_SCRATCH_KEY.as_ptr())
    };
    // `copy_mach_send` answers null for a value that is not a send right.
    if port == MACH_PORT_NULL || port == MACH_PORT_DEAD {
        return Err(io::Error::other(
            "MTLSharedEventHandle encoded an XPC object that is not a Mach send right",
        ));
    }
    // SAFETY: `copy_mach_send` handed this task one reference.
    Ok(unsafe { OwnedMachSendRight::from_raw_name(port) })
}

/// A `MTLSharedEventHandle` rebuilt from a send right minted by
/// [`mach_send_right_of_metal_shared_event_handle`] in another process. A
/// port that names no shared event still builds a handle; it is
/// `newSharedEventWithHandle:` that answers nil for it.
pub fn metal_shared_event_handle_of_mach_send_right(
    send_right: &OwnedMachSendRight,
) -> io::Result<Retained<MTLSharedEventHandle>> {
    let dictionary = new_xpc_dictionary()?;
    let dictionary_raw = Retained::as_ptr(&dictionary).cast_mut();
    // SAFETY: the dictionary copies its own send right; `get_value` returns
    // an object the dictionary keeps alive, retained here past it.
    let port_object = unsafe {
        xpc_dictionary_set_mach_send(
            dictionary_raw,
            XPC_DICTIONARY_SCRATCH_KEY.as_ptr(),
            send_right.as_raw_name(),
        );
        Retained::retain(xpc_dictionary_get_value(
            dictionary_raw,
            XPC_DICTIONARY_SCRATCH_KEY.as_ptr(),
        ))
    }
    .ok_or_else(|| io::Error::other("the XPC dictionary did not keep the shared event's port"))?;

    let coder = MachPortCapturingXPCCoder::new_with_ivars(MachPortCapturingXPCCoderIvars {
        replayed_port_object: RefCell::new(Some(port_object)),
        ..Default::default()
    });
    let handle: Option<Retained<MTLSharedEventHandle>> =
        objc2::exception::catch(std::panic::AssertUnwindSafe(|| {
            // SAFETY: `initWithCoder:` is `MTLSharedEventHandle`'s `NSCoding`
            // initializer; the coder replays the one key it reads.
            unsafe { msg_send![MTLSharedEventHandle::alloc(), initWithCoder: &*coder] }
        }))
        .map_err(|exception| {
            io::Error::other(format!(
                "MTLSharedEventHandle refused to decode from the XPC coder: {exception:?}"
            ))
        })?;
    handle.ok_or_else(|| io::Error::other("MTLSharedEventHandle initWithCoder: returned nil"))
}

#[cfg(test)]
mod tests {
    use objc2::runtime::ProtocolObject;
    use objc2_metal::{MTLCreateSystemDefaultDevice, MTLDevice, MTLSharedEvent};

    use super::*;

    fn a_shared_event() -> Option<Retained<ProtocolObject<dyn MTLSharedEvent>>> {
        MTLCreateSystemDefaultDevice().and_then(|device| device.newSharedEvent())
    }

    #[test]
    fn a_shared_event_round_trips_through_its_mach_send_right() {
        let Some(original) = a_shared_event() else {
            return;
        };
        original.setSignaledValue(41);
        let send_right =
            mach_send_right_of_metal_shared_event_handle(&original.newSharedEventHandle())
                .expect("encode the handle's send right");
        let rebuilt_handle =
            metal_shared_event_handle_of_mach_send_right(&send_right).expect("rebuild the handle");
        let device = MTLCreateSystemDefaultDevice().expect("a Metal device");
        let rebuilt = device
            .newSharedEventWithHandle(&rebuilt_handle)
            .expect("the rebuilt handle names the same event");

        assert_eq!(rebuilt.signaledValue(), 41);
        rebuilt.setSignaledValue(77);
        assert_eq!(original.signaledValue(), 77);
    }

    #[test]
    fn a_port_naming_no_shared_event_rebuilds_no_event() {
        let Some(device) = MTLCreateSystemDefaultDevice() else {
            return;
        };
        let unrelated = crate::OwnedMachReceiveRight::allocate().expect("a receive right");
        let send_right = unrelated.make_send_right().expect("a send right");
        let handle = metal_shared_event_handle_of_mach_send_right(&send_right)
            .expect("any send right builds a handle");
        assert!(device.newSharedEventWithHandle(&handle).is_none());
    }
}
