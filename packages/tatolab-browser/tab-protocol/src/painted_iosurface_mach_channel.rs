// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The Mach channel an IOSurface the page painted into crosses from the
//! browser host to its node on macOS: a Unix socket carries no Mach port, so
//! the node checks a bootstrap name in, the host looks it up, and each
//! painted texture crosses as one message moving the IOSurface's send right.

use std::ffi::{CString, c_char, c_void};
use std::io;
use std::time::Duration;

type MachPortName = u32;
type KernReturn = i32;
type MachMsgReturn = i32;

const KERN_SUCCESS: KernReturn = 0;
const MACH_PORT_NULL: MachPortName = 0;
const MACH_PORT_RIGHT_RECEIVE: u32 = 1;
const MACH_MSG_TYPE_MOVE_SEND: u32 = 17;
const MACH_MSG_TYPE_COPY_SEND: u32 = 19;
const MACH_MSGH_BITS_COMPLEX: u32 = 0x8000_0000;
const MACH_MSG_PORT_DESCRIPTOR: u8 = 0;
const MACH_SEND_MSG: i32 = 0x0000_0001;
const MACH_RCV_MSG: i32 = 0x0000_0002;
const MACH_SEND_TIMEOUT: i32 = 0x0000_0010;
const MACH_RCV_TIMEOUT: i32 = 0x0000_0100;
const MACH_RCV_TIMED_OUT: MachMsgReturn = 0x1000_4003;
const BOOTSTRAP_MAX_NAME_LEN: usize = 128;

/// The `msgh_id` a painted IOSurface message carries.
const PAINTED_IOSURFACE_MESSAGE_ID: i32 = 0x7462_7073;

/// `mach_msg_header_t`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct MachMessageHeader {
    bits: u32,
    size: u32,
    remote_port: MachPortName,
    local_port: MachPortName,
    voucher_port: MachPortName,
    id: i32,
}

/// `mach_msg_port_descriptor_t`: `pad2:16`, `disposition:8` and `type:8`
/// share the last word, low bits first.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct MachMessagePortDescriptor {
    name: MachPortName,
    pad1: u32,
    pad2: u16,
    disposition: u8,
    descriptor_type: u8,
}

/// One painted IOSurface as it crosses: the header, a one-descriptor body,
/// the IOSurface's send right, and the painted texture number. Mach packs
/// message structures to 4 bytes.
#[repr(C, packed(4))]
#[derive(Clone, Copy, Default)]
struct PaintedIOSurfaceMachMessage {
    header: MachMessageHeader,
    descriptor_count: u32,
    iosurface: MachMessagePortDescriptor,
    painted_texture_number: u64,
}

/// A received [`PaintedIOSurfaceMachMessage`] with room for the trailer the
/// kernel appends.
#[repr(C, packed(4))]
#[derive(Clone, Copy)]
struct ReceivedPaintedIOSurfaceMachMessage {
    message: PaintedIOSurfaceMachMessage,
    trailer: [u8; 68],
}

unsafe extern "C" {
    static bootstrap_port: MachPortName;
    static mach_task_self_: MachPortName;
    fn bootstrap_check_in(
        bootstrap: MachPortName,
        service_name: *const c_char,
        service_port: *mut MachPortName,
    ) -> KernReturn;
    fn bootstrap_look_up(
        bootstrap: MachPortName,
        service_name: *const c_char,
        service_port: *mut MachPortName,
    ) -> KernReturn;
    fn mach_msg(
        message: *mut MachMessageHeader,
        option: i32,
        send_size: u32,
        receive_size: u32,
        receive_name: MachPortName,
        timeout_ms: u32,
        notify: MachPortName,
    ) -> MachMsgReturn;
    fn mach_port_deallocate(task: MachPortName, name: MachPortName) -> KernReturn;
    fn mach_port_mod_refs(
        task: MachPortName,
        name: MachPortName,
        right: u32,
        delta: i32,
    ) -> KernReturn;
}

#[link(name = "IOSurface", kind = "framework")]
unsafe extern "C" {
    fn IOSurfaceCreateMachPort(iosurface: *const c_void) -> MachPortName;
    fn IOSurfaceLookupFromMachPort(port: MachPortName) -> *const c_void;
    fn IOSurfaceGetID(iosurface: *const c_void) -> u32;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(object: *const c_void);
}

fn mach_failure(operation: &str, kern_return: i32) -> io::Error {
    io::Error::other(format!("{operation} failed with {kern_return:#x}"))
}

fn bootstrap_service_name(service_name: &str) -> io::Result<CString> {
    if service_name.len() >= BOOTSTRAP_MAX_NAME_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("the Mach service name {service_name:?} is longer than a bootstrap name"),
        ));
    }
    CString::new(service_name).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "a Mach service name holds a NUL",
        )
    })
}

