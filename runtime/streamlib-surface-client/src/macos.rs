// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::ffi::{CStr, CString};
use std::io;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

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
    MACH_MSG_PORT_DESCRIPTOR, MACH_MSG_TYPE_COPY_SEND, MACH_MSG_TYPE_MAKE_SEND,
    MACH_MSG_TYPE_MAKE_SEND_ONCE, MACH_MSG_TYPE_MOVE_SEND, MACH_MSG_TYPE_PORT_SEND, MACH_MSGH_BITS,
    MACH_MSGH_BITS_COMPLEX, MACH_MSGH_BITS_LOCAL_MASK, MACH_MSGH_BITS_REMOTE_MASK, MACH_RCV_MSG,
    MACH_RCV_TIMED_OUT, MACH_RCV_TIMEOUT, MACH_RCV_TOO_LARGE, MACH_RCV_TRAILER_AUDIT,
    MACH_SEND_INTERRUPTED, MACH_SEND_INVALID_DEST, MACH_SEND_MSG, MACH_SEND_TIMED_OUT,
    MACH_SEND_TIMEOUT, audit_token_t, mach_msg, mach_msg_audit_trailer_t, mach_msg_body_t,
    mach_msg_destroy, mach_msg_header_t, mach_msg_id_t, mach_msg_option_t,
    mach_msg_port_descriptor_t, mach_msg_timeout_t,
};
use mach2::notify::{
    MACH_NOTIFY_DEAD_NAME, MACH_NOTIFY_FIRST, MACH_NOTIFY_LAST, MACH_NOTIFY_NO_SENDERS,
    mach_dead_name_notification_t,
};
use mach2::port::{
    MACH_PORT_DEAD, MACH_PORT_NULL, MACH_PORT_RIGHT_PORT_SET, MACH_PORT_RIGHT_RECEIVE,
    mach_port_name_t,
};
use mach2::traps::mach_task_self;

/// Environment variable a helper process reads the engine's surface-share
/// Mach service name from — the macOS peer of `STREAMLIB_SURFACE_SOCKET`.
pub const SURFACE_SHARE_MACH_SERVICE_ENVIRONMENT_VARIABLE: &str = "STREAMLIB_SURFACE_MACH_SERVICE";

/// Most port rights one surface-share message carries: an IOSurface, plus
/// headroom for the timeline edges that ride beside it.
pub const MAX_SURFACE_SHARE_MACH_MESSAGE_PORTS: usize = 4;

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

#[link(name = "bsm")]
unsafe extern "C" {
    fn audit_token_to_pid(audit_token: audit_token_t) -> libc::pid_t;
    fn audit_token_to_pidversion(audit_token: audit_token_t) -> libc::c_int;
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

/// Who sent a message, from the audit trailer the kernel appends — not from
/// anything the sender wrote.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SurfaceShareMachSenderAuditIdentity {
    /// The sending process.
    pub pid: libc::pid_t,
    /// The kernel's generation count for that pid, which differs across pid
    /// reuse.
    pub pidversion: i32,
}

/// Send one surface-share message to `destination`, moving `ports` with it.
///
/// `reply_port`, when given, travels as a freshly made send right the
/// receiver answers on. `send_timeout` bounds how long a full destination
/// queue may block the sender; `None` waits. On failure every right in
/// `ports` is released rather than leaked.
pub fn send_surface_share_mach_message(
    destination: &OwnedMachSendRight,
    reply_port: Option<&OwnedMachReceiveRight>,
    message_id: mach_msg_id_t,
    json_payload: &[u8],
    ports: Vec<OwnedMachSendRight>,
    send_timeout: Option<Duration>,
) -> io::Result<()> {
    if ports.len() > MAX_SURFACE_SHARE_MACH_MESSAGE_PORTS {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "a surface-share message carries at most {MAX_SURFACE_SHARE_MACH_MESSAGE_PORTS} \
                 ports, not {}",
                ports.len()
            ),
        ));
    }
    if json_payload.len() > MAX_SURFACE_SHARE_MACH_MESSAGE_JSON_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "a surface-share message's JSON is at most {MAX_SURFACE_SHARE_MACH_MESSAGE_JSON_BYTES} \
                 bytes, not {}",
                json_payload.len()
            ),
        ));
    }

    let port_names: Vec<mach_port_name_t> =
        ports.iter().map(OwnedMachSendRight::as_raw_name).collect();
    let mut encoded_message = encode_surface_share_mach_message(
        destination.name,
        reply_port.map(OwnedMachReceiveRight::as_raw_name),
        message_id,
        json_payload,
        &port_names,
    );
    let message_bytes = encoded_message.byte_len as u32;
    let mut options = MACH_SEND_MSG;
    let mut timeout_milliseconds: mach_msg_timeout_t = 0;
    if let Some(send_timeout) = send_timeout {
        options |= MACH_SEND_TIMEOUT;
        timeout_milliseconds = mach_timeout_milliseconds(send_timeout);
    }
    // SAFETY: the buffer holds a well-formed message of `message_bytes`
    // bytes whose rights this task owns.
    let send_result = unsafe {
        mach_msg(
            encoded_message.header_mut(),
            options,
            message_bytes,
            0,
            MACH_PORT_NULL,
            timeout_milliseconds,
            MACH_PORT_NULL,
        )
    };
    match send_result {
        KERN_SUCCESS => {
            for moved in ports {
                let _ = moved.into_raw_name();
            }
            Ok(())
        }
        // The kernel handed the message back into this task's space — as a
        // pseudo-receive once its rights were copied in, untouched when the
        // destination was already dead — and destroying it releases the
        // rights it carries either way.
        MACH_SEND_TIMED_OUT | MACH_SEND_INTERRUPTED | MACH_SEND_INVALID_DEST => {
            for handed_back in ports {
                let _ = handed_back.into_raw_name();
            }
            let header = encoded_message.header_mut();
            // SAFETY: the buffer holds the handed-back message's header.
            let (local_bits, local_port) = unsafe {
                (
                    ((*header).msgh_bits & MACH_MSGH_BITS_LOCAL_MASK) >> 8,
                    (*header).msgh_local_port,
                )
            };
            // A pseudo-receive keeps the reply right it already minted in
            // `msgh_local_port`, which `mach_msg_destroy` never touches.
            if local_bits == MACH_MSG_TYPE_PORT_SEND && local_port != MACH_PORT_NULL {
                // SAFETY: the kernel minted this reference for the message.
                unsafe { mach_port_deallocate(mach_task_self(), local_port) };
            }
            // SAFETY: the buffer holds the handed-back message.
            unsafe { mach_msg_destroy(header) };
            Err(mach_send_failure(send_result))
        }
        // Any other failure is a malformed message that the kernel may have
        // copied part of before refusing, consuming those rights. Releasing
        // `ports` again could free a right some other holder now owns under
        // the same name, so they are left unreleased instead.
        _ => {
            for possibly_consumed in ports {
                let _ = possibly_consumed.into_raw_name();
            }
            Err(mach_send_failure(send_result))
        }
    }
}

