// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Plain HTTP/1.1 over a runtime's local API socket, through hyper's client: the routes `tatolab`
//! reaches without MCP, and the one `/mcp/stdio` upgrade `mcp` opens its stream with. The request
//! carries no credential; the socket's file mode is the gate.

use std::path::{Path, PathBuf};
use std::time::Duration;

use http_body_util::{BodyExt, Full};
use hyper::body::{Bytes, Incoming};
use hyper::header::{CONNECTION, UPGRADE};
use hyper::{HeaderMap, Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;

/// The `Host` every request names; the socket path is the address.
const LOCAL_API_HOST_HEADER_VALUE: &str = "localhost";

/// The route a runtime serves MCP's stdio framing on once the connection is upgraded.
const MCP_STDIO_REQUEST_TARGET: &str = "/mcp/stdio";

/// The `Upgrade` protocol token `/mcp/stdio` switches to; the runtime matches it verbatim.
const MCP_STDIO_UPGRADE_PROTOCOL: &str = "mcp-stdio";

/// A request body: whole, and empty for a `GET`.
pub(crate) type LocalApiHttpRequestBody = Full<Bytes>;

/// One answered request, whatever its status.
#[derive(Debug)]
pub(crate) struct LocalApiHttpResponse {
    /// The status the runtime answered with.
    pub(crate) status: StatusCode,
    /// The headers it answered with.
    pub(crate) headers: HeaderMap,
    /// The whole body.
    pub(crate) body: Bytes,
}

/// Why a request over the local API socket got no answer.
#[derive(Debug)]
pub(crate) enum LocalApiHttpRequestFailure {
    /// The request named a target that is not a valid origin-form URI.
    RequestTargetIsNotAUri {
        request_target: String,
        uri_failure: String,
    },
    /// Nothing answered HTTP on the socket, or the answer did not come in time.
    LocalApiUnreachable {
        local_api_socket_path: PathBuf,
        transport_failure: String,
    },
}

impl std::fmt::Display for LocalApiHttpRequestFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RequestTargetIsNotAUri {
                request_target,
                uri_failure,
            } => write!(
                formatter,
                "`{request_target}` is not a request target the local API can be sent: \
                 {uri_failure}"
            ),
            Self::LocalApiUnreachable {
                local_api_socket_path,
                transport_failure,
            } => write!(
                formatter,
                "no control plane reachable at {} ({transport_failure})",
                local_api_socket_path.display()
            ),
        }
    }
}

impl std::error::Error for LocalApiHttpRequestFailure {}

fn local_api_unreachable(
    local_api_socket_path: &Path,
    transport_failure: impl std::fmt::Display,
) -> LocalApiHttpRequestFailure {
    LocalApiHttpRequestFailure::LocalApiUnreachable {
        local_api_socket_path: local_api_socket_path.to_path_buf(),
        transport_failure: transport_failure.to_string(),
    }
}

/// A request to `origin_form_request_target` with `Host` filled, ready for
/// [`send_request_over_the_local_api_socket`].
pub(crate) fn local_api_request_builder(
    method: Method,
    origin_form_request_target: &str,
) -> hyper::http::request::Builder {
    Request::builder()
        .method(method)
        .uri(origin_form_request_target)
        .header(hyper::header::HOST, LOCAL_API_HOST_HEADER_VALUE)
}

/// Open one HTTP/1.1 connection to the local API socket and send `request` on it, answering the
/// response head with its body still streaming. The connection is served with upgrades enabled,
/// so a `101` answer can be taken over with `hyper::upgrade::on`. Must run inside a tokio
/// runtime, which drives the connection.
pub(crate) async fn send_request_over_the_local_api_socket(
    local_api_socket_path: &Path,
    request: Request<LocalApiHttpRequestBody>,
) -> Result<Response<Incoming>, LocalApiHttpRequestFailure> {
    let local_api_stream = tokio::net::UnixStream::connect(local_api_socket_path)
        .await
        .map_err(|connect_failure| local_api_unreachable(local_api_socket_path, connect_failure))?;
    let (mut request_sender, local_api_connection) =
        hyper::client::conn::http1::handshake(TokioIo::new(local_api_stream))
            .await
            .map_err(|handshake_failure| {
                local_api_unreachable(local_api_socket_path, handshake_failure)
            })?;
    // Served until the exchange completes, or until a `101` hands the stream to its upgrade.
    tokio::spawn(local_api_connection.with_upgrades());
    request_sender
        .send_request(request)
        .await
        .map_err(|send_failure| local_api_unreachable(local_api_socket_path, send_failure))
}