fn milliseconds_of(timeout: Duration) -> u32 {
    u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX)
}

/// An IOSurface this process holds a reference to, released on drop.
pub struct RetainedPaintedIOSurface {
    iosurface: *const c_void,
}

// SAFETY: an IOSurfaceRef is a thread-safe CoreFoundation object.
unsafe impl Send for RetainedPaintedIOSurface {}

impl RetainedPaintedIOSurface {
    /// The `IOSurfaceRef`, live while this value is.
    pub fn as_iosurface_ref(&self) -> *const c_void {
        self.iosurface
    }

    /// The system-wide id IOSurface names this surface by.
    pub fn iosurface_id(&self) -> u32 {
        // SAFETY: `self.iosurface` is a live IOSurfaceRef this value retains.
        unsafe { IOSurfaceGetID(self.iosurface) }
    }
}

impl Drop for RetainedPaintedIOSurface {
    fn drop(&mut self) {
        // SAFETY: releases the reference IOSurfaceLookupFromMachPort returned.
        unsafe { CFRelease(self.iosurface) };
    }
}

/// The node's end: the receive right its bootstrap name was checked in with.
pub struct PaintedIOSurfaceReceiverOfTheNode {
    receive_right: MachPortName,
}

impl PaintedIOSurfaceReceiverOfTheNode {
    /// Check `service_name` in to this process's bootstrap namespace; the
    /// name goes away with the receiver.
    pub fn check_in(service_name: &str) -> io::Result<Self> {
        let service_name = bootstrap_service_name(service_name)?;
        let mut receive_right = MACH_PORT_NULL;
        // SAFETY: the task's own bootstrap port, a NUL-terminated name.
        let kern_return = unsafe {
            bootstrap_check_in(bootstrap_port, service_name.as_ptr(), &mut receive_right)
        };
        if kern_return != KERN_SUCCESS {
            return Err(mach_failure("bootstrap_check_in", kern_return));
        }
        Ok(Self { receive_right })
    }

    /// The IOSurface handed over as `painted_texture_number`, waiting at most
    /// `timeout`; one handed over earlier and never asked for is let go.
    pub fn receive_painted_iosurface_numbered(
        &self,
        painted_texture_number: u64,
        timeout: Duration,
    ) -> io::Result<RetainedPaintedIOSurface> {
        loop {
            let (received_number, iosurface) = self.receive_within(timeout)?;
            if received_number == painted_texture_number {
                return Ok(iosurface);
            }
            if received_number > painted_texture_number {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "painted texture {received_number} arrived while {painted_texture_number} \
                         was awaited"
                    ),
                ));
            }
        }
    }

    fn receive_within(&self, timeout: Duration) -> io::Result<(u64, RetainedPaintedIOSurface)> {
        let mut received = ReceivedPaintedIOSurfaceMachMessage {
            message: PaintedIOSurfaceMachMessage::default(),
            trailer: [0; 68],
        };
        // SAFETY: a receive into a buffer of the stated size on this
        // receiver's own right.
        let mach_return = unsafe {
            mach_msg(
                std::ptr::addr_of_mut!(received).cast(),
                MACH_RCV_MSG | MACH_RCV_TIMEOUT,
                0,
                size_of::<ReceivedPaintedIOSurfaceMachMessage>() as u32,
                self.receive_right,
                milliseconds_of(timeout),
                MACH_PORT_NULL,
            )
        };
        if mach_return == MACH_RCV_TIMED_OUT {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("no painted IOSurface arrived within {timeout:?}"),
            ));
        }
        if mach_return != KERN_SUCCESS {
            return Err(mach_failure("mach_msg receive", mach_return));
        }
        let message = received.message;
        let header = message.header;
        let descriptor_count = message.descriptor_count;
        let iosurface_port = message.iosurface;
        let painted_texture_number = message.painted_texture_number;
        if header.id != PAINTED_IOSURFACE_MESSAGE_ID
            || header.bits & MACH_MSGH_BITS_COMPLEX == 0
            || descriptor_count != 1
            || iosurface_port.descriptor_type != MACH_MSG_PORT_DESCRIPTOR
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "a message {} with {descriptor_count} descriptors is no painted IOSurface",
                    header.id
                ),
            ));
        }
        // SAFETY: the descriptor moved a send right into this task; the
        // lookup takes its own reference, and the right is released after.
        let iosurface = unsafe { IOSurfaceLookupFromMachPort(iosurface_port.name) };
        // SAFETY: as above.
        unsafe { mach_port_deallocate(mach_task_self_, iosurface_port.name) };
        if iosurface.is_null() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("painted texture {painted_texture_number}'s port names no IOSurface"),
            ));
        }
        Ok((
            painted_texture_number,
            RetainedPaintedIOSurface { iosurface },
        ))
    }
}