fn mach_send_failure(send_result: kern_return_t) -> io::Error {
    let failure = mach_failure("mach_msg(SEND)", send_result);
    match send_result {
        MACH_SEND_TIMED_OUT => io::Error::new(io::ErrorKind::TimedOut, failure.to_string()),
        MACH_SEND_INVALID_DEST => io::Error::new(io::ErrorKind::BrokenPipe, failure.to_string()),
        _ => failure,
    }
}

fn mach_timeout_milliseconds(timeout: Duration) -> mach_msg_timeout_t {
    timeout.as_millis().min(mach_msg_timeout_t::MAX as u128) as mach_msg_timeout_t
}

/// A message laid out for `mach_msg` in eight-byte-aligned storage.
struct EncodedSurfaceShareMachMessage {
    words: Vec<u64>,
    byte_len: usize,
}

impl EncodedSurfaceShareMachMessage {
    fn header_mut(&mut self) -> *mut mach_msg_header_t {
        self.words.as_mut_ptr().cast()
    }
}

fn encode_surface_share_mach_message(
    destination: mach_port_name_t,
    reply_port: Option<mach_port_name_t>,
    message_id: mach_msg_id_t,
    json_payload: &[u8],
    port_names: &[mach_port_name_t],
) -> EncodedSurfaceShareMachMessage {
    let descriptors_start = SURFACE_SHARE_MACH_MESSAGE_PREFIX_BYTES;
    let json_length_start = descriptors_start + port_names.len() * PORT_DESCRIPTOR_BYTES;
    let json_start = json_length_start + JSON_LENGTH_PREFIX_BYTES;
    let byte_len = (json_start + json_payload.len()).next_multiple_of(4);
    let mut words = vec![0u64; byte_len.div_ceil(8)];
    let bytes = words.as_mut_ptr().cast::<u8>();

    let local_disposition = if reply_port.is_some() {
        MACH_MSG_TYPE_MAKE_SEND
    } else {
        0
    };
    let prefix = SurfaceShareMachMessagePrefix {
        header: mach_msg_header_t {
            msgh_bits: MACH_MSGH_BITS(MACH_MSG_TYPE_COPY_SEND, local_disposition)
                | MACH_MSGH_BITS_COMPLEX,
            msgh_size: byte_len as u32,
            msgh_remote_port: destination,
            msgh_local_port: reply_port.unwrap_or(MACH_PORT_NULL),
            msgh_voucher_port: MACH_PORT_NULL,
            msgh_id: message_id,
        },
        body: mach_msg_body_t {
            msgh_descriptor_count: port_names.len() as u32,
        },
    };
    // SAFETY: every write lands inside `words`, which spans `byte_len` bytes.
    unsafe {
        bytes.cast::<SurfaceShareMachMessagePrefix>().write(prefix);
        for (index, port_name) in port_names.iter().enumerate() {
            bytes
                .add(descriptors_start + index * PORT_DESCRIPTOR_BYTES)
                .cast::<mach_msg_port_descriptor_t>()
                .write_unaligned(mach_msg_port_descriptor_t::new(
                    *port_name,
                    MACH_MSG_TYPE_MOVE_SEND,
                ));
        }
        bytes
            .add(json_length_start)
            .cast::<u32>()
            .write_unaligned(json_payload.len() as u32);
        std::ptr::copy_nonoverlapping(
            json_payload.as_ptr(),
            bytes.add(json_start),
            json_payload.len(),
        );
    }
    EncodedSurfaceShareMachMessage { words, byte_len }
}

