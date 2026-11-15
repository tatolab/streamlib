// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A client's connection to the surface-share Mach service.

use std::io;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use super::surface_share_mach_messages::{
    ReceivedSurfaceShareMachTraffic, SurfaceShareMachMessageReceiveBuffer,
    receive_surface_share_mach_traffic, send_surface_share_mach_message,
};
use super::{
    OwnedMachReceiveRight, OwnedMachSendRight, SURFACE_SHARE_MACH_CONNECT_MESSAGE_ID,
    SURFACE_SHARE_MACH_REPLY_MESSAGE_ID, SURFACE_SHARE_MACH_REQUEST_MESSAGE_ID,
    look_up_surface_share_mach_service, request_dead_name_notification,
};

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
        self.send_request_with_ports_answered_within(request, ports, None)
    }

    /// [`Self::send_request_with_ports`], giving up with
    /// [`io::ErrorKind::TimedOut`] once `response_timeout` passes without the
    /// request sent and answered (never, for `None`). An answer that arrives
    /// after the timeout waits on the reply port, so a connection that timed
    /// out is never asked again.
    pub fn send_request_with_ports_answered_within(
        &self,
        request: &serde_json::Value,
        ports: Vec<OwnedMachSendRight>,
        response_timeout: Option<Duration>,
    ) -> io::Result<(serde_json::Value, Vec<OwnedMachSendRight>)> {
        let answered_by = response_timeout.map(|timeout| std::time::Instant::now() + timeout);
        let time_left_to_answer = || {
            answered_by
                .map(|deadline| deadline.saturating_duration_since(std::time::Instant::now()))
        };
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
            time_left_to_answer(),
        )?;
        loop {
            match receive_surface_share_mach_traffic(
                self.reply_receive_right.name,
                &mut receive_buffer,
                time_left_to_answer(),
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