/// A runtime's MCP stream once its `101` is in: the local API socket itself, and the bytes the
/// runtime streamed behind the response head, which precede anything read from the socket.
#[derive(Debug)]
pub(crate) struct UpgradedLocalApiMcpStdioStream {
    /// The socket, carrying MCP's stdio framing both ways.
    pub(crate) local_api_stream: tokio::net::UnixStream,
    /// What hyper read past the `101`'s head before handing the socket back.
    pub(crate) bytes_streamed_behind_the_response_head: Bytes,
}

/// Why the `/mcp/stdio` upgrade handed over no stream.
#[derive(Debug)]
pub(crate) enum LocalApiMcpStdioUpgradeFailure {
    /// The upgrade request got no answer.
    RequestUnanswered(LocalApiHttpRequestFailure),
    /// The runtime answered with a status other than `101`.
    Refused { answered_status_line: String },
    /// The runtime answered `101`, and the connection was never handed over.
    ConnectionNeverHandedOver { upgrade_failure: String },
}

impl std::fmt::Display for LocalApiMcpStdioUpgradeFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RequestUnanswered(request_failure) => {
                write!(formatter, "{request_failure}")
            }
            Self::Refused {
                answered_status_line,
            } => write!(formatter, "it answered `{answered_status_line}`"),
            Self::ConnectionNeverHandedOver { upgrade_failure } => write!(
                formatter,
                "it answered `101` and never handed over the connection ({upgrade_failure})"
            ),
        }
    }
}

impl std::error::Error for LocalApiMcpStdioUpgradeFailure {}

/// The status line `response` arrived with: its version, code and reason phrase.
fn response_status_line<ResponseBody>(response: &Response<ResponseBody>) -> String {
    let reason_phrase = response
        .extensions()
        .get::<hyper::ext::ReasonPhrase>()
        .map(|non_canonical_reason_phrase| {
            String::from_utf8_lossy(non_canonical_reason_phrase.as_bytes()).into_owned()
        })
        .or_else(|| response.status().canonical_reason().map(str::to_owned))
        .unwrap_or_default();
    format!(
        "{:?} {} {reason_phrase}",
        response.version(),
        response.status().as_u16()
    )
    .trim_end()
    .to_owned()
}

/// Send the one `GET /mcp/stdio` with `Connection: Upgrade` and `Upgrade: mcp-stdio` over the
/// local API socket and, on its `101`, take the socket back from hyper. Must run inside a tokio
/// runtime, which drives the connection until the upgrade completes.
pub(crate) async fn upgrade_local_api_connection_to_mcp_stdio(
    local_api_socket_path: &Path,
) -> Result<UpgradedLocalApiMcpStdioStream, LocalApiMcpStdioUpgradeFailure> {
    let upgrade_request = local_api_request_builder(Method::GET, MCP_STDIO_REQUEST_TARGET)
        .header(CONNECTION, "Upgrade")
        .header(UPGRADE, MCP_STDIO_UPGRADE_PROTOCOL)
        .body(LocalApiHttpRequestBody::new(Bytes::new()))
        .map_err(|uri_failure| {
            LocalApiMcpStdioUpgradeFailure::RequestUnanswered(
                LocalApiHttpRequestFailure::RequestTargetIsNotAUri {
                    request_target: MCP_STDIO_REQUEST_TARGET.to_owned(),
                    uri_failure: uri_failure.to_string(),
                },
            )
        })?;
    let mut upgrade_response =
        send_request_over_the_local_api_socket(local_api_socket_path, upgrade_request)
            .await
            .map_err(LocalApiMcpStdioUpgradeFailure::RequestUnanswered)?;
    if upgrade_response.status() != StatusCode::SWITCHING_PROTOCOLS {
        return Err(LocalApiMcpStdioUpgradeFailure::Refused {
            answered_status_line: response_status_line(&upgrade_response),
        });
    }
    let upgraded_connection =
        hyper::upgrade::on(&mut upgrade_response)
            .await
            .map_err(|upgrade_failure| {
                LocalApiMcpStdioUpgradeFailure::ConnectionNeverHandedOver {
                    upgrade_failure: upgrade_failure.to_string(),
                }
            })?;
    let upgraded_connection_parts = upgraded_connection
        .downcast::<TokioIo<tokio::net::UnixStream>>()
        .map_err(|_upgraded_over_another_io| {
            LocalApiMcpStdioUpgradeFailure::ConnectionNeverHandedOver {
                upgrade_failure: "hyper handed back an IO other than the local API socket"
                    .to_owned(),
            }
        })?;
    Ok(UpgradedLocalApiMcpStdioStream {
        local_api_stream: upgraded_connection_parts.io.into_inner(),
        bytes_streamed_behind_the_response_head: upgraded_connection_parts.read_buf,
    })
}