/// Storage a receive lands in, sized for the largest message the wire allows.
pub struct SurfaceShareMachMessageReceiveBuffer {
    words: Vec<u64>,
}

impl SurfaceShareMachMessageReceiveBuffer {
    /// A buffer for one receive at a time.
    pub fn new() -> Self {
        Self {
            words: vec![0u64; LARGEST_RECEIVED_SURFACE_SHARE_MACH_MESSAGE_BYTES.div_ceil(8)],
        }
    }

    fn byte_capacity(&self) -> usize {
        self.words.len() * 8
    }
}

impl Default for SurfaceShareMachMessageReceiveBuffer {
    fn default() -> Self {
        Self::new()
    }
}

/// One surface-share message, its rights adopted and its sender identified.
#[derive(Debug)]
pub struct ReceivedSurfaceShareMachMessage {
    /// Which of the surface-share `msgh_id`s it carries.
    pub message_id: mach_msg_id_t,
    /// The receive right (never a port set) it arrived on.
    pub received_on_port: mach_port_name_t,
    /// The send right the sender asked to be answered on.
    pub reply_send_right: Option<OwnedMachSendRight>,
    /// The rights it carried, in order.
    pub ports: Vec<OwnedMachSendRight>,
    /// Its JSON.
    pub json_payload: Vec<u8>,
    /// The sending process, per the kernel.
    pub sender: SurfaceShareMachSenderAuditIdentity,
}

/// What one receive produced.
#[derive(Debug)]
pub enum ReceivedSurfaceShareMachTraffic {
    /// A surface-share message.
    Message(ReceivedSurfaceShareMachMessage),
    /// The receiver behind a watched send right went away. Carries the extra
    /// reference the notification added to the dead name.
    DeadName {
        /// The name of the right that died, in this task.
        dead_name: OwnedMachSendRight,
    },
    /// No send right to a watched receive right remains.
    NoSenders {
        /// The receive right that lost its last sender — the port the
        /// notification arrived on, which is where
        /// [`OwnedMachReceiveRight::request_no_senders_notification`] has it
        /// delivered.
        port_without_senders: mach_port_name_t,
    },
    /// A kernel notification this protocol has no use for.
    OtherKernelNotification {
        /// Its `msgh_id`.
        message_id: mach_msg_id_t,
    },
}

/// Wait for the next message on `receive_name` — a receive right or a port
/// set — for up to `timeout`, or indefinitely for `None`.
///
/// A timeout is [`io::ErrorKind::TimedOut`]. A message that is not
/// well-formed for this wire is destroyed, its rights released, and reported
/// as [`io::ErrorKind::InvalidData`]; so is one too large to receive, which
/// the kernel discards.
pub fn receive_surface_share_mach_traffic(
    receive_name: mach_port_name_t,
    receive_buffer: &mut SurfaceShareMachMessageReceiveBuffer,
    timeout: Option<Duration>,
) -> io::Result<ReceivedSurfaceShareMachTraffic> {
    let mut options = MACH_RCV_MSG | RECEIVE_THE_AUDIT_TRAILER;
    let mut timeout_milliseconds: mach_msg_timeout_t = 0;
    if let Some(timeout) = timeout {
        options |= MACH_RCV_TIMEOUT;
        timeout_milliseconds = mach_timeout_milliseconds(timeout);
    }
    let receive_capacity = receive_buffer.byte_capacity() as u32;
    let header_pointer: *mut mach_msg_header_t = receive_buffer.words.as_mut_ptr().cast();
    // SAFETY: the buffer spans `receive_capacity` writable bytes.
    let receive_result = unsafe {
        mach_msg(
            header_pointer,
            options,
            0,
            receive_capacity,
            receive_name,
            timeout_milliseconds,
            MACH_PORT_NULL,
        )
    };
    match receive_result {
        KERN_SUCCESS => {}
        MACH_RCV_TIMED_OUT => {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "no surface-share message arrived before the timeout",
            ));
        }
        MACH_RCV_TOO_LARGE => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "a surface-share message larger than the wire allows was discarded",
            ));
        }
        _ => return Err(mach_failure("mach_msg(RECEIVE)", receive_result)),
    }

    // SAFETY: a successful receive wrote a header, a body of `msgh_size`
    // bytes and the audit trailer requested above.
    unsafe {
        adopt_received_surface_share_mach_traffic(header_pointer, receive_buffer.byte_capacity())
    }
}

