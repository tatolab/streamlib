// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::ffi::{CStr, CString};
use std::io;

use mach2::bootstrap::{
    BOOTSTRAP_MAX_NAME_LEN, BOOTSTRAP_SERVICE_ACTIVE, bootstrap_check_in, bootstrap_look_up,
    bootstrap_port,
};
use mach2::kern_return::{KERN_SUCCESS, kern_return_t};
use mach2::mach_port::{
    mach_port_allocate, mach_port_deallocate, mach_port_insert_right, mach_port_mod_refs,
    mach_port_request_notification,
};
use mach2::message::{
    MACH_MSG_TYPE_MAKE_SEND, MACH_MSG_TYPE_MAKE_SEND_ONCE, MACH_RCV_TRAILER_AUDIT,
    mach_msg_audit_trailer_t, mach_msg_body_t, mach_msg_header_t, mach_msg_id_t, mach_msg_option_t,
    mach_msg_port_descriptor_t,
};
use mach2::notify::{MACH_NOTIFY_DEAD_NAME, MACH_NOTIFY_NO_SENDERS};
use mach2::port::{
    MACH_PORT_DEAD, MACH_PORT_NULL, MACH_PORT_RIGHT_PORT_SET, MACH_PORT_RIGHT_RECEIVE,
    mach_port_name_t,
};
use mach2::traps::mach_task_self;

pub use surface_share_mach_messages::{
    ReceivedSurfaceShareMachMessage, ReceivedSurfaceShareMachTraffic,
    SurfaceShareMachMessageReceiveBuffer, SurfaceShareMachSenderAuditIdentity,
    receive_surface_share_mach_traffic, send_surface_share_mach_message,
};
pub use surface_share_mach_service_connection::SurfaceShareMachServiceConnection;

mod surface_share_mach_messages;
mod surface_share_mach_service_connection;
#[cfg(test)]
mod tests;

/// Environment variable a helper process reads the engine's surface-share
/// Mach service name from — the macOS peer of `STREAMLIB_SURFACE_SOCKET`.
pub const SURFACE_SHARE_MACH_SERVICE_ENVIRONMENT_VARIABLE: &str = "STREAMLIB_SURFACE_MACH_SERVICE";

/// Most port rights one surface-share message carries: an IOSurface, plus
/// headroom for the timeline edges that ride beside it.
pub const MAX_SURFACE_SHARE_MACH_MESSAGE_PORTS: usize = 4;

/// Flag on a registration or a lookup reply: a `produce_done` shared-event
/// port follows the IOSurface port.
pub const SURFACE_SHARE_HAS_PRODUCE_DONE_PORT: &str = "has_produce_done_port";

/// Flag on a registration or a lookup reply: a `consume_done` shared-event
/// port follows the `produce_done` one.
pub const SURFACE_SHARE_HAS_CONSUME_DONE_PORT: &str = "has_consume_done_port";

/// Verb a helper reports its release of a frame with when its surface orders
/// host-side: `{"op", "surface_id", "value"}`.
pub const SURFACE_SHARE_OP_SIGNAL_CONSUME_DONE: &str = "signal_consume_done";

/// Verb a helper reports a timeline pair it could not import with, moving the
/// surface to host-side ordering: `{"op", "surface_id", "reason"}`.
pub const SURFACE_SHARE_OP_TIMELINE_IMPORT_REFUSED: &str = "timeline_import_refused";

/// Largest JSON payload one surface-share message carries.
pub const MAX_SURFACE_SHARE_MACH_MESSAGE_JSON_BYTES: usize = 64 * 1024;

/// `msgh_id` of the message a client opens a connection with.
pub const SURFACE_SHARE_MACH_CONNECT_MESSAGE_ID: mach_msg_id_t = 0x534C_5301;

/// `msgh_id` of a request on an open connection.
pub const SURFACE_SHARE_MACH_REQUEST_MESSAGE_ID: mach_msg_id_t = 0x534C_5302;

/// `msgh_id` of every service reply, the connect answer included.
pub const SURFACE_SHARE_MACH_REPLY_MESSAGE_ID: mach_msg_id_t = 0x534C_5303;

