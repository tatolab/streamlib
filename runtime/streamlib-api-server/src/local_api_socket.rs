// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The local API's listener: a Unix socket in the runtime directory that only
//! the runtime's own user can open.
//!
//! File permission is the whole gate, so the router is served on it with no
//! bearer check.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::unix_socket_path_cleared_for_bind::{
    UnixSocketPathClearedForBind, clear_unix_socket_path_for_bind,
};

/// Owner read-write only: the socket's file mode is what keeps every other user out.
pub const LOCAL_API_SOCKET_FILE_MODE: u32 = 0o600;

/// Bind the local API's listener at `local_api_socket_path`, refusing a path a
/// live runtime answers on, replacing a stale file, and leaving the socket at
/// [`LOCAL_API_SOCKET_FILE_MODE`]. Must run inside a tokio runtime.
pub fn bind_local_api_unix_listener(
    local_api_socket_path: &Path,
) -> Result<tokio::net::UnixListener> {
    let cleared = clear_unix_socket_path_for_bind(local_api_socket_path).map_err(|refusal| {
        Error::Runtime(format!(
            "Local API socket {refusal}; is another runtime running with the same runtime_id?"
        ))
    })?;
    if cleared == UnixSocketPathClearedForBind::StaleSocketFileRemoved {
        tracing::warn!(
            "Removed a stale local API socket left by a prior runtime: {}",
            local_api_socket_path.display()
        );
    }

    let listener = tokio::net::UnixListener::bind(local_api_socket_path).map_err(|bind_failure| {
        Error::Runtime(format!(
            "Failed to bind the local API socket {}: {bind_failure}",
            local_api_socket_path.display()
        ))
    })?;
    std::fs::set_permissions(
        local_api_socket_path,
        std::fs::Permissions::from_mode(LOCAL_API_SOCKET_FILE_MODE),
    )
    .map_err(|chmod_failure| {
        Error::Runtime(format!(
            "Failed to restrict the local API socket {} to its owner: {chmod_failure}",
            local_api_socket_path.display()
        ))
    })?;
    Ok(listener)
}

