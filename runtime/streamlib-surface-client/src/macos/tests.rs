// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::time::Duration;

use mach2::message::{
    MACH_MSG_TYPE_COPY_SEND, MACH_MSG_TYPE_MOVE_SEND, MACH_MSGH_BITS, MACH_MSGH_BITS_COMPLEX,
};
use mach2::notify::mach_dead_name_notification_t;

use super::surface_share_mach_messages::encode_surface_share_mach_message;
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
    let encoded = encode_surface_share_mach_message(7, Some(9), 0x1234, json_payload, &[11, 13]);
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
        MACH_MSGH_BITS(MACH_MSG_TYPE_COPY_SEND, MACH_MSG_TYPE_MAKE_SEND) | MACH_MSGH_BITS_COMPLEX
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