/// Header and descriptor count every surface-share message opens with. The
/// port descriptors follow, then a native-endian `u32` JSON byte length and
/// the JSON itself, zero-padded to the next four bytes.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct SurfaceShareMachMessagePrefix {
    header: mach_msg_header_t,
    body: mach_msg_body_t,
}

const SURFACE_SHARE_MACH_MESSAGE_PREFIX_BYTES: usize =
    std::mem::size_of::<SurfaceShareMachMessagePrefix>();
const PORT_DESCRIPTOR_BYTES: usize = std::mem::size_of::<mach_msg_port_descriptor_t>();
const JSON_LENGTH_PREFIX_BYTES: usize = std::mem::size_of::<u32>();
const AUDIT_TRAILER_BYTES: usize = std::mem::size_of::<mach_msg_audit_trailer_t>();

/// `MACH_RCV_TRAILER_TYPE(MACH_MSG_TRAILER_FORMAT_0) |
/// MACH_RCV_TRAILER_ELEMENTS(MACH_RCV_TRAILER_AUDIT)` — asks the kernel to
/// append the sender's audit token to every received message.
const RECEIVE_THE_AUDIT_TRAILER: mach_msg_option_t = ((MACH_RCV_TRAILER_AUDIT & 0xf) << 24) as _;

/// Bytes a receive buffer must hold for the largest message the wire allows,
/// with its audit trailer.
const LARGEST_RECEIVED_SURFACE_SHARE_MACH_MESSAGE_BYTES: usize =
    SURFACE_SHARE_MACH_MESSAGE_PREFIX_BYTES
        + MAX_SURFACE_SHARE_MACH_MESSAGE_PORTS * PORT_DESCRIPTOR_BYTES
        + JSON_LENGTH_PREFIX_BYTES
        + MAX_SURFACE_SHARE_MACH_MESSAGE_JSON_BYTES
        + 4
        + AUDIT_TRAILER_BYTES;

unsafe extern "C" {
    fn mach_port_insert_member(
        task: mach_port_name_t,
        member: mach_port_name_t,
        port_set: mach_port_name_t,
    ) -> kern_return_t;
    fn mach_error_string(error_value: kern_return_t) -> *const libc::c_char;
}

/// `operation`'s Mach failure as an `io::Error` naming the kernel's code.
fn mach_failure(operation: &str, kern_return: kern_return_t) -> io::Error {
    // SAFETY: `mach_error_string` returns a static C string for every value.
    let description = unsafe { CStr::from_ptr(mach_error_string(kern_return)) }.to_string_lossy();
    io::Error::other(format!(
        "{operation} failed: {description} ({kern_return:#x})"
    ))
}

/// One user reference to a send right (or a dead name) in this task,
/// released on drop.
#[derive(Debug)]
pub struct OwnedMachSendRight {
    name: mach_port_name_t,
}

impl OwnedMachSendRight {
    /// Adopt one user reference to the send right `name`.
    ///
    /// # Safety
    /// The caller owns that reference and hands it over; nothing else
    /// releases it.
    pub unsafe fn from_raw_name(name: mach_port_name_t) -> Self {
        Self { name }
    }

    /// The right's name in this task.
    pub fn as_raw_name(&self) -> mach_port_name_t {
        self.name
    }

    /// Surrender the reference to the caller.
    pub fn into_raw_name(self) -> mach_port_name_t {
        let name = self.name;
        std::mem::forget(self);
        name
    }

    /// A second user reference to the same right.
    pub fn try_clone(&self) -> io::Result<Self> {
        if self.name == MACH_PORT_NULL || self.name == MACH_PORT_DEAD {
            return Ok(Self { name: self.name });
        }
        // SAFETY: adds one reference to a right this task holds.
        let kern_return = unsafe {
            mach_port_mod_refs(
                mach_task_self(),
                self.name,
                mach2::port::MACH_PORT_RIGHT_SEND,
                1,
            )
        };
        if kern_return != KERN_SUCCESS {
            return Err(mach_failure("mach_port_mod_refs(SEND, +1)", kern_return));
        }
        Ok(Self { name: self.name })
    }
}