/// Remove the local API's socket file once its listener has stopped. Already
/// gone is not a failure.
pub fn remove_local_api_socket_file(local_api_socket_path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(local_api_socket_path) {
        Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => Ok(()),
        removed_or_failed => removed_or_failed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control_plane_stub_support::{
        STUB_EXCHANGED_FRAME_SURFACE_ID_PERCENT_ENCODED, STUB_EXCHANGED_IMAGE_BYTES,
    };
    use crate::handlers::router_surface_and_auth_gate_tests::auth_disabled_router;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// One HTTP/1.1 exchange's status line, headers and body, as raw as the
    /// socket carried them.
    struct RawHttpResponseOverTheSocket {
        status_line: String,
        head: String,
        body: Vec<u8>,
    }

    /// Send `request_head` (and `request_body`) over a fresh connection to the
    /// socket, asking the server to close, and read the whole answer.
    async fn http_exchange_over_unix_socket(
        local_api_socket_path: &Path,
        request_head: &str,
        request_body: &[u8],
    ) -> RawHttpResponseOverTheSocket {
        let mut stream = tokio::net::UnixStream::connect(local_api_socket_path)
            .await
            .expect("connect to the local API socket");
        let head = format!(
            "{request_head}\r\nHost: localhost\r\nConnection: close\r\nContent-Length: {}\r\n\r\n",
            request_body.len()
        );
        stream.write_all(head.as_bytes()).await.unwrap();
        stream.write_all(request_body).await.unwrap();
        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).await.unwrap();
        split_raw_http_response(&raw)
    }

    fn split_raw_http_response(raw: &[u8]) -> RawHttpResponseOverTheSocket {
        let head_end = raw
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .expect("a complete response head");
        let head = String::from_utf8(raw[..head_end].to_vec()).unwrap();
        let status_line = head.lines().next().unwrap().to_string();
        RawHttpResponseOverTheSocket {
            status_line,
            head,
            body: raw[head_end + 4..].to_vec(),
        }
    }

    /// Serve the real router, stub runtime behind it, on a socket bound in a
    /// fresh temp directory.
    async fn serve_the_router_on_a_fresh_socket() -> (tempfile::TempDir, std::path::PathBuf) {
        let directory = tempfile::tempdir().unwrap();
        let local_api_socket_path = directory.path().join("local-api-Rtest.sock");
        let listener = bind_local_api_unix_listener(&local_api_socket_path).unwrap();
        tokio::spawn(async move {
            axum::serve(listener, auth_disabled_router()).await.unwrap();
        });
        (directory, local_api_socket_path)
    }

    #[tokio::test]
    async fn the_socket_is_only_its_owners_to_open() {
        let (_directory, local_api_socket_path) = serve_the_router_on_a_fresh_socket().await;

        let mode = std::fs::metadata(&local_api_socket_path)
            .unwrap()
            .permissions()
            .mode();

        assert_eq!(mode & 0o777, LOCAL_API_SOCKET_FILE_MODE, "mode {:o}", mode & 0o777);
    }

    #[tokio::test]
    async fn the_router_answers_rest_over_the_socket() {
        let (_directory, local_api_socket_path) = serve_the_router_on_a_fresh_socket().await;

        let health =
            http_exchange_over_unix_socket(&local_api_socket_path, "GET /health HTTP/1.1", b"")
                .await;
        assert!(health.status_line.contains(" 200 "), "{}", health.status_line);
        assert_eq!(health.body, b"ok");

        let exchanged_image = http_exchange_over_unix_socket(
            &local_api_socket_path,
            &format!(
                "GET /api/surfaces/{STUB_EXCHANGED_FRAME_SURFACE_ID_PERCENT_ENCODED}/image HTTP/1.1"
            ),
            b"",
        )
        .await;
        assert!(
            exchanged_image.status_line.contains(" 200 "),
            "{}",
            exchanged_image.status_line
        );
        assert_eq!(exchanged_image.body, STUB_EXCHANGED_IMAGE_BYTES);
    }

    #[tokio::test]
    async fn the_router_answers_an_mcp_tool_call_over_the_socket() {
        let (_directory, local_api_socket_path) = serve_the_router_on_a_fresh_socket().await;
        let tool_call = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {"name": "graph", "arguments": {}},
        })
        .to_string();

        let answered = http_exchange_over_unix_socket(
            &local_api_socket_path,
            "POST /mcp HTTP/1.1\r\nContent-Type: application/json",
            tool_call.as_bytes(),
        )
        .await;

        assert!(answered.status_line.contains(" 200 "), "{}", answered.status_line);
        let response: serde_json::Value = serde_json::from_slice(&answered.body).unwrap();
        assert_eq!(response["id"], 1, "{response}");
        assert!(response.get("error").is_none(), "{response}");
        assert_eq!(response["result"]["isError"], false, "{response}");
    }

    #[tokio::test]
    async fn the_router_upgrades_a_websocket_over_the_socket() {
        let (_directory, local_api_socket_path) = serve_the_router_on_a_fresh_socket().await;
        let mut stream = tokio::net::UnixStream::connect(&local_api_socket_path)
            .await
            .unwrap();
        stream
            .write_all(
                b"GET /ws/events HTTP/1.1\r\nHost: localhost\r\nConnection: Upgrade\r\n\
                  Upgrade: websocket\r\nSec-WebSocket-Version: 13\r\n\
                  Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n",
            )
            .await
            .unwrap();

        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            stream.read_exact(&mut byte).await.unwrap();
            head.push(byte[0]);
        }
        let head = String::from_utf8(head).unwrap();

        assert!(head.starts_with("HTTP/1.1 101 "), "{head}");
    }

    #[tokio::test]
    async fn a_second_bind_while_the_first_serves_is_refused_naming_the_socket() {
        let (_directory, local_api_socket_path) = serve_the_router_on_a_fresh_socket().await;

        let refusal = bind_local_api_unix_listener(&local_api_socket_path)
            .unwrap_err()
            .to_string();

        assert!(
            refusal.contains(&local_api_socket_path.display().to_string()),
            "{refusal}"
        );
        assert!(refusal.contains("already bound by a live process"), "{refusal}");
        let still_served =
            http_exchange_over_unix_socket(&local_api_socket_path, "GET /health HTTP/1.1", b"")
                .await;
        assert!(still_served.status_line.contains(" 200 "), "{}", still_served.status_line);
    }

    #[tokio::test]
    async fn a_stale_socket_file_is_replaced_and_served() {
        let directory = tempfile::tempdir().unwrap();
        let local_api_socket_path = directory.path().join("local-api-Rstale.sock");
        drop(std::os::unix::net::UnixListener::bind(&local_api_socket_path).unwrap());
        assert!(local_api_socket_path.exists());

        let listener = bind_local_api_unix_listener(&local_api_socket_path).unwrap();
        tokio::spawn(async move {
            axum::serve(listener, auth_disabled_router()).await.unwrap();
        });

        let health =
            http_exchange_over_unix_socket(&local_api_socket_path, "GET /health HTTP/1.1", b"")
                .await;
        assert!(health.status_line.contains(" 200 "), "{}", health.status_line);
        assert!(health.head.to_ascii_lowercase().contains("content-type: text/plain"));
    }

    #[test]
    fn removing_the_socket_file_is_idempotent() {
        let directory = tempfile::tempdir().unwrap();
        let local_api_socket_path = directory.path().join("local-api-Rgone.sock");
        drop(std::os::unix::net::UnixListener::bind(&local_api_socket_path).unwrap());

        remove_local_api_socket_file(&local_api_socket_path).unwrap();
        assert!(!local_api_socket_path.exists());
        remove_local_api_socket_file(&local_api_socket_path).unwrap();
    }
}
