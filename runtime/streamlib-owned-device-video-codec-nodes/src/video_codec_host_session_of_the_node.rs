// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A video codec node's session in the machine's video codec host: leased
//! through the module host table, opened over the video codec host protocol,
//! and opened again when the host ends and the runtime starts it again.

use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use tatolab_module_sdk::{ModuleHost, ModuleHostSharedNativeServiceLease};

use crate::VideoCodecNodeIdentity;
use crate::video_codec_host_protocol::{
    VideoCodecHostConnection, VideoCodecHostReply, VideoCodecHostRequest,
    VideoDecodeSessionRequest, VideoEncodeSessionRequest,
};
use crate::video_codec_retry_gate::VideoCodecRefusal;

pub use streamlib_consumer_rhi::runtime_unit_layout::VIDEO_CODEC_HOST_SHARED_NATIVE_SERVICE_NAME;

/// How long an answer may take before the host is taken for lost: a coding
/// takes milliseconds, and a session's first frame or access unit mints its
/// coding session.
const LONGEST_VIDEO_CODEC_HOST_ANSWER: Duration = Duration::from_secs(10);

/// How long after the last opening a lost session is opened again.
pub(crate) const VIDEO_CODEC_HOST_REOPEN_INTERVAL: Duration = Duration::from_secs(1);

/// The session a node opens in the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VideoCodecHostSessionOpening {
    Encode(VideoEncodeSessionRequest),
    Decode(VideoDecodeSessionRequest),
}

impl VideoCodecHostSessionOpening {
    fn request(&self) -> VideoCodecHostRequest {
        match self {
            Self::Encode(session_request) => {
                VideoCodecHostRequest::OpenEncodeSession(session_request.clone())
            }
            Self::Decode(session_request) => {
                VideoCodecHostRequest::OpenDecodeSession(session_request.clone())
            }
        }
    }

    fn coding(&self) -> &'static str {
        match self {
            Self::Encode(_) => "encoding",
            Self::Decode(_) => "decoding",
        }
    }
}

/// Whether the session's connection is the one its last request went over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoCodecHostSessionConnection {
    /// The host still holds every state the session built.
    Kept,
    /// The connection was lost and the session opened again, on a host that
    /// holds none of it.
    OpenedAgain,
}

/// One node's session in the video codec host, and the lease that keeps the
/// host running.
pub struct VideoCodecHostSessionOfTheNode {
    session: VideoCodecHostSession,
    _lease: ModuleHostSharedNativeServiceLease,
}

impl VideoCodecHostSessionOfTheNode {
    /// Lease the video codec host and open `opening` in it.
    pub fn open(
        node: VideoCodecNodeIdentity,
        module_host: &ModuleHost,
        opening: VideoCodecHostSessionOpening,
    ) -> Result<Self, VideoCodecRefusal> {
        let lease = module_host
            .lease_shared_native_service(VIDEO_CODEC_HOST_SHARED_NATIVE_SERVICE_NAME)
            .map_err(|refusal| {
                VideoCodecRefusal::ForGood(format!(
                    "{}: the video codec host was not leased: {refusal}",
                    node.node_type_name
                ))
            })?;
        let session = VideoCodecHostSession::open(node, lease.endpoint_socket_path(), opening)?;
        Ok(Self {
            session,
            _lease: lease,
        })
    }

    /// See [`VideoCodecHostSession::connected`].
    pub fn connected(&mut self) -> Result<VideoCodecHostSessionConnection, String> {
        self.session.connected()
    }

    /// See [`VideoCodecHostSession::ask`].
    pub fn ask(
        &mut self,
        request: &VideoCodecHostRequest,
        most_fds: usize,
    ) -> Result<(VideoCodecHostReply, Vec<OwnedFd>), String> {
        self.session.ask(request, most_fds)
    }
}

/// One session in the video codec host at an endpoint socket, opened again
/// after its connection is lost.
pub struct VideoCodecHostSession {
    node: VideoCodecNodeIdentity,
    endpoint_socket_path: PathBuf,
    opening: VideoCodecHostSessionOpening,
    connection: Option<VideoCodecHostConnection<UnixStream>>,
    last_opening: Instant,
}

impl VideoCodecHostSession {
    /// Open `opening` in the video codec host listening on
    /// `endpoint_socket_path`: refused for now when the host cannot be
    /// reached or lacks a resource another client of the GPU may free.
    pub fn open(
        node: VideoCodecNodeIdentity,
        endpoint_socket_path: &Path,
        opening: VideoCodecHostSessionOpening,
    ) -> Result<Self, VideoCodecRefusal> {
        let connection =
            open_the_session_in_the_video_codec_host(node, endpoint_socket_path, &opening)?;
        Ok(Self {
            node,
            endpoint_socket_path: endpoint_socket_path.to_path_buf(),
            opening,
            connection: Some(connection),
            last_opening: Instant::now(),
        })
    }