impl Drop for OwnedMachSendRight {
    fn drop(&mut self) {
        if self.name != MACH_PORT_NULL && self.name != MACH_PORT_DEAD {
            // SAFETY: releases the one reference this value owns.
            unsafe { mach_port_deallocate(mach_task_self(), self.name) };
        }
    }
}

/// A receive right in this task, destroyed on drop — which turns every
/// sender's right into a dead name.
#[derive(Debug)]
pub struct OwnedMachReceiveRight {
    name: mach_port_name_t,
}

impl OwnedMachReceiveRight {
    /// Allocate a fresh receive right.
    pub fn allocate() -> io::Result<Self> {
        let mut name: mach_port_name_t = MACH_PORT_NULL;
        // SAFETY: `name` is a valid out-pointer.
        let kern_return =
            unsafe { mach_port_allocate(mach_task_self(), MACH_PORT_RIGHT_RECEIVE, &mut name) };
        if kern_return != KERN_SUCCESS {
            return Err(mach_failure("mach_port_allocate(RECEIVE)", kern_return));
        }
        Ok(Self { name })
    }

    /// The right's name in this task.
    pub fn as_raw_name(&self) -> mach_port_name_t {
        self.name
    }

    /// Mint a send right to this port.
    pub fn make_send_right(&self) -> io::Result<OwnedMachSendRight> {
        // SAFETY: inserts a send right under the receive right's own name.
        let kern_return = unsafe {
            mach_port_insert_right(
                mach_task_self(),
                self.name,
                self.name,
                MACH_MSG_TYPE_MAKE_SEND,
            )
        };
        if kern_return != KERN_SUCCESS {
            return Err(mach_failure(
                "mach_port_insert_right(MAKE_SEND)",
                kern_return,
            ));
        }
        Ok(OwnedMachSendRight { name: self.name })
    }

    /// Ask the kernel to deliver `MACH_NOTIFY_NO_SENDERS` to this port itself
    /// once no send right to it remains. Arms only after a send right to the
    /// port has been made; from then on a port with no senders notifies at
    /// once.
    pub fn request_no_senders_notification(&self) -> io::Result<()> {
        request_notification(
            self.name,
            MACH_NOTIFY_NO_SENDERS,
            self,
            "mach_port_request_notification(NO_SENDERS)",
        )
    }
}

impl Drop for OwnedMachReceiveRight {
    fn drop(&mut self) {
        // SAFETY: destroys the one receive right this value owns.
        unsafe { mach_port_mod_refs(mach_task_self(), self.name, MACH_PORT_RIGHT_RECEIVE, -1) };
    }
}

/// A port set, so one thread can wait on several receive rights at once.
#[derive(Debug)]
pub struct OwnedMachPortSet {
    name: mach_port_name_t,
}

impl OwnedMachPortSet {
    /// Allocate an empty port set.
    pub fn allocate() -> io::Result<Self> {
        let mut name: mach_port_name_t = MACH_PORT_NULL;
        // SAFETY: `name` is a valid out-pointer.
        let kern_return =
            unsafe { mach_port_allocate(mach_task_self(), MACH_PORT_RIGHT_PORT_SET, &mut name) };
        if kern_return != KERN_SUCCESS {
            return Err(mach_failure("mach_port_allocate(PORT_SET)", kern_return));
        }
        Ok(Self { name })
    }

    /// The set's name in this task, which a receive waits on.
    pub fn as_raw_name(&self) -> mach_port_name_t {
        self.name
    }

    /// Add `member` to the set. Destroying the member's receive right removes
    /// it again.
    pub fn insert_member(&self, member: &OwnedMachReceiveRight) -> io::Result<()> {
        // SAFETY: both names denote rights this task holds.
        let kern_return =
            unsafe { mach_port_insert_member(mach_task_self(), member.name, self.name) };
        if kern_return != KERN_SUCCESS {
            return Err(mach_failure("mach_port_insert_member", kern_return));
        }
        Ok(())
    }
}

impl Drop for OwnedMachPortSet {
    fn drop(&mut self) {
        // SAFETY: destroys the one port-set right this value owns.
        unsafe { mach_port_mod_refs(mach_task_self(), self.name, MACH_PORT_RIGHT_PORT_SET, -1) };
    }
}

