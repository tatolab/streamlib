// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Sending surface-share Mach messages, and receiving and adopting the
//! traffic that arrives on a port.

use std::io;
use std::time::Duration;

use mach2::kern_return::{KERN_SUCCESS, kern_return_t};
use mach2::mach_port::mach_port_deallocate;
use mach2::message::{
    MACH_MSG_PORT_DESCRIPTOR, MACH_MSG_TYPE_COPY_SEND, MACH_MSG_TYPE_MAKE_SEND,
    MACH_MSG_TYPE_MOVE_SEND, MACH_MSG_TYPE_PORT_SEND, MACH_MSGH_BITS, MACH_MSGH_BITS_COMPLEX,
    MACH_MSGH_BITS_LOCAL_MASK, MACH_MSGH_BITS_REMOTE_MASK, MACH_RCV_MSG, MACH_RCV_TIMED_OUT,
    MACH_RCV_TIMEOUT, MACH_RCV_TOO_LARGE, MACH_SEND_INTERRUPTED, MACH_SEND_INVALID_DEST,
    MACH_SEND_MSG, MACH_SEND_TIMED_OUT, MACH_SEND_TIMEOUT, audit_token_t, mach_msg,
    mach_msg_audit_trailer_t, mach_msg_body_t, mach_msg_destroy, mach_msg_header_t, mach_msg_id_t,
    mach_msg_port_descriptor_t, mach_msg_timeout_t,
};
use mach2::notify::{
    MACH_NOTIFY_DEAD_NAME, MACH_NOTIFY_FIRST, MACH_NOTIFY_LAST, MACH_NOTIFY_NO_SENDERS,
    mach_dead_name_notification_t,
};
use mach2::port::{MACH_PORT_NULL, mach_port_name_t};
use mach2::traps::mach_task_self;

use super::{
    AUDIT_TRAILER_BYTES, JSON_LENGTH_PREFIX_BYTES,
    LARGEST_RECEIVED_SURFACE_SHARE_MACH_MESSAGE_BYTES, MAX_SURFACE_SHARE_MACH_MESSAGE_JSON_BYTES,
    MAX_SURFACE_SHARE_MACH_MESSAGE_PORTS, OwnedMachReceiveRight, OwnedMachSendRight,
    PORT_DESCRIPTOR_BYTES, RECEIVE_THE_AUDIT_TRAILER, SURFACE_SHARE_MACH_MESSAGE_PREFIX_BYTES,
    SurfaceShareMachMessagePrefix, mach_failure,
};

#[link(name = "bsm")]
unsafe extern "C" {
    fn audit_token_to_pid(audit_token: audit_token_t) -> libc::pid_t;
    fn audit_token_to_pidversion(audit_token: audit_token_t) -> libc::c_int;
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
        // Any other failure is a malformed message, or one refused for lack
        // of kernel resources, and the kernel may have copied part of it in
        // before refusing, consuming those rights. Releasing `ports` again
        // could free a right some other holder now owns under the same name,
        // so they are left unreleased instead.
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
pub(super) struct EncodedSurfaceShareMachMessage {
    pub(super) words: Vec<u64>,
    pub(super) byte_len: usize,
}

impl EncodedSurfaceShareMachMessage {
    fn header_mut(&mut self) -> *mut mach_msg_header_t {
        self.words.as_mut_ptr().cast()
    }
}

pub(super) fn encode_surface_share_mach_message(
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