/// `GET origin_form_request_target` over the local API socket, answering the status, headers and
/// whole body, whatever the status. Only a transport failure or `timeout` elapsing is an error.
pub(crate) fn get_whole_response_over_the_local_api_socket(
    local_api_socket_path: &Path,
    origin_form_request_target: &str,
    timeout: Duration,
) -> Result<LocalApiHttpResponse, LocalApiHttpRequestFailure> {
    let get_request = local_api_request_builder(Method::GET, origin_form_request_target)
        .body(LocalApiHttpRequestBody::new(Bytes::new()))
        .map_err(
            |uri_failure| LocalApiHttpRequestFailure::RequestTargetIsNotAUri {
                request_target: origin_form_request_target.to_owned(),
                uri_failure: uri_failure.to_string(),
            },
        )?;
    let request_tokio_runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|runtime_start_failure| {
            local_api_unreachable(
                local_api_socket_path,
                format!("could not start the request's runtime: {runtime_start_failure}"),
            )
        })?;
    request_tokio_runtime.block_on(async {
        tokio::time::timeout(timeout, async {
            let answered =
                send_request_over_the_local_api_socket(local_api_socket_path, get_request).await?;
            let (response_head, response_body) = answered.into_parts();
            let whole_body = response_body
                .collect()
                .await
                .map_err(|body_failure| local_api_unreachable(local_api_socket_path, body_failure))?
                .to_bytes();
            Ok(LocalApiHttpResponse {
                status: response_head.status,
                headers: response_head.headers,
                body: whole_body,
            })
        })
        .await
        .map_err(|_elapsed| {
            local_api_unreachable(
                local_api_socket_path,
                format!("no answer within {timeout:?}"),
            )
        })?
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::isolated_node_registry::NOTHING_LISTENS_LOCAL_API_SOCKET_PATH;
    use crate::stub_local_api_server::{
        SOURCE_SURFACE_PIXEL_HEIGHT_HEADER, SOURCE_SURFACE_PIXEL_WIDTH_HEADER, StubLocalApiScript,
        StubLocalApiServer, StubSurfaceImageAnswer,
    };

    const EXCHANGE_TEST_TIMEOUT: Duration = Duration::from_secs(10);

    fn stub_answering_surface_images<const SURFACE_COUNT: usize>(
        surface_image_answers: [(&str, StubSurfaceImageAnswer); SURFACE_COUNT],
    ) -> StubLocalApiServer {
        StubLocalApiServer::serve(StubLocalApiScript {
            surface_image_answers: HashMap::from(
                surface_image_answers.map(|(surface_id, answer)| (surface_id.to_owned(), answer)),
            ),
            ..StubLocalApiScript::default()
        })
    }

    #[test]
    fn a_get_answers_the_status_headers_and_whole_body_of_an_image() {
        let png_image_bytes = b"\x89PNG\r\n\x1a\nnot-really-a-png".as_slice();
        let stub_local_api_server = stub_answering_surface_images([(
            "slot#7",
            StubSurfaceImageAnswer::png_image(png_image_bytes, Some(1920), Some(1080)),
        )]);

        let answered = get_whole_response_over_the_local_api_socket(
            &stub_local_api_server.local_api_socket_path,
            "/api/surfaces/slot%237/image",
            EXCHANGE_TEST_TIMEOUT,
        )
        .unwrap();

        assert_eq!(answered.status, StatusCode::OK);
        assert_eq!(answered.body.as_ref(), png_image_bytes);
        assert_eq!(answered.headers[hyper::header::CONTENT_TYPE], "image/png");
        assert_eq!(answered.headers[SOURCE_SURFACE_PIXEL_WIDTH_HEADER], "1920");
        assert_eq!(answered.headers[SOURCE_SURFACE_PIXEL_HEIGHT_HEADER], "1080");
        assert_eq!(
            stub_local_api_server.recorded_image_request_paths(),
            ["/api/surfaces/slot%237/image"]
        );
    }

    #[test]
    fn a_refused_get_answers_its_status_and_body_rather_than_failing() {
        let stub_local_api_server = stub_answering_surface_images([(
            "slot#8",
            StubSurfaceImageAnswer::refusal(410, "frame recycled"),
        )]);

        for (request_target, expected_status, expected_error) in [
            (
                "/api/surfaces/slot%238/image",
                StatusCode::GONE,
                "frame recycled",
            ),
            (
                "/api/surfaces/never-published/image",
                StatusCode::NOT_FOUND,
                "no such surface",
            ),
        ] {
            let answered = get_whole_response_over_the_local_api_socket(
                &stub_local_api_server.local_api_socket_path,
                request_target,
                EXCHANGE_TEST_TIMEOUT,
            )
            .unwrap();

            assert_eq!(answered.status, expected_status, "{request_target}");
            let refusal_body: serde_json::Value = serde_json::from_slice(&answered.body).unwrap();
            assert_eq!(refusal_body, serde_json::json!({ "error": expected_error }));
        }
    }

    #[test]
    fn an_image_without_extent_headers_answers_none_of_them() {
        let stub_local_api_server = stub_answering_surface_images([(
            "slot#9",
            StubSurfaceImageAnswer::png_image(b"png", None, None),
        )]);

        let answered = get_whole_response_over_the_local_api_socket(
            &stub_local_api_server.local_api_socket_path,
            "/api/surfaces/slot%239/image",
            EXCHANGE_TEST_TIMEOUT,
        )
        .unwrap();

        assert!(
            !answered
                .headers
                .contains_key(SOURCE_SURFACE_PIXEL_WIDTH_HEADER)
        );
        assert!(
            !answered
                .headers
                .contains_key(SOURCE_SURFACE_PIXEL_HEIGHT_HEADER)
        );
    }

    #[test]
    fn a_socket_nothing_listens_on_is_named_as_unreachable() {
        let unreachable = get_whole_response_over_the_local_api_socket(
            Path::new(NOTHING_LISTENS_LOCAL_API_SOCKET_PATH),
            "/api/surfaces/slot%237/image",
            EXCHANGE_TEST_TIMEOUT,
        )
        .unwrap_err();

        assert!(
            unreachable.to_string().starts_with(&format!(
                "no control plane reachable at {NOTHING_LISTENS_LOCAL_API_SOCKET_PATH} ("
            )),
            "{unreachable}"
        );
    }

    #[test]
    fn a_socket_that_accepts_and_never_answers_fails_once_the_timeout_elapses() {
        let silent_socket_directory = tempfile::Builder::new()
            .prefix("tl-silent-")
            .tempdir_in("/tmp")
            .unwrap();
        let silent_socket_path = silent_socket_directory.path().join("local-api.sock");
        let _never_accepting_listener =
            std::os::unix::net::UnixListener::bind(&silent_socket_path).unwrap();

        let timed_out = get_whole_response_over_the_local_api_socket(
            &silent_socket_path,
            "/api/surfaces/slot%237/image",
            Duration::from_millis(200),
        )
        .unwrap_err();

        assert_eq!(
            timed_out.to_string(),
            format!(
                "no control plane reachable at {} (no answer within 200ms)",
                silent_socket_path.display()
            )
        );
    }

    #[test]
    fn a_request_target_that_is_not_a_uri_is_refused_before_anything_is_sent() {
        let refusal = get_whole_response_over_the_local_api_socket(
            Path::new(NOTHING_LISTENS_LOCAL_API_SOCKET_PATH),
            "/api/surfaces/not percent encoded/image",
            EXCHANGE_TEST_TIMEOUT,
        )
        .unwrap_err();

        assert!(
            matches!(
                refusal,
                LocalApiHttpRequestFailure::RequestTargetIsNotAUri { .. }
            ),
            "{refusal:?}"
        );
    }
}

#[cfg(test)]
mod mcp_stdio_upgrade_tests {
    use std::io::{Read, Write};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::thread::JoinHandle;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;
    use crate::isolated_node_registry::NOTHING_LISTENS_LOCAL_API_SOCKET_PATH;

    const SCRIPTED_RUNTIME_READ_TIMEOUT: Duration = Duration::from_secs(10);

    const SWITCHING_PROTOCOLS_HEAD: &[u8] =
        b"HTTP/1.1 101 Switching Protocols\r\nconnection: upgrade\r\nupgrade: mcp-stdio\r\n\r\n";

    /// A Unix socket whose one connection a script plays as the runtime, on a thread of its own.
    struct ScriptedLocalApiSocket {
        local_api_socket_path: PathBuf,
        playing_thread: Option<JoinHandle<()>>,
        _local_api_socket_directory: tempfile::TempDir,
    }

    impl ScriptedLocalApiSocket {
        fn playing(play_the_runtime: impl FnOnce(UnixStream) + Send + 'static) -> Self {
            let local_api_socket_directory = tempfile::Builder::new()
                .prefix("tl-mcp-")
                .tempdir_in("/tmp")
                .unwrap();
            let local_api_socket_path = local_api_socket_directory.path().join("local-api.sock");
            let local_api_listener = UnixListener::bind(&local_api_socket_path).unwrap();
            let playing_thread = std::thread::spawn(move || {
                let (accepted_connection, _peer_address) = local_api_listener.accept().unwrap();
                accepted_connection
                    .set_read_timeout(Some(SCRIPTED_RUNTIME_READ_TIMEOUT))
                    .unwrap();
                play_the_runtime(accepted_connection);
            });
            Self {
                local_api_socket_path,
                playing_thread: Some(playing_thread),
                _local_api_socket_directory: local_api_socket_directory,
            }
        }

        /// Wait for the script to finish, failing the test with any assertion it failed.
        fn finish(mut self) {
            if let Some(playing_thread) = self.playing_thread.take()
                && let Err(script_panic) = playing_thread.join()
            {
                std::panic::resume_unwind(script_panic);
            }
        }
    }

    fn read_until(runtime_connection: &mut UnixStream, terminator: &[u8]) -> Vec<u8> {
        let mut received = Vec::new();
        let mut one_byte = [0_u8; 1];
        while !received.ends_with(terminator) {
            if runtime_connection.read(&mut one_byte).unwrap() == 0 {
                break;
            }
            received.push(one_byte[0]);
        }
        received
    }

    fn read_to_end(runtime_connection: &mut UnixStream) -> Vec<u8> {
        let mut received = Vec::new();
        runtime_connection.read_to_end(&mut received).unwrap();
        received
    }

    fn upgrade_on_a_fresh_tokio_runtime<Outcome>(
        local_api_socket_path: &Path,
        use_the_upgrade: impl AsyncFnOnce(
            Result<UpgradedLocalApiMcpStdioStream, LocalApiMcpStdioUpgradeFailure>,
        ) -> Outcome,
    ) -> Outcome {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                use_the_upgrade(
                    upgrade_local_api_connection_to_mcp_stdio(local_api_socket_path).await,
                )
                .await
            })
    }

    #[test]
    fn the_upgrade_is_one_get_of_mcp_stdio_naming_the_upgrade_and_nothing_else() {
        let (received_request, request_received) = std::sync::mpsc::channel();
        let scripted_local_api_socket =
            ScriptedLocalApiSocket::playing(move |mut runtime_connection| {
                let request_head = read_until(&mut runtime_connection, b"\r\n\r\n");
                runtime_connection
                    .write_all(SWITCHING_PROTOCOLS_HEAD)
                    .unwrap();
                let sent_after_the_head = read_to_end(&mut runtime_connection);
                received_request
                    .send((request_head, sent_after_the_head))
                    .unwrap();
            });

        upgrade_on_a_fresh_tokio_runtime(
            &scripted_local_api_socket.local_api_socket_path,
            async |upgraded| drop(upgraded.unwrap()),
        );
        scripted_local_api_socket.finish();

        let (request_head, sent_after_the_head) = request_received.recv().unwrap();
        let request_head = String::from_utf8(request_head).unwrap();
        let mut request_head_lines = request_head.trim_end().split("\r\n");
        assert_eq!(
            request_head_lines.next(),
            Some("GET /mcp/stdio HTTP/1.1"),
            "{request_head}"
        );
        let mut header_lines: Vec<(String, String)> = request_head_lines
            .map(|header_line| {
                let (header_name, header_value) = header_line.split_once(':').unwrap();
                (
                    header_name.to_ascii_lowercase(),
                    header_value.trim().to_owned(),
                )
            })
            .collect();
        header_lines.sort();
        assert_eq!(
            header_lines,
            [
                ("connection".to_owned(), "Upgrade".to_owned()),
                ("host".to_owned(), "localhost".to_owned()),
                ("upgrade".to_owned(), "mcp-stdio".to_owned()),
            ],
            "{request_head}"
        );
        assert_eq!(
            sent_after_the_head, b"",
            "the upgrade request carries no body"
        );
    }

    #[test]
    fn bytes_streamed_with_the_101_are_handed_over_ahead_of_the_socket() {
        let scripted_local_api_socket =
            ScriptedLocalApiSocket::playing(move |mut runtime_connection| {
                read_until(&mut runtime_connection, b"\r\n\r\n");
                // One write, so the bytes land in the same read as the head.
                runtime_connection
                    .write_all(&[SWITCHING_PROTOCOLS_HEAD, b"streamed with the 101\n"].concat())
                    .unwrap();
                assert_eq!(read_until(&mut runtime_connection, b"\n"), b"go on\n");
                runtime_connection
                    .write_all(b"read from the socket\n")
                    .unwrap();
            });

        let (bytes_streamed_behind_the_head, read_from_the_socket) =
            upgrade_on_a_fresh_tokio_runtime(
                &scripted_local_api_socket.local_api_socket_path,
                async |upgraded| {
                    let UpgradedLocalApiMcpStdioStream {
                        mut local_api_stream,
                        bytes_streamed_behind_the_response_head,
                    } = upgraded.unwrap();
                    local_api_stream.write_all(b"go on\n").await.unwrap();
                    let mut read_from_the_socket = Vec::new();
                    local_api_stream
                        .read_to_end(&mut read_from_the_socket)
                        .await
                        .unwrap();
                    (
                        bytes_streamed_behind_the_response_head,
                        read_from_the_socket,
                    )
                },
            );
        scripted_local_api_socket.finish();

        assert_eq!(
            bytes_streamed_behind_the_head.as_ref(),
            b"streamed with the 101\n"
        );
        assert_eq!(read_from_the_socket, b"read from the socket\n");
    }

    #[test]
    fn a_refused_upgrade_is_named_by_the_status_line_it_was_answered_with() {
        for (answered_response, expected_status_line) in [
            (
                "HTTP/1.1 426 Upgrade Required\r\ncontent-length: 0\r\n\r\n",
                "HTTP/1.1 426 Upgrade Required",
            ),
            (
                "HTTP/1.1 503 Draining Its Streams\r\ncontent-length: 0\r\n\r\n",
                "HTTP/1.1 503 Draining Its Streams",
            ),
            (
                "HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n",
                "HTTP/1.1 200 OK",
            ),
        ] {
            let scripted_local_api_socket =
                ScriptedLocalApiSocket::playing(move |mut runtime_connection| {
                    read_until(&mut runtime_connection, b"\r\n\r\n");
                    runtime_connection
                        .write_all(answered_response.as_bytes())
                        .unwrap();
                });

            let refusal = upgrade_on_a_fresh_tokio_runtime(
                &scripted_local_api_socket.local_api_socket_path,
                async |upgraded| upgraded.unwrap_err(),
            );
            scripted_local_api_socket.finish();

            assert_eq!(
                refusal.to_string(),
                format!("it answered `{expected_status_line}`"),
                "{refusal:?}"
            );
        }
    }

    #[test]
    fn a_runtime_closing_before_it_answers_leaves_the_upgrade_unanswered() {
        let scripted_local_api_socket =
            ScriptedLocalApiSocket::playing(move |mut runtime_connection| {
                read_until(&mut runtime_connection, b"\r\n\r\n");
            });

        let unanswered = upgrade_on_a_fresh_tokio_runtime(
            &scripted_local_api_socket.local_api_socket_path,
            async |upgraded| upgraded.unwrap_err(),
        );
        scripted_local_api_socket.finish();

        assert!(
            matches!(
                unanswered,
                LocalApiMcpStdioUpgradeFailure::RequestUnanswered(
                    LocalApiHttpRequestFailure::LocalApiUnreachable { .. }
                )
            ),
            "{unanswered:?}"
        );
    }

    #[test]
    fn a_socket_nothing_listens_on_leaves_the_upgrade_unanswered_naming_it() {
        let unanswered = upgrade_on_a_fresh_tokio_runtime(
            Path::new(NOTHING_LISTENS_LOCAL_API_SOCKET_PATH),
            async |upgraded| upgraded.unwrap_err(),
        );

        assert!(
            unanswered.to_string().starts_with(&format!(
                "no control plane reachable at {NOTHING_LISTENS_LOCAL_API_SOCKET_PATH} ("
            )),
            "{unanswered}"
        );
    }
}