/// Ask the kernel to deliver `MACH_NOTIFY_DEAD_NAME` to `notification_port`
/// when `send_right`'s receiver goes away — at once, if it already has.
pub fn request_dead_name_notification(
    send_right: &OwnedMachSendRight,
    notification_port: &OwnedMachReceiveRight,
) -> io::Result<()> {
    request_notification(
        send_right.name,
        MACH_NOTIFY_DEAD_NAME,
        notification_port,
        "mach_port_request_notification(DEAD_NAME)",
    )
}

fn request_notification(
    watched_name: mach_port_name_t,
    notification_id: libc::c_int,
    notification_port: &OwnedMachReceiveRight,
    operation: &str,
) -> io::Result<()> {
    let mut previous_notification_right: mach_port_name_t = MACH_PORT_NULL;
    // SAFETY: `watched_name` and the notification port are rights this task
    // holds; the kernel keeps the send-once right it mints for itself.
    let kern_return = unsafe {
        mach_port_request_notification(
            mach_task_self(),
            watched_name,
            notification_id,
            // Arms the request against the current state: a receiver already
            // gone, or no senders already, notifies immediately.
            1,
            notification_port.name,
            MACH_MSG_TYPE_MAKE_SEND_ONCE,
            &mut previous_notification_right,
        )
    };
    if kern_return != KERN_SUCCESS {
        return Err(mach_failure(operation, kern_return));
    }
    if previous_notification_right != MACH_PORT_NULL {
        // SAFETY: a replaced request hands its send-once right back to us.
        unsafe { mach_port_deallocate(mach_task_self(), previous_notification_right) };
    }
    Ok(())
}

/// Register `service_name` in this process's bootstrap namespace and take
/// its receive right — no launchd plist, no bundle. The name disappears when
/// the right is destroyed, including when this process dies.
///
/// A name another live process holds is refused with
/// [`io::ErrorKind::AddrInUse`].
pub fn check_in_surface_share_mach_service(
    service_name: &str,
) -> io::Result<OwnedMachReceiveRight> {
    let service_name_c_string = bootstrap_service_name(service_name)?;
    let mut receive_name: mach_port_name_t = MACH_PORT_NULL;
    // SAFETY: `bootstrap_port` is the task's inherited bootstrap right; the
    // name is NUL-terminated and within `BOOTSTRAP_MAX_NAME_LEN`.
    let kern_return = unsafe {
        bootstrap_check_in(
            bootstrap_port,
            service_name_c_string.as_ptr(),
            &mut receive_name,
        )
    };
    if kern_return == BOOTSTRAP_SERVICE_ACTIVE {
        return Err(io::Error::new(
            io::ErrorKind::AddrInUse,
            format!("the Mach service name '{service_name}' is held by a live process"),
        ));
    }
    if kern_return != KERN_SUCCESS {
        return Err(mach_failure(
            &format!("bootstrap_check_in('{service_name}')"),
            kern_return,
        ));
    }
    Ok(OwnedMachReceiveRight { name: receive_name })
}

fn look_up_surface_share_mach_service(service_name: &str) -> io::Result<OwnedMachSendRight> {
    let service_name_c_string = bootstrap_service_name(service_name)?;
    let mut send_name: mach_port_name_t = MACH_PORT_NULL;
    // SAFETY: as `check_in_surface_share_mach_service`.
    let kern_return = unsafe {
        bootstrap_look_up(
            bootstrap_port,
            service_name_c_string.as_ptr(),
            &mut send_name,
        )
    };
    if kern_return != KERN_SUCCESS {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "no surface-share Mach service is registered as '{service_name}' ({kern_return})"
            ),
        ));
    }
    Ok(OwnedMachSendRight { name: send_name })
}

fn bootstrap_service_name(service_name: &str) -> io::Result<CString> {
    if service_name.len() >= BOOTSTRAP_MAX_NAME_LEN as usize {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "Mach service name '{service_name}' is {} bytes; bootstrap names hold at most {}",
                service_name.len(),
                BOOTSTRAP_MAX_NAME_LEN - 1
            ),
        ));
    }
    CString::new(service_name).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("Mach service name '{service_name}' contains a NUL byte"),
        )
    })
}
