// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The local API's listener: a Unix socket in the runtime directory that only
//! the runtime's own user can open, and the only listener a runtime's control
//! plane has. File permission is the whole gate.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::unix_socket_path_cleared_for_bind::{
    UnixSocketPathClearedForBind, clear_unix_socket_path_for_bind,
};
use tokio_util::sync::CancellationToken;

/// Owner read-write only: the socket's file mode is what keeps every other user out.
pub const LOCAL_API_SOCKET_FILE_MODE: u32 = 0o600;

/// Bind the local API's listener at `local_api_socket_path`, refusing a path a
/// live runtime answers on, replacing a stale file, and leaving the socket at
/// [`LOCAL_API_SOCKET_FILE_MODE`]. Must run inside a tokio runtime.
fn bind_local_api_unix_listener(local_api_socket_path: &Path) -> Result<tokio::net::UnixListener> {
    let cleared = clear_unix_socket_path_for_bind(local_api_socket_path)
        .map_err(|refusal| Error::Runtime(format!("Local API socket: {refusal}")))?;
    if cleared == UnixSocketPathClearedForBind::StaleSocketFileRemoved {
        tracing::warn!(
            "Removed a stale local API socket left by a prior runtime: {}",
            local_api_socket_path.display()
        );
    }

    let listener =
        tokio::net::UnixListener::bind(local_api_socket_path).map_err(|bind_failure| {
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

/// The local API socket being served. Dropping it stops serving and removes
/// the socket file.
#[must_use = "dropping this stops the local API server"]
#[derive(Debug)]
pub struct RunningLocalApiSocketServer {
    local_api_stopping: CancellationToken,
    local_api_socket_path: PathBuf,
}

impl Drop for RunningLocalApiSocketServer {
    fn drop(&mut self) {
        self.local_api_stopping.cancel();
        // Logged, not raised: the next bind at the path clears a stale file anyway.
        if let Err(error) = remove_local_api_socket_file(&self.local_api_socket_path) {
            tracing::warn!(
                %error,
                "failed to remove the local API socket {} on stop",
                self.local_api_socket_path.display()
            );
        }
    }
}

/// Bind the local API socket at `local_api_socket_path` and serve
/// `control_plane_router` on it from `tokio_handle` until the returned server
/// is dropped, which cancels `local_api_stopping`.
pub fn serve_router_on_local_api_socket(
    control_plane_router: axum::Router,
    local_api_stopping: CancellationToken,
    tokio_handle: &tokio::runtime::Handle,
    local_api_socket_path: &Path,
) -> Result<RunningLocalApiSocketServer> {
    let local_api_listener = {
        let _entered_tokio_runtime = tokio_handle.enter();
        bind_local_api_unix_listener(local_api_socket_path)?
    };
    tokio_handle.spawn(serve_local_api_until_stopped(
        local_api_listener,
        control_plane_router,
        local_api_stopping.clone(),
    ));
    Ok(RunningLocalApiSocketServer {
        local_api_stopping,
        local_api_socket_path: local_api_socket_path.to_path_buf(),
    })
}

async fn serve_local_api_until_stopped(
    local_api_listener: tokio::net::UnixListener,
    control_plane_router: axum::Router,
    local_api_stopping: CancellationToken,
) {
    let served = axum::serve(local_api_listener, control_plane_router)
        .with_graceful_shutdown(local_api_stopping.cancelled_owned())
        .await;
    if let Err(error) = served {
        tracing::error!(%error, "the local API socket stopped serving");
    }
}

/// Remove the local API's socket file. Already gone is not a failure.
fn remove_local_api_socket_file(local_api_socket_path: &Path) -> std::io::Result<()> {
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
    use crate::handlers::router_surface_tests::control_plane_router_over_a_stub_runtime;
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

    /// Serve the real router, stub runtime behind it, on a socket bound at
    /// `local_api_socket_path`.
    fn serve_the_stub_router_at(local_api_socket_path: &Path) -> RunningLocalApiSocketServer {
        serve_router_on_local_api_socket(
            control_plane_router_over_a_stub_runtime(),
            CancellationToken::new(),
            &tokio::runtime::Handle::current(),
            local_api_socket_path,
        )
        .unwrap()
    }

    /// The stub router served on a socket in a fresh temp directory, for as
    /// long as this lives.
    struct StubRouterServedOnAFreshSocket {
        local_api_socket_path: PathBuf,
        _running_server: RunningLocalApiSocketServer,
        _directory: tempfile::TempDir,
    }

    fn serve_the_stub_router_on_a_fresh_socket() -> StubRouterServedOnAFreshSocket {
        let directory = tempfile::tempdir().unwrap();
        let local_api_socket_path = directory.path().join("local-api-Rtest.sock");
        StubRouterServedOnAFreshSocket {
            _running_server: serve_the_stub_router_at(&local_api_socket_path),
            local_api_socket_path,
            _directory: directory,
        }
    }

    /// Probe `GET /health` over the socket, assert it answered 200, and hand
    /// the answer back for any further check.
    async fn assert_the_local_api_socket_answers_health(
        local_api_socket_path: &Path,
    ) -> RawHttpResponseOverTheSocket {
        let health =
            http_exchange_over_unix_socket(local_api_socket_path, "GET /health HTTP/1.1", b"")
                .await;
        assert!(
            health.status_line.contains(" 200 "),
            "{}",
            health.status_line
        );
        health
    }

    #[tokio::test]
    async fn the_socket_is_only_its_owners_to_open() {
        let served = serve_the_stub_router_on_a_fresh_socket();

        let mode = std::fs::metadata(&served.local_api_socket_path)
            .unwrap()
            .permissions()
            .mode();

        assert_eq!(
            mode & 0o777,
            LOCAL_API_SOCKET_FILE_MODE,
            "mode {:o}",
            mode & 0o777
        );
    }

    #[tokio::test]
    async fn the_router_answers_rest_over_the_socket() {
        let served = serve_the_stub_router_on_a_fresh_socket();
        let local_api_socket_path = &served.local_api_socket_path;

        let health = assert_the_local_api_socket_answers_health(local_api_socket_path).await;
        assert_eq!(health.body, b"ok");

        let exchanged_image = http_exchange_over_unix_socket(
            local_api_socket_path,
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
    async fn the_router_upgrades_a_websocket_over_the_socket() {
        let served = serve_the_stub_router_on_a_fresh_socket();
        let mut stream = tokio::net::UnixStream::connect(&served.local_api_socket_path)
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
        let served = serve_the_stub_router_on_a_fresh_socket();
        let local_api_socket_path = &served.local_api_socket_path;

        let refusal = bind_local_api_unix_listener(local_api_socket_path)
            .unwrap_err()
            .to_string();

        assert!(
            refusal.contains(&local_api_socket_path.display().to_string()),
            "{refusal}"
        );
        assert!(
            refusal.contains("already bound by a live process"),
            "{refusal}"
        );
        assert_the_local_api_socket_answers_health(local_api_socket_path).await;
    }

    #[tokio::test]
    async fn a_stale_socket_file_is_replaced_and_served() {
        let directory = tempfile::tempdir().unwrap();
        let local_api_socket_path = directory.path().join("local-api-Rstale.sock");
        drop(std::os::unix::net::UnixListener::bind(&local_api_socket_path).unwrap());
        assert!(local_api_socket_path.exists());

        let _running_server = serve_the_stub_router_at(&local_api_socket_path);

        let health = assert_the_local_api_socket_answers_health(&local_api_socket_path).await;
        assert!(
            health
                .head
                .to_ascii_lowercase()
                .contains("content-type: text/plain")
        );
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

    /// The inode `/proc/self/fd/<raw_fd>` names when the descriptor is a socket.
    #[cfg(target_os = "linux")]
    fn socket_inode_of_descriptor(raw_fd: std::os::fd::RawFd) -> Option<u64> {
        std::fs::read_link(format!("/proc/self/fd/{raw_fd}"))
            .ok()?
            .to_str()?
            .strip_prefix("socket:[")?
            .strip_suffix(']')?
            .parse()
            .ok()
    }

    /// Inodes of the TCP sockets, IPv4 or IPv6, that this process holds open in
    /// the LISTEN state: the `/proc/self/net/tcp{,6}` rows in state `0A` whose
    /// inode one of `/proc/self/fd`'s descriptors names.
    #[cfg(target_os = "linux")]
    fn listening_tcp_socket_inodes_held_by_this_process() -> std::collections::BTreeSet<u64> {
        use std::os::fd::RawFd;

        const TCP_LISTEN_STATE: &str = "0A";
        const STATE_COLUMN: usize = 3;
        const INODE_COLUMN: usize = 9;

        let socket_inodes_held: std::collections::BTreeSet<u64> =
            std::fs::read_dir("/proc/self/fd")
                .expect("/proc/self/fd lists this process's descriptors")
                .filter_map(|descriptor| {
                    let raw_fd: RawFd = descriptor.ok()?.file_name().to_str()?.parse().ok()?;
                    socket_inode_of_descriptor(raw_fd)
                })
                .collect();

        ["/proc/self/net/tcp", "/proc/self/net/tcp6"]
            .into_iter()
            .filter_map(|tcp_table_path| std::fs::read_to_string(tcp_table_path).ok())
            .flat_map(|tcp_table| {
                tcp_table
                    .lines()
                    .skip(1)
                    .filter_map(|row| {
                        let columns: Vec<&str> = row.split_whitespace().collect();
                        if *columns.get(STATE_COLUMN)? != TCP_LISTEN_STATE {
                            return None;
                        }
                        columns.get(INODE_COLUMN)?.parse::<u64>().ok()
                    })
                    .collect::<Vec<u64>>()
            })
            .filter(|inode| socket_inodes_held.contains(inode))
            .collect()
    }

    /// Serving the real router through [`serve_router_on_local_api_socket`]
    /// leaves this process holding no new TCP listener, loopback included. The
    /// scan covers the whole test process, so no other test in this binary may
    /// hold a TCP listener. A launched node's own proof is the rig test
    /// `test_a_launched_node_listens_on_no_tcp_socket`.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn serving_the_router_on_the_local_api_socket_opens_no_tcp_listener() {
        use std::os::fd::AsRawFd;

        let mut tcp_listeners_the_scan_must_see =
            vec![std::net::TcpListener::bind("127.0.0.1:0").unwrap()];
        // A host without IPv6 loopback has no `tcp6` row to check the scan against.
        tcp_listeners_the_scan_must_see.extend(std::net::TcpListener::bind("[::1]:0").ok());
        let listening_while_holding_them = listening_tcp_socket_inodes_held_by_this_process();
        for tcp_listener in &tcp_listeners_the_scan_must_see {
            let inode_the_scan_must_see = socket_inode_of_descriptor(tcp_listener.as_raw_fd())
                .expect("a bound TCP listener's descriptor names a socket inode");
            assert!(
                listening_while_holding_them.contains(&inode_the_scan_must_see),
                "the scan must see the TCP listener this test holds at {}",
                tcp_listener.local_addr().unwrap()
            );
        }
        drop(tcp_listeners_the_scan_must_see);
        let listening_before_serving = listening_tcp_socket_inodes_held_by_this_process();

        let served = serve_the_stub_router_on_a_fresh_socket();
        assert_the_local_api_socket_answers_health(&served.local_api_socket_path).await;

        let listening_while_serving = listening_tcp_socket_inodes_held_by_this_process();
        let opened_by_serving: Vec<&u64> = listening_while_serving
            .difference(&listening_before_serving)
            .collect();
        assert!(
            opened_by_serving.is_empty(),
            "serving the local API opened TCP listeners: {opened_by_serving:?}"
        );
    }

    /// Dropping the running server removes its socket file and stops serving:
    /// a connection held open across the drop is closed by the server.
    #[tokio::test]
    async fn dropping_the_running_server_stops_serving_and_removes_its_socket_file() {
        let directory = tempfile::tempdir().unwrap();
        let local_api_socket_path = directory.path().join("local-api-Rdrop.sock");
        let running_server = serve_the_stub_router_at(&local_api_socket_path);
        let mut held_connection = tokio::net::UnixStream::connect(&local_api_socket_path)
            .await
            .unwrap();
        held_connection
            .write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .unwrap();
        let mut answered = Vec::new();
        while !answered.ends_with(b"\r\n\r\nok") {
            let mut chunk = [0u8; 256];
            let read = held_connection.read(&mut chunk).await.unwrap();
            assert_ne!(read, 0, "the server closed the connection before answering");
            answered.extend_from_slice(&chunk[..read]);
        }

        drop(running_server);

        assert!(!local_api_socket_path.exists());
        let mut after_the_drop = [0u8; 1];
        let read_after_the_drop = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            held_connection.read(&mut after_the_drop),
        )
        .await
        .expect("the server must close a held connection once it is dropped");
        assert!(
            matches!(read_after_the_drop, Ok(0) | Err(_)),
            "the held connection must be closed, not answered: {read_after_the_drop:?}"
        );
    }
}