/// Parse a received message in place, adopting its rights or destroying it.
///
/// # Safety
/// `header_pointer` points at a message a receive just wrote, inside a
/// buffer of `buffer_capacity` bytes, followed by its audit trailer.
unsafe fn adopt_received_surface_share_mach_traffic(
    header_pointer: *mut mach_msg_header_t,
    buffer_capacity: usize,
) -> io::Result<ReceivedSurfaceShareMachTraffic> {
    let bytes = header_pointer.cast::<u8>();
    // SAFETY: per this function's contract.
    let header = unsafe { header_pointer.read() };
    let message_bytes = header.msgh_size as usize;
    let malformed = |reason: String| {
        // SAFETY: destroys the rights of the message just received.
        unsafe { mach_msg_destroy(header_pointer) };
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("refused a malformed surface-share message: {reason}"),
        ))
    };
    if message_bytes < std::mem::size_of::<mach_msg_header_t>()
        || message_bytes + AUDIT_TRAILER_BYTES > buffer_capacity
    {
        return malformed(format!("its size {message_bytes} is out of range"));
    }
    // SAFETY: the audit trailer follows the message, inside the buffer.
    let trailer = unsafe {
        bytes
            .add(message_bytes)
            .cast::<mach_msg_audit_trailer_t>()
            .read_unaligned()
    };
    if (trailer.msgh_trailer_size as usize) < AUDIT_TRAILER_BYTES {
        return malformed("the kernel appended no audit trailer".to_string());
    }
    // SAFETY: plain accessors over a copied token.
    let sender = unsafe {
        SurfaceShareMachSenderAuditIdentity {
            pid: audit_token_to_pid(trailer.msgh_audit),
            pidversion: audit_token_to_pidversion(trailer.msgh_audit),
        }
    };

    let carries_rights = header.msgh_bits & MACH_MSGH_BITS_COMPLEX != 0;
    if !carries_rights && (MACH_NOTIFY_FIRST..=MACH_NOTIFY_LAST).contains(&header.msgh_id) {
        // Only the kernel sends notifications, and its audit pid is zero.
        if sender.pid != 0 {
            return malformed(format!(
                "a notification-shaped message came from pid {}",
                sender.pid
            ));
        }
        return match header.msgh_id {
            MACH_NOTIFY_DEAD_NAME
                if message_bytes
                    >= std::mem::offset_of!(mach_dead_name_notification_t, trailer) =>
            {
                // SAFETY: the message is a dead-name notification of that size.
                let notification = unsafe {
                    header_pointer
                        .cast::<mach_dead_name_notification_t>()
                        .read_unaligned()
                };
                Ok(ReceivedSurfaceShareMachTraffic::DeadName {
                    dead_name: OwnedMachSendRight {
                        name: notification.not_port,
                    },
                })
            }
            MACH_NOTIFY_NO_SENDERS => Ok(ReceivedSurfaceShareMachTraffic::NoSenders {
                port_without_senders: header.msgh_local_port,
            }),
            other_notification => {
                // SAFETY: releases whatever the notification carried.
                unsafe { mach_msg_destroy(header_pointer) };
                Ok(ReceivedSurfaceShareMachTraffic::OtherKernelNotification {
                    message_id: other_notification,
                })
            }
        };
    }

    if !carries_rights {
        return malformed("it is not a complex message".to_string());
    }
    if message_bytes < SURFACE_SHARE_MACH_MESSAGE_PREFIX_BYTES {
        return malformed("it is shorter than its prefix".to_string());
    }
    // SAFETY: the prefix lies inside the message.
    let prefix = unsafe {
        bytes
            .cast::<SurfaceShareMachMessagePrefix>()
            .read_unaligned()
    };
    let port_count = prefix.body.msgh_descriptor_count as usize;
    if port_count > MAX_SURFACE_SHARE_MACH_MESSAGE_PORTS {
        return malformed(format!("it carries {port_count} descriptors"));
    }
    let json_length_start =
        SURFACE_SHARE_MACH_MESSAGE_PREFIX_BYTES + port_count * PORT_DESCRIPTOR_BYTES;
    let json_start = json_length_start + JSON_LENGTH_PREFIX_BYTES;
    if json_start > message_bytes {
        return malformed("its descriptors overrun it".to_string());
    }
    let mut port_names = Vec::with_capacity(port_count);
    for descriptor_index in 0..port_count {
        // SAFETY: the descriptor lies inside the message.
        let descriptor = unsafe {
            bytes
                .add(
                    SURFACE_SHARE_MACH_MESSAGE_PREFIX_BYTES
                        + descriptor_index * PORT_DESCRIPTOR_BYTES,
                )
                .cast::<mach_msg_port_descriptor_t>()
                .read_unaligned()
        };
        if u32::from(descriptor.type_) != MACH_MSG_PORT_DESCRIPTOR
            || u32::from(descriptor.disposition) != MACH_MSG_TYPE_PORT_SEND
        {
            return malformed(format!(
                "descriptor {descriptor_index} is not a port send right"
            ));
        }
        port_names.push(descriptor.name);
    }
    // SAFETY: the length prefix lies inside the message.
    let json_byte_len =
        unsafe { bytes.add(json_length_start).cast::<u32>().read_unaligned() } as usize;
    if json_byte_len > MAX_SURFACE_SHARE_MACH_MESSAGE_JSON_BYTES
        || json_start + json_byte_len > message_bytes
    {
        return malformed(format!("its JSON length {json_byte_len} overruns it"));
    }
    let reply_disposition = header.msgh_bits & MACH_MSGH_BITS_REMOTE_MASK;
    let reply_send_right = match header.msgh_remote_port {
        MACH_PORT_NULL => None,
        reply_name if reply_disposition == MACH_MSG_TYPE_PORT_SEND => Some(reply_name),
        _ => return malformed("its reply right is not a send right".to_string()),
    };

    // SAFETY: the JSON lies inside the message.
    let json_payload =
        unsafe { std::slice::from_raw_parts(bytes.add(json_start), json_byte_len) }.to_vec();
    Ok(ReceivedSurfaceShareMachTraffic::Message(
        ReceivedSurfaceShareMachMessage {
            message_id: header.msgh_id,
            received_on_port: header.msgh_local_port,
            reply_send_right: reply_send_right.map(|name| OwnedMachSendRight { name }),
            ports: port_names
                .into_iter()
                .map(|name| OwnedMachSendRight { name })
                .collect(),
            json_payload,
            sender,
        },
    ))
}