impl Drop for PaintedIOSurfaceReceiverOfTheNode {
    fn drop(&mut self) {
        // SAFETY: destroys the receive right this value owns, which removes
        // the bootstrap name.
        unsafe {
            mach_port_mod_refs(
                mach_task_self_,
                self.receive_right,
                MACH_PORT_RIGHT_RECEIVE,
                -1,
            )
        };
    }
}

/// The browser host's end: a send right to the node's receiver.
pub struct PaintedIOSurfaceSenderOfTheBrowserHost {
    send_right: MachPortName,
}

// SAFETY: a Mach port name is a task-wide integer; `mach_msg` is thread-safe.
unsafe impl Send for PaintedIOSurfaceSenderOfTheBrowserHost {}
// SAFETY: as above.
unsafe impl Sync for PaintedIOSurfaceSenderOfTheBrowserHost {}

impl PaintedIOSurfaceSenderOfTheBrowserHost {
    /// Look up the receiver the node checked in as `service_name`.
    pub fn look_up(service_name: &str) -> io::Result<Self> {
        let service_name = bootstrap_service_name(service_name)?;
        let mut send_right = MACH_PORT_NULL;
        // SAFETY: as in `check_in`.
        let kern_return =
            unsafe { bootstrap_look_up(bootstrap_port, service_name.as_ptr(), &mut send_right) };
        if kern_return != KERN_SUCCESS {
            return Err(mach_failure("bootstrap_look_up", kern_return));
        }
        Ok(Self { send_right })
    }

    /// Hand `iosurface` over as `painted_texture_number`, waiting at most
    /// `timeout` for room.
    ///
    /// # Safety
    ///
    /// `iosurface` is null or an `IOSurfaceRef` live for the call.
    pub unsafe fn send_painted_iosurface(
        &self,
        iosurface: *const c_void,
        painted_texture_number: u64,
        timeout: Duration,
    ) -> io::Result<()> {
        if iosurface.is_null() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a painted texture names a null IOSurface",
            ));
        }
        // SAFETY: the caller hands a live IOSurfaceRef; the call mints a send right this
        // function moves into the message.
        let iosurface_port = unsafe { IOSurfaceCreateMachPort(iosurface) };
        if iosurface_port == MACH_PORT_NULL {
            return Err(io::Error::other("the IOSurface minted no Mach port"));
        }
        let mut message = PaintedIOSurfaceMachMessage {
            header: MachMessageHeader {
                bits: MACH_MSG_TYPE_COPY_SEND | MACH_MSGH_BITS_COMPLEX,
                size: size_of::<PaintedIOSurfaceMachMessage>() as u32,
                remote_port: self.send_right,
                local_port: MACH_PORT_NULL,
                voucher_port: MACH_PORT_NULL,
                id: PAINTED_IOSURFACE_MESSAGE_ID,
            },
            descriptor_count: 1,
            iosurface: MachMessagePortDescriptor {
                name: iosurface_port,
                pad1: 0,
                pad2: 0,
                disposition: MACH_MSG_TYPE_MOVE_SEND as u8,
                descriptor_type: MACH_MSG_PORT_DESCRIPTOR,
            },
            painted_texture_number,
        };
        // SAFETY: a send of a fully initialised message of the stated size.
        let mach_return = unsafe {
            mach_msg(
                std::ptr::addr_of_mut!(message).cast(),
                MACH_SEND_MSG | MACH_SEND_TIMEOUT,
                size_of::<PaintedIOSurfaceMachMessage>() as u32,
                0,
                MACH_PORT_NULL,
                milliseconds_of(timeout),
                MACH_PORT_NULL,
            )
        };
        if mach_return != KERN_SUCCESS {
            // SAFETY: a failed send leaves the moved right with this task.
            unsafe { mach_port_deallocate(mach_task_self_, iosurface_port) };
            return Err(mach_failure("mach_msg send", mach_return));
        }
        Ok(())
    }
}

