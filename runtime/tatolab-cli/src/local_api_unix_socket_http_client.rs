// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Plain HTTP/1.1 over a runtime's local API socket, through hyper's client: the routes `tatolab`
//! reaches without MCP. The request carries no credential; the socket's file mode is the gate.

#![cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "no verb in this build sends a plain request yet; `exchange` and `mcp` are its \
                  callers"
    )
)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use http_body_util::{BodyExt, Full};
use hyper::body::{Bytes, Incoming};
use hyper::{HeaderMap, Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;

/// The `Host` every request names; the socket path is the address.
const LOCAL_API_HOST_HEADER_VALUE: &str = "localhost";

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
    // Ends on its own once the response is read, or once an upgraded stream is dropped.
    tokio::spawn(local_api_connection.with_upgrades());
    request_sender
        .send_request(request)
        .await
        .map_err(|send_failure| local_api_unreachable(local_api_socket_path, send_failure))
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