/// A client's open connection to a surface-share Mach service: requests go
/// out on a port the service minted for this connection alone, and answers
/// come back on this connection's own reply port.
///
/// The service validates every message by its audit token, so a connection
/// only opens for a process the service admits.
pub struct SurfaceShareMachServiceConnection {
    request_send_right: OwnedMachSendRight,
    reply_receive_right: OwnedMachReceiveRight,
    service_death_notification_receive_right: OwnedMachReceiveRight,
    reply_receive_buffer_one_request_at_a_time: Mutex<SurfaceShareMachMessageReceiveBuffer>,
    service_went_away: AtomicBool,
}

impl std::fmt::Debug for SurfaceShareMachServiceConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SurfaceShareMachServiceConnection")
            .field("request_port", &self.request_send_right.as_raw_name())
            .field("reply_port", &self.reply_receive_right.as_raw_name())
            .finish()
    }
}

impl SurfaceShareMachServiceConnection {
    /// Look `service_name` up in the bootstrap namespace and open a
    /// connection, waiting up to `handshake_timeout` for the service to admit
    /// this process.
    ///
    /// A refusal is [`io::ErrorKind::PermissionDenied`], naming the
    /// service's reason.
    pub fn connect(service_name: &str, handshake_timeout: Duration) -> io::Result<Self> {
        let service_send_right = look_up_surface_share_mach_service(service_name)?;
        let reply_receive_right = OwnedMachReceiveRight::allocate()?;
        send_surface_share_mach_message(
            &service_send_right,
            Some(&reply_receive_right),
            SURFACE_SHARE_MACH_CONNECT_MESSAGE_ID,
            br#"{"op":"connect"}"#,
            Vec::new(),
            Some(handshake_timeout),
        )?;
        drop(service_send_right);

        let mut receive_buffer = SurfaceShareMachMessageReceiveBuffer::new();
        let connect_answer = match receive_surface_share_mach_traffic(
            reply_receive_right.name,
            &mut receive_buffer,
            Some(handshake_timeout),
        )? {
            ReceivedSurfaceShareMachTraffic::Message(message)
                if message.message_id == SURFACE_SHARE_MACH_REPLY_MESSAGE_ID =>
            {
                message
            }
            unexpected => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "the surface-share service '{service_name}' answered a connect with \
                         {unexpected:?}"
                    ),
                ));
            }
        };
        let answer: serde_json::Value = serde_json::from_slice(&connect_answer.json_payload)
            .map_err(|parse_failure| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("the connect answer is not JSON: {parse_failure}"),
                )
            })?;
        if let Some(refusal) = answer.get("error").and_then(serde_json::Value::as_str) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "the surface-share service '{service_name}' refused this process: {refusal}"
                ),
            ));
        }
        let mut ports = connect_answer.ports.into_iter();
        let (Some(request_send_right), None) = (ports.next(), ports.next()) else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "the connect answer did not carry exactly one request port",
            ));
        };

        let service_death_notification_receive_right = OwnedMachReceiveRight::allocate()?;
        request_dead_name_notification(
            &request_send_right,
            &service_death_notification_receive_right,
        )?;
        // Delivered to the reply port itself, so a request blocked on its
        // answer wakes when the service is gone rather than waiting forever.
        reply_receive_right.request_no_senders_notification()?;

        Ok(Self {
            request_send_right,
            reply_receive_right,
            service_death_notification_receive_right,
            reply_receive_buffer_one_request_at_a_time: Mutex::new(receive_buffer),
            service_went_away: AtomicBool::new(false),
        })
    }

    /// Send one request, moving `ports` with it, and wait for the service's
    /// answer and the rights it carries.
    ///
    /// A service that went away is [`io::ErrorKind::BrokenPipe`].
    pub fn send_request_with_ports(
        &self,
        request: &serde_json::Value,
        ports: Vec<OwnedMachSendRight>,
    ) -> io::Result<(serde_json::Value, Vec<OwnedMachSendRight>)> {
        let request_bytes = serde_json::to_vec(request).map_err(|serialize_failure| {
            io::Error::other(format!("failed to serialise request: {serialize_failure}"))
        })?;
        let mut receive_buffer = self
            .reply_receive_buffer_one_request_at_a_time
            .lock()
            .map_err(|_| io::Error::other("the connection's request lock is poisoned"))?;
        if self.service_went_away.load(Ordering::Acquire) {
            return Err(the_service_went_away());
        }
        send_surface_share_mach_message(
            &self.request_send_right,
            None,
            SURFACE_SHARE_MACH_REQUEST_MESSAGE_ID,
            &request_bytes,
            ports,
            None,
        )?;
        loop {
            match receive_surface_share_mach_traffic(
                self.reply_receive_right.name,
                &mut receive_buffer,
                None,
            )? {
                ReceivedSurfaceShareMachTraffic::Message(answer)
                    if answer.message_id == SURFACE_SHARE_MACH_REPLY_MESSAGE_ID =>
                {
                    let response =
                        serde_json::from_slice(&answer.json_payload).map_err(|parse_failure| {
                            io::Error::new(
                                io::ErrorKind::InvalidData,
                                format!("the service's answer is not JSON: {parse_failure}"),
                            )
                        })?;
                    return Ok((response, answer.ports));
                }
                ReceivedSurfaceShareMachTraffic::NoSenders { .. } => {
                    self.service_went_away.store(true, Ordering::Release);
                    return Err(the_service_went_away());
                }
                // Nothing else is sent to a reply port; drop it and keep
                // waiting for the answer.
                _ => {}
            }
        }
    }

    /// Wait up to `timeout` (forever for `None`) for the service to go away —
    /// its process dying included. `true` once it has.
    ///
    /// A helper wires this to its teardown: an IOSurface it still holds stays
    /// readable after the engine dies, so nothing else releases it.
    pub fn wait_for_the_service_to_go_away(&self, timeout: Option<Duration>) -> io::Result<bool> {
        if self.service_went_away.load(Ordering::Acquire) {
            return Ok(true);
        }
        let mut receive_buffer = SurfaceShareMachMessageReceiveBuffer::new();
        loop {
            match receive_surface_share_mach_traffic(
                self.service_death_notification_receive_right.name,
                &mut receive_buffer,
                timeout,
            ) {
                Ok(ReceivedSurfaceShareMachTraffic::DeadName { .. }) => {
                    self.service_went_away.store(true, Ordering::Release);
                    return Ok(true);
                }
                Ok(_) => {}
                Err(timed_out) if timed_out.kind() == io::ErrorKind::TimedOut => return Ok(false),
                Err(receive_failure) => return Err(receive_failure),
            }
        }
    }
}