    /// Whether the session's connection is kept or was opened again for the
    /// next request. A lost connection is opened again once the reopen
    /// interval since the last opening has passed; before that it is refused.
    pub fn connected(&mut self) -> Result<VideoCodecHostSessionConnection, String> {
        if self.connection.is_some() {
            return Ok(VideoCodecHostSessionConnection::Kept);
        }
        let node_type_name = self.node.node_type_name;
        if self.last_opening.elapsed() < VIDEO_CODEC_HOST_REOPEN_INTERVAL {
            return Err(format!(
                "{node_type_name}: the video codec host's connection was lost, and it is opened \
                 again {VIDEO_CODEC_HOST_REOPEN_INTERVAL:?} after the last opening"
            ));
        }
        self.last_opening = Instant::now();
        let connection = open_the_session_in_the_video_codec_host(
            self.node,
            &self.endpoint_socket_path,
            &self.opening,
        )
        .map_err(VideoCodecRefusal::into_message)?;
        tracing::info!("{node_type_name}: the video codec host session was opened again");
        self.connection = Some(connection);
        Ok(VideoCodecHostSessionConnection::OpenedAgain)
    }

    /// Send `request` and wait for the reply and the fds attached to it, up
    /// to `most_fds` of them. A connection that fails is dropped, for
    /// [`Self::connected`] to open again.
    pub fn ask(
        &mut self,
        request: &VideoCodecHostRequest,
        most_fds: usize,
    ) -> Result<(VideoCodecHostReply, Vec<OwnedFd>), String> {
        let node_type_name = self.node.node_type_name;
        let mut connection = self.connection.take().ok_or_else(|| {
            format!("{node_type_name}: the session has no connection to the video codec host")
        })?;
        let reply = connection
            .send(request)
            .and_then(|()| connection.receive_with_fds::<VideoCodecHostReply>(most_fds));
        match reply {
            Ok(Some(reply_and_fds)) => {
                self.connection = Some(connection);
                Ok(reply_and_fds)
            }
            Ok(None) => Err(format!(
                "{node_type_name}: the video codec host closed the session's connection"
            )),
            Err(failure) => Err(format!(
                "{node_type_name}: the video codec host's connection was lost: {failure}"
            )),
        }
    }
}

fn open_the_session_in_the_video_codec_host(
    node: VideoCodecNodeIdentity,
    endpoint_socket_path: &Path,
    opening: &VideoCodecHostSessionOpening,
) -> Result<VideoCodecHostConnection<UnixStream>, VideoCodecRefusal> {
    let node_type_name = node.node_type_name;
    let socket = UnixStream::connect(endpoint_socket_path).map_err(|failure| {
        VideoCodecRefusal::ForNow(format!(
            "{node_type_name}: the video codec host at {} refused a connection: {failure}",
            endpoint_socket_path.display()
        ))
    })?;
    socket
        .set_read_timeout(Some(LONGEST_VIDEO_CODEC_HOST_ANSWER))
        .and_then(|()| socket.set_write_timeout(Some(LONGEST_VIDEO_CODEC_HOST_ANSWER)))
        .map_err(|failure| {
            VideoCodecRefusal::ForGood(format!(
                "{node_type_name}: the video codec host's connection took no timeout: {failure}"
            ))
        })?;
    let mut connection = VideoCodecHostConnection::over(socket);
    let not_answered = |failure: std::io::Error| {
        VideoCodecRefusal::ForNow(format!(
            "{node_type_name}: the video codec host session's opening was not answered: {failure}"
        ))
    };
    connection.send(&opening.request()).map_err(not_answered)?;
    match connection.receive::<VideoCodecHostReply>() {
        Ok(Some(VideoCodecHostReply::SessionOpened { device_name })) => {
            tracing::info!(
                device = device_name,
                "{node_type_name}: {} on the video codec host's shared device",
                opening.coding()
            );
            Ok(connection)
        }
        Ok(Some(VideoCodecHostReply::Refused { refusal })) => Err(VideoCodecRefusal::ForGood(
            format!("{node_type_name}: the video codec host refused the session: {refusal}"),
        )),
        Ok(Some(VideoCodecHostReply::RefusedForNow { refusal })) => {
            Err(VideoCodecRefusal::ForNow(format!(
                "{node_type_name}: the video codec host refused the session for now: {refusal}"
            )))
        }
        Ok(Some(VideoCodecHostReply::Encode(_) | VideoCodecHostReply::Decode(_))) => {
            Err(VideoCodecRefusal::ForGood(format!(
                "{node_type_name}: the video codec host answered the session's opening with a \
                 coding"
            )))
        }
        Ok(None) => Err(VideoCodecRefusal::ForNow(format!(
            "{node_type_name}: the video codec host closed the connection before the session \
             opened"
        ))),
        Err(failure) => Err(not_answered(failure)),
    }
}
