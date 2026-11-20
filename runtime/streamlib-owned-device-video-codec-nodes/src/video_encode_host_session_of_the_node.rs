// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! An encoder node's session in the machine's encode host: leased through
//! the module host table, opened over the encode host protocol, and opened
//! again when the host ends and the runtime starts it again.

use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use tatolab_module_sdk::{ModuleHost, ModuleHostSharedNativeServiceLease};

use crate::VideoCodecNodeIdentity;
use crate::video_encode_host_protocol::{
    VideoEncodeHostConnection, VideoEncodeHostReply, VideoEncodeHostRequest,
    VideoEncodeHostSessionRequest,
};
use crate::video_encode_on_a_device::{VideoFrameEncodeOutcome, VideoFrameToEncode};

pub use streamlib_consumer_rhi::runtime_unit_layout::VIDEO_ENCODE_HOST_SHARED_NATIVE_SERVICE_NAME;

/// How long an answer may take before the host is taken for lost: an encode
/// takes milliseconds, and a session's first frame mints its session.
const LONGEST_ENCODE_HOST_ANSWER: Duration = Duration::from_secs(10);

/// How long after the last opening a lost session is opened again.
pub(crate) const ENCODE_HOST_REOPEN_INTERVAL: Duration = Duration::from_secs(1);

/// One node's session in the encode host, and the lease that keeps the host
/// running.
pub struct VideoEncodeHostSessionOfTheNode {
    session: VideoEncodeHostSession,
    _lease: ModuleHostSharedNativeServiceLease,
}

impl VideoEncodeHostSessionOfTheNode {
    /// Lease the encode host and open `session_request` in it.
    pub fn open(
        node: VideoCodecNodeIdentity,
        module_host: &ModuleHost,
        session_request: VideoEncodeHostSessionRequest,
    ) -> Result<Self, String> {
        let lease = module_host
            .lease_shared_native_service(VIDEO_ENCODE_HOST_SHARED_NATIVE_SERVICE_NAME)
            .map_err(|refusal| {
                format!(
                    "{}: the encode host was not leased: {refusal}",
                    node.node_type_name
                )
            })?;
        let session =
            VideoEncodeHostSession::open(node, lease.endpoint_socket_path(), session_request)?;
        Ok(Self {
            session,
            _lease: lease,
        })
    }

    /// Encode `frame` in the host; see [`VideoEncodeHostSession::encode`].
    pub fn encode(
        &mut self,
        frame: VideoFrameToEncode,
        as_a_sync_point: bool,
    ) -> Result<VideoFrameEncodeOutcome, String> {
        self.session.encode(frame, as_a_sync_point)
    }
}

/// One session in the encode host at an endpoint socket, opened again after
/// its connection is lost.
pub struct VideoEncodeHostSession {
    node: VideoCodecNodeIdentity,
    endpoint_socket_path: PathBuf,
    session_request: VideoEncodeHostSessionRequest,
    connection: Option<VideoEncodeHostConnection<UnixStream>>,
    last_opening: Instant,
}

impl VideoEncodeHostSession {
    /// Open `session_request` in the encode host listening on
    /// `endpoint_socket_path`.
    pub fn open(
        node: VideoCodecNodeIdentity,
        endpoint_socket_path: &Path,
        session_request: VideoEncodeHostSessionRequest,
    ) -> Result<Self, String> {
        let connection =
            open_the_session_in_the_encode_host(node, endpoint_socket_path, &session_request)?;
        Ok(Self {
            node,
            endpoint_socket_path: endpoint_socket_path.to_path_buf(),
            session_request,
            connection: Some(connection),
            last_opening: Instant::now(),
        })
    }

    /// Encode `frame` in the host. A lost connection is opened again once
    /// the reopen interval since the last opening has passed, and the frame
    /// it carries first is a sync point; a frame before that is refused.
    pub fn encode(
        &mut self,
        frame: VideoFrameToEncode,
        as_a_sync_point: bool,
    ) -> Result<VideoFrameEncodeOutcome, String> {
        let node_type_name = self.node.node_type_name;
        let (mut connection, as_a_sync_point) = match self.connection.take() {
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
                    &self.endpoint_socket_path,
                    &self.session_request,
                )?;
                tracing::info!("{node_type_name}: the encode host session was opened again");
                (connection, true)
            }
        };
        let request = VideoEncodeHostRequest::EncodeFrame {
            frame,
            as_a_sync_point,
        };
        let reply = connection
            .send(&request)
            .and_then(|()| connection.receive::<VideoEncodeHostReply>());
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
    endpoint_socket_path: &Path,
    session_request: &VideoEncodeHostSessionRequest,
) -> Result<VideoEncodeHostConnection<UnixStream>, String> {
    let node_type_name = node.node_type_name;
    let socket = UnixStream::connect(endpoint_socket_path).map_err(|failure| {
        format!(
            "{node_type_name}: the encode host at {} refused a connection: {failure}",
            endpoint_socket_path.display()
        )
    })?;
    socket
        .set_read_timeout(Some(LONGEST_ENCODE_HOST_ANSWER))
        .and_then(|()| socket.set_write_timeout(Some(LONGEST_ENCODE_HOST_ANSWER)))
        .map_err(|failure| {
            format!("{node_type_name}: the encode host's connection took no timeout: {failure}")
        })?;
    let mut connection = VideoEncodeHostConnection::over(socket);
    connection
        .send(&VideoEncodeHostRequest::OpenSession(
            session_request.clone(),
        ))
        .map_err(|failure| {
            format!("{node_type_name}: the encode host session could not be asked for: {failure}")
        })?;
    match connection.receive::<VideoEncodeHostReply>() {
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
        Ok(Some(VideoEncodeHostReply::Encode(_))) => Err(format!(
            "{node_type_name}: the encode host answered the session's opening with an encode"
        )),
        Ok(None) => Err(format!(
            "{node_type_name}: the encode host closed the connection before the session opened"
        )),
        Err(failure) => Err(format!(
            "{node_type_name}: the encode host session's opening was not answered: {failure}"
        )),
    }
}