fn the_service_went_away() -> io::Error {
    io::Error::new(
        io::ErrorKind::BrokenPipe,
        "the surface-share service went away",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wire_structs_keep_the_kernels_layout() {
        assert_eq!(std::mem::size_of::<mach_msg_header_t>(), 24);
        assert_eq!(std::mem::size_of::<mach_msg_body_t>(), 4);
        assert_eq!(SURFACE_SHARE_MACH_MESSAGE_PREFIX_BYTES, 28);
        assert_eq!(
            std::mem::offset_of!(SurfaceShareMachMessagePrefix, body),
            24
        );
        assert_eq!(PORT_DESCRIPTOR_BYTES, 12);
        assert_eq!(
            std::mem::offset_of!(mach_msg_port_descriptor_t, disposition),
            10
        );
        assert_eq!(std::mem::offset_of!(mach_msg_port_descriptor_t, type_), 11);
        assert_eq!(AUDIT_TRAILER_BYTES, 52);
        assert_eq!(
            std::mem::offset_of!(mach_msg_audit_trailer_t, msgh_audit),
            20
        );
        assert_eq!(
            std::mem::offset_of!(mach_dead_name_notification_t, not_port),
            32
        );
        assert_eq!(RECEIVE_THE_AUDIT_TRAILER, 0x0300_0000);
    }

    #[test]
    fn an_encoded_message_lays_out_ports_then_a_length_prefixed_json() {
        let json_payload = br#"{"op":"lookup"}"#;
        let encoded =
            encode_surface_share_mach_message(7, Some(9), 0x1234, json_payload, &[11, 13]);
        let bytes = unsafe {
            std::slice::from_raw_parts(encoded.words.as_ptr().cast::<u8>(), encoded.byte_len)
        };
        assert_eq!(encoded.byte_len % 4, 0);
        assert_eq!(
            encoded.byte_len,
            (28 + 2 * 12 + 4 + json_payload.len()).next_multiple_of(4)
        );
        let prefix = unsafe {
            bytes
                .as_ptr()
                .cast::<SurfaceShareMachMessagePrefix>()
                .read()
        };
        assert_eq!(prefix.header.msgh_size as usize, encoded.byte_len);
        assert_eq!(prefix.header.msgh_remote_port, 7);
        assert_eq!(prefix.header.msgh_local_port, 9);
        assert_eq!(prefix.header.msgh_id, 0x1234);
        assert_eq!(
            prefix.header.msgh_bits,
            MACH_MSGH_BITS(MACH_MSG_TYPE_COPY_SEND, MACH_MSG_TYPE_MAKE_SEND)
                | MACH_MSGH_BITS_COMPLEX
        );
        assert_eq!(prefix.body.msgh_descriptor_count, 2);
        let second_descriptor = unsafe {
            bytes
                .as_ptr()
                .add(28 + 12)
                .cast::<mach_msg_port_descriptor_t>()
                .read_unaligned()
        };
        assert_eq!(second_descriptor.name, 13);
        assert_eq!(
            u32::from(second_descriptor.disposition),
            MACH_MSG_TYPE_MOVE_SEND
        );
        let json_byte_len =
            unsafe { bytes.as_ptr().add(28 + 24).cast::<u32>().read_unaligned() } as usize;
        assert_eq!(
            &bytes[28 + 24 + 4..28 + 24 + 4 + json_byte_len],
            json_payload
        );
    }

    unsafe extern "C" {
        fn mach_port_type(
            task: mach_port_name_t,
            name: mach_port_name_t,
            right_type: *mut mach2::port::mach_port_type_t,
        ) -> kern_return_t;
        fn mach_port_get_refs(
            task: mach_port_name_t,
            name: mach_port_name_t,
            right: mach2::port::mach_port_right_t,
            references: *mut mach2::port::mach_port_urefs_t,
        ) -> kern_return_t;
    }

    fn send_references_of(name: mach_port_name_t) -> mach2::port::mach_port_urefs_t {
        let mut send_references: mach2::port::mach_port_urefs_t = 0;
        unsafe {
            mach_port_get_refs(
                mach_task_self(),
                name,
                mach2::port::MACH_PORT_RIGHT_SEND,
                &mut send_references,
            )
        };
        send_references
    }

    fn port_is_alive(name: mach_port_name_t) -> bool {
        let mut right_type: mach2::port::mach_port_type_t = 0;
        unsafe { mach_port_type(mach_task_self(), name, &mut right_type) == KERN_SUCCESS }
    }

    #[test]
    fn a_message_with_ports_round_trips_in_process_with_the_senders_audit_identity() {
        let receive_right = OwnedMachReceiveRight::allocate().unwrap();
        let destination = receive_right.make_send_right().unwrap();
        let reply_receive_right = OwnedMachReceiveRight::allocate().unwrap();
        let carried_port = OwnedMachReceiveRight::allocate().unwrap();
        let carried_send_right = carried_port.make_send_right().unwrap();

        send_surface_share_mach_message(
            &destination,
            Some(&reply_receive_right),
            SURFACE_SHARE_MACH_REQUEST_MESSAGE_ID,
            br#"{"op":"register"}"#,
            vec![carried_send_right],
            Some(Duration::from_secs(1)),
        )
        .unwrap();

        let mut receive_buffer = SurfaceShareMachMessageReceiveBuffer::new();
        let ReceivedSurfaceShareMachTraffic::Message(message) = receive_surface_share_mach_traffic(
            receive_right.as_raw_name(),
            &mut receive_buffer,
            Some(Duration::from_secs(1)),
        )
        .unwrap() else {
            panic!("expected a message");
        };
        assert_eq!(message.message_id, SURFACE_SHARE_MACH_REQUEST_MESSAGE_ID);
        assert_eq!(message.received_on_port, receive_right.as_raw_name());
        assert_eq!(message.json_payload, br#"{"op":"register"}"#);
        assert_eq!(message.sender.pid, std::process::id() as libc::pid_t);
        assert_eq!(message.ports.len(), 1);
        assert_eq!(message.ports[0].as_raw_name(), carried_port.as_raw_name());
        assert_eq!(
            message
                .reply_send_right
                .as_ref()
                .map(OwnedMachSendRight::as_raw_name),
            Some(reply_receive_right.as_raw_name())
        );
    }

    #[test]
    fn a_receive_with_nothing_queued_times_out() {
        let receive_right = OwnedMachReceiveRight::allocate().unwrap();
        let mut receive_buffer = SurfaceShareMachMessageReceiveBuffer::new();
        let timed_out = receive_surface_share_mach_traffic(
            receive_right.as_raw_name(),
            &mut receive_buffer,
            Some(Duration::from_millis(10)),
        )
        .unwrap_err();
        assert_eq!(timed_out.kind(), io::ErrorKind::TimedOut);
    }

    #[test]
    fn destroying_a_receive_right_notifies_the_dead_name_watcher() {
        let watched_receive_right = OwnedMachReceiveRight::allocate().unwrap();
        let watched_send_right = watched_receive_right.make_send_right().unwrap();
        let notification_port = OwnedMachReceiveRight::allocate().unwrap();
        request_dead_name_notification(&watched_send_right, &notification_port).unwrap();
        let watched_name = watched_send_right.as_raw_name();

        drop(watched_receive_right);

        let mut receive_buffer = SurfaceShareMachMessageReceiveBuffer::new();
        let traffic = receive_surface_share_mach_traffic(
            notification_port.as_raw_name(),
            &mut receive_buffer,
            Some(Duration::from_secs(1)),
        )
        .unwrap();
        let ReceivedSurfaceShareMachTraffic::DeadName { dead_name } = traffic else {
            panic!("expected a dead-name notification, got {traffic:?}");
        };
        assert_eq!(dead_name.as_raw_name(), watched_name);
        drop(dead_name);
        drop(watched_send_right);
        assert!(
            !port_is_alive(watched_name),
            "both dead-name references were released"
        );
    }

    #[test]
    fn a_failed_send_releases_the_ports_it_was_moving() {
        let dead_destination = {
            let receive_right = OwnedMachReceiveRight::allocate().unwrap();
            receive_right.make_send_right().unwrap()
        };
        let carried_port = OwnedMachReceiveRight::allocate().unwrap();
        let carried_send_right = carried_port.make_send_right().unwrap();

        let refused = send_surface_share_mach_message(
            &dead_destination,
            None,
            SURFACE_SHARE_MACH_REQUEST_MESSAGE_ID,
            b"{}",
            vec![carried_send_right],
            Some(Duration::ZERO),
        )
        .unwrap_err();

        assert_eq!(refused.kind(), io::ErrorKind::BrokenPipe);
        let mut send_references: mach2::port::mach_port_urefs_t = 0;
        unsafe {
            mach_port_get_refs(
                mach_task_self(),
                carried_port.as_raw_name(),
                mach2::port::MACH_PORT_RIGHT_SEND,
                &mut send_references,
            )
        };
        assert_eq!(
            send_references, 0,
            "the moved send right was released, not leaked"
        );
    }

    #[test]
    fn a_send_timing_out_on_a_full_queue_releases_its_reply_right_and_its_ports() {
        let full_receive_right = OwnedMachReceiveRight::allocate().unwrap();
        let mut limits = mach2::port::mach_port_limits_t { mpl_qlimit: 1 };
        let limited = unsafe {
            mach2::mach_port::mach_port_set_attributes(
                mach_task_self(),
                full_receive_right.as_raw_name(),
                mach2::port::MACH_PORT_LIMITS_INFO,
                (&mut limits as *mut mach2::port::mach_port_limits_t).cast(),
                mach2::port::MACH_PORT_LIMITS_INFO_COUNT,
            )
        };
        assert_eq!(limited, KERN_SUCCESS);
        let full_destination = full_receive_right.make_send_right().unwrap();
        send_surface_share_mach_message(
            &full_destination,
            None,
            SURFACE_SHARE_MACH_REQUEST_MESSAGE_ID,
            b"{}",
            Vec::new(),
            Some(Duration::ZERO),
        )
        .expect("the one message the queue holds");
        let reply_receive_right = OwnedMachReceiveRight::allocate().unwrap();
        let carried_port = OwnedMachReceiveRight::allocate().unwrap();

        let refused = send_surface_share_mach_message(
            &full_destination,
            Some(&reply_receive_right),
            SURFACE_SHARE_MACH_REQUEST_MESSAGE_ID,
            b"{}",
            vec![carried_port.make_send_right().unwrap()],
            Some(Duration::ZERO),
        )
        .unwrap_err();

        assert_eq!(refused.kind(), io::ErrorKind::TimedOut);
        assert_eq!(
            send_references_of(reply_receive_right.as_raw_name()),
            0,
            "the reply right the kernel minted was released"
        );
        assert_eq!(
            send_references_of(carried_port.as_raw_name()),
            0,
            "the moved send right was released"
        );
    }

    #[test]
    fn an_oversized_json_is_refused_before_anything_is_sent() {
        let receive_right = OwnedMachReceiveRight::allocate().unwrap();
        let destination = receive_right.make_send_right().unwrap();
        let refused = send_surface_share_mach_message(
            &destination,
            None,
            SURFACE_SHARE_MACH_REQUEST_MESSAGE_ID,
            &vec![b' '; MAX_SURFACE_SHARE_MACH_MESSAGE_JSON_BYTES + 1],
            Vec::new(),
            None,
        )
        .unwrap_err();
        assert_eq!(refused.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn a_service_name_past_the_bootstrap_limit_is_refused_by_name() {
        let refused = check_in_surface_share_mach_service(&"x".repeat(200)).unwrap_err();
        assert_eq!(refused.kind(), io::ErrorKind::InvalidInput);
        assert!(refused.to_string().contains("bootstrap names hold at most"));
    }

    #[test]
    fn a_live_service_name_is_refused_to_a_second_check_in() {
        let service_name = format!(
            "com.tatolab.streamlib.surface-client-test.{}",
            std::process::id()
        );
        let _first = check_in_surface_share_mach_service(&service_name).unwrap();
        let refused = check_in_surface_share_mach_service(&service_name).unwrap_err();
        assert_eq!(refused.kind(), io::ErrorKind::AddrInUse);
    }
}