impl Drop for PaintedIOSurfaceSenderOfTheBrowserHost {
    fn drop(&mut self) {
        // SAFETY: releases the send right `look_up` took.
        unsafe { mach_port_deallocate(mach_task_self_, self.send_right) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mach_message_structures_have_the_kernels_layout() {
        assert_eq!(size_of::<MachMessageHeader>(), 24);
        assert_eq!(size_of::<MachMessagePortDescriptor>(), 12);
        assert_eq!(size_of::<PaintedIOSurfaceMachMessage>(), 48);
        assert_eq!(
            std::mem::offset_of!(PaintedIOSurfaceMachMessage, iosurface),
            28
        );
        assert_eq!(
            std::mem::offset_of!(PaintedIOSurfaceMachMessage, painted_texture_number),
            40
        );
    }

    #[test]
    fn a_name_longer_than_a_bootstrap_name_is_refused() {
        let Err(refusal) = PaintedIOSurfaceReceiverOfTheNode::check_in(&"n".repeat(200)) else {
            panic!("a 200-byte name is no bootstrap name");
        };
        assert_eq!(refusal.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn a_painted_iosurface_crosses_to_the_receiver_with_its_number() {
        let service_name = format!("com.tatolab.browser.paint.test.{}", std::process::id());
        let receiver = PaintedIOSurfaceReceiverOfTheNode::check_in(&service_name).unwrap();
        let sender = PaintedIOSurfaceSenderOfTheBrowserHost::look_up(&service_name).unwrap();
        let iosurface = test_iosurface::a_small_bgra_iosurface();
        let sent_id = unsafe { IOSurfaceGetID(iosurface) };
        for painted_texture_number in [1, 2] {
            unsafe {
                sender.send_painted_iosurface(
                    iosurface,
                    painted_texture_number,
                    Duration::from_secs(1),
                )
            }
            .unwrap();
        }
        let received = receiver
            .receive_painted_iosurface_numbered(2, Duration::from_secs(1))
            .unwrap();
        assert_eq!(received.iosurface_id(), sent_id);
        unsafe { CFRelease(iosurface) };
    }

    #[test]
    fn a_receive_with_nothing_sent_times_out() {
        let service_name = format!("com.tatolab.browser.paint.idle.{}", std::process::id());
        let receiver = PaintedIOSurfaceReceiverOfTheNode::check_in(&service_name).unwrap();
        let Err(refusal) =
            receiver.receive_painted_iosurface_numbered(1, Duration::from_millis(10))
        else {
            panic!("nothing was sent");
        };
        assert_eq!(refusal.kind(), io::ErrorKind::TimedOut);
    }

    mod test_iosurface {
        use std::ffi::c_void;

        #[link(name = "CoreFoundation", kind = "framework")]
        unsafe extern "C" {
            static kCFTypeDictionaryKeyCallBacks: u8;
            static kCFTypeDictionaryValueCallBacks: u8;
            fn CFDictionaryCreateMutable(
                allocator: *const c_void,
                capacity: isize,
                key_callbacks: *const c_void,
                value_callbacks: *const c_void,
            ) -> *mut c_void;
            fn CFDictionarySetValue(
                dictionary: *mut c_void,
                key: *const c_void,
                value: *const c_void,
            );
            fn CFNumberCreate(
                allocator: *const c_void,
                number_type: isize,
                value: *const c_void,
            ) -> *const c_void;
            fn CFRelease(object: *const c_void);
        }

        #[link(name = "IOSurface", kind = "framework")]
        unsafe extern "C" {
            static kIOSurfaceWidth: *const c_void;
            static kIOSurfaceHeight: *const c_void;
            static kIOSurfaceBytesPerElement: *const c_void;
            static kIOSurfacePixelFormat: *const c_void;
            fn IOSurfaceCreate(properties: *const c_void) -> *const c_void;
        }

        const K_CF_NUMBER_SINT32_TYPE: isize = 3;
        const BGRA_FOURCC: i32 = i32::from_be_bytes(*b"BGRA");

        /// A 16 × 16 BGRA IOSurface, retained once for the caller.
        pub fn a_small_bgra_iosurface() -> *const c_void {
            unsafe {
                let properties = CFDictionaryCreateMutable(
                    std::ptr::null(),
                    0,
                    std::ptr::addr_of!(kCFTypeDictionaryKeyCallBacks).cast(),
                    std::ptr::addr_of!(kCFTypeDictionaryValueCallBacks).cast(),
                );
                for (key, value) in [
                    (kIOSurfaceWidth, 16i32),
                    (kIOSurfaceHeight, 16),
                    (kIOSurfaceBytesPerElement, 4),
                    (kIOSurfacePixelFormat, BGRA_FOURCC),
                ] {
                    let number = CFNumberCreate(
                        std::ptr::null(),
                        K_CF_NUMBER_SINT32_TYPE,
                        std::ptr::addr_of!(value).cast(),
                    );
                    CFDictionarySetValue(properties, key, number);
                    CFRelease(number);
                }
                let iosurface = IOSurfaceCreate(properties);
                CFRelease(properties);
                assert!(!iosurface.is_null(), "IOSurfaceCreate");
                iosurface
            }
        }
    }
}
