// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! An encoder node's session in the machine's encode host: leased through
//! the module host table, opened over the encode host protocol, and opened
//! again when the host ends and the runtime starts it again.

use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use tatolab_module_sdk::{ModuleHost, ModuleHostSharedNativeServiceLease};

use crate::VideoCodecNodeIdentity;
use crate::video_encode_host_protocol::{
    VideoEncodeHostReply, VideoEncodeHostRequest, VideoEncodeHostSessionRequest,
    receive_video_encode_host_message, send_video_encode_host_message,
};
use crate::video_encode_on_a_device::{VideoFrameEncodeOutcome, VideoFrameToEncode};

pub use streamlib_consumer_rhi::runtime_unit_layout::VIDEO_ENCODE_HOST_SHARED_NATIVE_SERVICE_NAME;

/// How long an answer may take before the host is taken for lost: an encode
/// takes milliseconds, and a session's first frame mints its session.
const LONGEST_ENCODE_HOST_ANSWER: Duration = Duration::from_secs(10);

/// How long after a lost connection the session is opened again.
const ENCODE_HOST_REOPEN_INTERVAL: Duration = Duration::from_secs(1);

/// One node's session in the encode host, and the lease that keeps the host
/// running.
pub struct VideoEncodeHostSessionOfTheNode {
    node: VideoCodecNodeIdentity,
    lease: ModuleHostSharedNativeServiceLease,
    session_request: VideoEncodeHostSessionRequest,
    connection: Option<UnixStream>,
    last_opening: Instant,
}

impl VideoEncodeHostSessionOfTheNode {
    /// Lease the encode host and open `session_request` in it.
    pub fn open(
        node: VideoCodecNodeIdentity,
        module_host: &ModuleHost,
        session_request: VideoEncodeHostSessionRequest,
    ) -> Result<Self, String> {
        let node_type_name = node.node_type_name;
        let lease = module_host
            .lease_shared_native_service(VIDEO_ENCODE_HOST_SHARED_NATIVE_SERVICE_NAME)
            .map_err(|refusal| {
                format!("{node_type_name}: the encode host was not leased: {refusal}")
            })?;
        let connection = open_the_session_in_the_encode_host(node, &lease, &session_request)?;
        Ok(Self {
            node,
            lease,
            session_request,
            connection: Some(connection),
            last_opening: Instant::now(),
        })
    }

    /// Encode `frame` in the host, opening the session again first when the
    /// connection was lost; a frame arriving before the reopen interval has
    /// passed is refused.
    pub fn encode(
        &mut self,
        frame: &VideoFrameToEncode,
        as_a_sync_point: bool,
    ) -> Result<VideoFrameEncodeOutcome, String> {
        let node_type_name = self.node.node_type_name;
        let (connection, as_a_sync_point) = match self.connection.take() {
            Some(connection) => (connection, as_a_sync_point),
            None => {
                if self.last_opening.elapsed() < ENCODE_HOST_REOPEN_INTERVAL {
                    return Err(format!(
                        "{node_type_name}: the encode host's connection was lost, so the frame \
                         was dropped"
                    ));
                }
                self.last_opening = Instant::now();
                let connection = open_the_session_in_the_encode_host(
                    self.node,
                    &self.lease,
                    &self.session_request,
                )?;
                tracing::info!("{node_type_name}: the encode host session was opened again");
                (connection, true)
            }
        };
        let mut connection = connection;
        let request = VideoEncodeHostRequest::EncodeFrame {
            frame: frame.into(),
            as_a_sync_point,
        };
        let reply = send_video_encode_host_message(&mut connection, &request).and_then(|()| {
            receive_video_encode_host_message::<VideoEncodeHostReply>(&mut connection)
        });
        match reply {
            Ok(Some(reply)) => {
                self.connection = Some(connection);
                reply.into_the_encode()
            }
            Ok(None) => Err(format!(
                "{node_type_name}: the encode host closed the session's connection"
            )),
            Err(failure) => Err(format!(
                "{node_type_name}: the encode host's connection was lost: {failure}"
            )),
        }
    }
}

fn open_the_session_in_the_encode_host(
    node: VideoCodecNodeIdentity,
    lease: &ModuleHostSharedNativeServiceLease,
    session_request: &VideoEncodeHostSessionRequest,
) -> Result<UnixStream, String> {
    let node_type_name = node.node_type_name;
    let mut connection = UnixStream::connect(lease.endpoint_socket_path()).map_err(|failure| {
        format!(
            "{node_type_name}: the encode host at {} refused a connection: {failure}",
            lease.endpoint_socket_path().display()
        )
    })?;
    connection
        .set_read_timeout(Some(LONGEST_ENCODE_HOST_ANSWER))
        .and_then(|()| connection.set_write_timeout(Some(LONGEST_ENCODE_HOST_ANSWER)))
        .map_err(|failure| {
            format!("{node_type_name}: the encode host's connection took no timeout: {failure}")
        })?;
    send_video_encode_host_message(
        &mut connection,
        &VideoEncodeHostRequest::OpenSession(session_request.clone()),
    )
    .map_err(|failure| {
        format!("{node_type_name}: the encode host session could not be asked for: {failure}")
    })?;
    match receive_video_encode_host_message::<VideoEncodeHostReply>(&mut connection) {
        Ok(Some(VideoEncodeHostReply::SessionOpened { device_name })) => {
            tracing::info!(
                device = device_name,
                "{node_type_name}: encoding on the encode host's shared device"
            );
            Ok(connection)
        }
        Ok(Some(VideoEncodeHostReply::Refused { refusal })) => Err(format!(
            "{node_type_name}: the encode host refused the session: {refusal}"
        )),
        Ok(Some(_)) => Err(format!(
            "{node_type_name}: the encode host answered the session's opening with something \
             else"
        )),
        Ok(None) => Err(format!(
            "{node_type_name}: the encode host closed the connection before the session opened"
        )),
        Err(failure) => Err(format!(
            "{node_type_name}: the encode host session's opening was not answered: {failure}"
        )),
    }
}
