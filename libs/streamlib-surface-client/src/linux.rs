// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::os::unix::io::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::path::Path;

/// Maximum number of DMA-BUF plane fds carried in a single SCM_RIGHTS message.
///
/// Matches the DRM format-modifier plane ceiling for every format streamlib
/// exchanges today — NV12 splits into Y/UV (2), YUV420 into Y/U/V (3), and a
/// fourth slot covers RGBA-with-auxiliary / YUVA variants. The Linux kernel's
/// SCM_MAX_FD is 253 so the wire itself is not the bottleneck; the cap exists
/// to bound `cmsg` buffer sizing and to refuse obviously-bogus surfaces before
/// they cost an fd table entry.
pub const MAX_DMA_BUF_PLANES: usize = 4;

/// Connect to the per-runtime surface-share Unix socket.
pub fn connect_to_surface_share_socket(socket_path: &Path) -> std::io::Result<UnixStream> {
    UnixStream::connect(socket_path)
}

/// `CMSG_SPACE(n * sizeof(RawFd))` at runtime.
fn cmsg_space_for(n: usize) -> usize {
    let bytes = (n * std::mem::size_of::<RawFd>()) as libc::c_uint;
    unsafe { libc::CMSG_SPACE(bytes) as usize }
}

/// Send a length-prefixed message with zero or more SCM_RIGHTS fds attached.
///
/// The fds are packed into a single `cmsg` record of type `SCM_RIGHTS`. The
/// kernel duplicates them into the receiver's fd table on successful
/// `recvmsg`; the sender retains ownership of its copies (close after send).
///
/// Returns an error if `fds.len() > MAX_DMA_BUF_PLANES` without performing
/// any syscalls — the caller still owns every fd in that case.
pub fn send_message_with_fds(
    stream: &UnixStream,
    data: &[u8],
    fds: &[RawFd],
) -> std::io::Result<()> {
    if fds.len() > MAX_DMA_BUF_PLANES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "SCM_RIGHTS fd count {} exceeds MAX_DMA_BUF_PLANES={}",
                fds.len(),
                MAX_DMA_BUF_PLANES
            ),
        ));
    }

    // First send the length prefix
    let len_bytes = (data.len() as u32).to_be_bytes();
    let mut len_iov = libc::iovec {
        iov_base: len_bytes.as_ptr() as *mut libc::c_void,
        iov_len: 4,
    };
    let mut len_msg: libc::msghdr = unsafe { std::mem::zeroed() };
    len_msg.msg_iov = &mut len_iov;
    len_msg.msg_iovlen = 1;

    let n = unsafe { libc::sendmsg(stream.as_raw_fd(), &len_msg, 0) };
    if n < 0 {
        return Err(std::io::Error::last_os_error());
    }

    // Then send the data payload with optional fds
    let mut iov = libc::iovec {
        iov_base: data.as_ptr() as *mut libc::c_void,
        iov_len: data.len(),
    };

    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;

    // Size the control buffer for the worst case we allow, so a late-arriving
    // receiver that probed a smaller cap still parses our messages.
    let cmsg_space = cmsg_space_for(MAX_DMA_BUF_PLANES);
    let mut cmsg_buf = vec![0u8; cmsg_space];

    if !fds.is_empty() {
        let payload_space = cmsg_space_for(fds.len());
        msg.msg_control = cmsg_buf.as_mut_ptr() as *mut libc::c_void;
        msg.msg_controllen = payload_space;

        let cmsg_ptr = unsafe { libc::CMSG_FIRSTHDR(&msg) };
        if cmsg_ptr.is_null() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                "CMSG_FIRSTHDR returned null",
            ));
        }
        let fd_bytes = (fds.len() * std::mem::size_of::<RawFd>()) as libc::c_uint;
        unsafe {
            (*cmsg_ptr).cmsg_level = libc::SOL_SOCKET;
            (*cmsg_ptr).cmsg_type = libc::SCM_RIGHTS;
            (*cmsg_ptr).cmsg_len = libc::CMSG_LEN(fd_bytes) as usize;
            let dst = libc::CMSG_DATA(cmsg_ptr) as *mut RawFd;
            std::ptr::copy_nonoverlapping(fds.as_ptr(), dst, fds.len());
        }
        msg.msg_controllen = payload_space;
    }

    let n = unsafe { libc::sendmsg(stream.as_raw_fd(), &msg, 0) };
    if n < 0 {
        return Err(std::io::Error::last_os_error());
    }

    Ok(())
}

/// Receive a length-prefixed message payload with up to `max_fds` SCM_RIGHTS
/// fds attached.
///
/// `msg_len` is the payload byte count, which callers obtain from the 4-byte
/// big-endian length prefix (read by `send_request_with_fds` for the
/// response, or directly by the server's connection loop for each request).
///
/// `max_fds` sizes the `cmsg` buffer allocated for the `recvmsg` call. It
/// MUST be at least as large as the greatest plane count any peer will send
/// on this connection — if the kernel delivers more fds than the buffer
/// holds, `MSG_CTRUNC` is set and **the surplus fds are silently leaked into
/// the peer's table**; this helper treats that condition as an error. Callers
/// that expect single-plane traffic can pass `1`; callers that accept
/// multi-plane should pass `MAX_DMA_BUF_PLANES`.
///
/// On any failure all received fds (if any) are closed before the error is
/// returned.
pub fn recv_message_with_fds(
    stream: &UnixStream,
    msg_len: usize,
    max_fds: usize,
) -> std::io::Result<(Vec<u8>, Vec<RawFd>)> {
    let cap = max_fds.min(MAX_DMA_BUF_PLANES);
    let cmsg_space = cmsg_space_for(cap.max(1));
    let mut cmsg_buf = vec![0u8; cmsg_space];

    let mut buf = vec![0u8; msg_len];

    let mut iov = libc::iovec {
        iov_base: buf.as_mut_ptr() as *mut libc::c_void,
        iov_len: msg_len,
    };

    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = cmsg_buf.as_mut_ptr() as *mut libc::c_void;
    msg.msg_controllen = cmsg_space;

    let n = unsafe { libc::recvmsg(stream.as_raw_fd(), &mut msg, 0) };
    if n < 0 {
        return Err(std::io::Error::last_os_error());
    }
    if n == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "Connection closed",
        ));
    }

    let received_fds = extract_cmsg_fds(&msg);

    // Check for truncated control message AFTER extracting whatever fds did
    // fit, so we can close them instead of leaking them.
    if msg.msg_flags & libc::MSG_CTRUNC != 0 {
        for fd in &received_fds {
            unsafe { libc::close(*fd) };
        }
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "SCM_RIGHTS control message truncated (peer sent more fds than max_fds)",
        ));
    }

    // If we didn't get the full message, read the remainder with plain read
    let mut total_read = n as usize;
    while total_read < msg_len {
        let remaining = &mut buf[total_read..];
        let n = unsafe {
            libc::read(
                stream.as_raw_fd(),
                remaining.as_mut_ptr() as *mut libc::c_void,
                remaining.len(),
            )
        };
        if n <= 0 {
            for fd in &received_fds {
                unsafe { libc::close(*fd) };
            }
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "Connection closed during message read",
            ));
        }
        total_read += n as usize;
    }

    Ok((buf, received_fds))
}

/// Walk every `SCM_RIGHTS` cmsg in `msg` and flatten its fds into a single
/// vec. A well-formed sender emits one record, but the kernel is free to
/// split — handling both keeps us compatible.
fn extract_cmsg_fds(msg: &libc::msghdr) -> Vec<RawFd> {
    let mut out = Vec::new();
    let mut cmsg_ptr = unsafe { libc::CMSG_FIRSTHDR(msg) };
    while !cmsg_ptr.is_null() {
        let cmsg = unsafe { &*cmsg_ptr };
        if cmsg.cmsg_level == libc::SOL_SOCKET && cmsg.cmsg_type == libc::SCM_RIGHTS {
            let data_ptr = unsafe { libc::CMSG_DATA(cmsg_ptr) } as *const RawFd;
            // cmsg_len covers header + payload; CMSG_LEN(0) is header alone.
            let header_len = unsafe { libc::CMSG_LEN(0) } as usize;
            let payload_bytes = cmsg.cmsg_len.saturating_sub(header_len);
            let n_fds = payload_bytes / std::mem::size_of::<RawFd>();
            for i in 0..n_fds {
                out.push(unsafe { *data_ptr.add(i) });
            }
        }
        cmsg_ptr = unsafe { libc::CMSG_NXTHDR(msg, cmsg_ptr) };
    }
    out
}

/// Send a request and receive a response over the surface-share socket.
///
/// `fds` is the set of SCM_RIGHTS fds to attach to the request (empty for
/// requests that don't transfer any, e.g. `check_out`, `release`).
/// `max_response_fds` caps how many fds the helper is willing to receive on
/// the response; `check_out` passes `MAX_DMA_BUF_PLANES`, `register` and
/// `release` pass `0`.
pub fn send_request_with_fds(
    stream: &UnixStream,
    request: &serde_json::Value,
    fds: &[RawFd],
    max_response_fds: usize,
) -> std::io::Result<(serde_json::Value, Vec<RawFd>)> {
    let request_bytes = serde_json::to_vec(request).map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("Failed to serialize request: {}", e),
        )
    })?;

    send_message_with_fds(stream, &request_bytes, fds)?;

    // Read response length prefix
    let mut len_buf = [0u8; 4];
    {
        let mut total = 0;
        while total < 4 {
            let n = unsafe {
                libc::read(
                    stream.as_raw_fd(),
                    len_buf[total..].as_mut_ptr() as *mut libc::c_void,
                    4 - total,
                )
            };
            if n <= 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "Failed to read response length",
                ));
            }
            total += n as usize;
        }
    }
    let response_len = u32::from_be_bytes(len_buf) as usize;

    let (response_bytes, response_fds) =
        recv_message_with_fds(stream, response_len, max_response_fds)?;

    let response: serde_json::Value = serde_json::from_slice(&response_bytes).map_err(|e| {
        // Close any fds that came with the malformed response so we don't
        // leak them into the caller's table.
        for fd in &response_fds {
            unsafe { libc::close(*fd) };
        }
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("Invalid JSON response: {}", e),
        )
    })?;

    Ok((response, response_fds))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::os::unix::io::{FromRawFd, IntoRawFd};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::PathBuf;

    /// Build a temp socket path unique to this process + monotonic nanos.
    fn tmp_socket_path(label: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        p.push(format!(
            "streamlib-surface-client-test-{}-{}-{}.sock",
            label,
            std::process::id(),
            nanos
        ));
        p
    }

    /// Create an anonymous kernel fd (memfd) seeded with `contents`.
    fn make_memfd_with(contents: &[u8]) -> RawFd {
        use std::io::{Seek, SeekFrom, Write};

        let name = std::ffi::CString::new("streamlib-surface-client-test").unwrap();
        let fd = unsafe { libc::memfd_create(name.as_ptr(), 0) };
        assert!(fd >= 0, "memfd_create failed: {}", std::io::Error::last_os_error());
        let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
        file.write_all(contents).expect("memfd write");
        file.seek(SeekFrom::Start(0)).expect("memfd rewind");
        file.into_raw_fd()
    }

    fn read_all_from_fd(fd: RawFd) -> Vec<u8> {
        use std::io::{Seek, SeekFrom};

        let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
        file.seek(SeekFrom::Start(0)).expect("recv memfd rewind");
        let mut buf = Vec::new();
        file.read_to_end(&mut buf).expect("recv memfd read");
        buf
    }

    /// Lock the on-wire length-prefix shape: 4-byte big-endian `u32` followed
    /// by the payload bytes verbatim. A drift here would silently break every
    /// subprocess consumer — the protocol has no version handshake.
    #[test]
    fn wire_format_is_big_endian_u32_length_prefix_plus_payload() {
        let socket_path = tmp_socket_path("wire-format");
        let listener = UnixListener::bind(&socket_path).expect("bind");

        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut len_buf = [0u8; 4];
            stream.read_exact(&mut len_buf).expect("read len");
            let payload_len = u32::from_be_bytes(len_buf) as usize;
            let mut payload = vec![0u8; payload_len];
            stream.read_exact(&mut payload).expect("read payload");
            (len_buf, payload)
        });

        let client = UnixStream::connect(&socket_path).expect("connect");
        let payload = b"\x00hello\xff\x01wire\x7f";
        send_message_with_fds(&client, payload, &[]).expect("send");
        drop(client);

        let (len_buf, received) = server.join().expect("server thread");
        assert_eq!(len_buf, (payload.len() as u32).to_be_bytes());
        assert_eq!(received, payload);

        let _ = std::fs::remove_file(&socket_path);
    }

    /// Round-trip a single fd via SCM_RIGHTS: the receiving side must see a
    /// fd that refers to the same kernel file object (memfd contents match
    /// byte for byte). Single-plane is the common case — locking it here
    /// protects the regression gate.
    #[test]
    fn send_recv_preserves_fd_content_via_scm_rights() {
        let socket_path = tmp_socket_path("fd-roundtrip");
        let listener = UnixListener::bind(&socket_path).expect("bind");

        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept");
            let mut len_buf = [0u8; 4];
            let mut total = 0;
            while total < 4 {
                let n = unsafe {
                    libc::read(
                        stream.as_raw_fd(),
                        len_buf[total..].as_mut_ptr() as *mut libc::c_void,
                        4 - total,
                    )
                };
                assert!(n > 0, "server read len");
                total += n as usize;
            }
            let payload_len = u32::from_be_bytes(len_buf) as usize;
            recv_message_with_fds(&stream, payload_len, 1).expect("recv")
        });

        let pattern = b"streamlib-surface-client-scm-rights-fixture-0123456789";
        let send_fd = make_memfd_with(pattern);
        let client = UnixStream::connect(&socket_path).expect("connect");
        let payload = br#"{"op":"noop"}"#;
        send_message_with_fds(&client, payload, &[send_fd]).expect("send");
        unsafe { libc::close(send_fd) };
        drop(client);

        let (received_payload, received_fds) = server.join().expect("server thread");
        assert_eq!(received_payload, payload);
        assert_eq!(received_fds.len(), 1, "fd should be delivered");
        let received_fd = received_fds[0];
        assert!(received_fd >= 0);
        assert_eq!(read_all_from_fd(received_fd), pattern);

        let _ = std::fs::remove_file(&socket_path);
    }

    /// Multi-plane round-trip: send `MAX_DMA_BUF_PLANES` distinct memfds in
    /// one SCM_RIGHTS record, confirm every plane arrives with distinct
    /// content and in order. Order matters — plane index is the only key the
    /// consumer has to pair an fd with its `plane_sizes[i]`/`plane_offsets[i]`.
    #[test]
    fn send_recv_preserves_multi_fd_order_and_content_via_scm_rights() {
        let socket_path = tmp_socket_path("multi-fd-roundtrip");
        let listener = UnixListener::bind(&socket_path).expect("bind");

        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept");
            let mut len_buf = [0u8; 4];
            let mut total = 0;
            while total < 4 {
                let n = unsafe {
                    libc::read(
                        stream.as_raw_fd(),
                        len_buf[total..].as_mut_ptr() as *mut libc::c_void,
                        4 - total,
                    )
                };
                assert!(n > 0, "server read len");
                total += n as usize;
            }
            let payload_len = u32::from_be_bytes(len_buf) as usize;
            recv_message_with_fds(&stream, payload_len, MAX_DMA_BUF_PLANES).expect("recv")
        });

        let patterns: [&[u8]; MAX_DMA_BUF_PLANES] = [
            b"plane-0-Y-plane-bytes-0000",
            b"plane-1-U-plane-bytes-1111",
            b"plane-2-V-plane-bytes-2222",
            b"plane-3-A-plane-bytes-3333",
        ];
        let send_fds: Vec<RawFd> = patterns.iter().map(|p| make_memfd_with(p)).collect();
        let client = UnixStream::connect(&socket_path).expect("connect");
        let payload = br#"{"op":"check_out","surface_id":"multi"}"#;
        send_message_with_fds(&client, payload, &send_fds).expect("send");
        for fd in &send_fds {
            unsafe { libc::close(*fd) };
        }
        drop(client);

        let (received_payload, received_fds) = server.join().expect("server thread");
        assert_eq!(received_payload, payload);
        assert_eq!(received_fds.len(), MAX_DMA_BUF_PLANES, "all planes delivered");
        for (i, fd) in received_fds.iter().enumerate() {
            assert!(*fd >= 0);
            assert_eq!(
                read_all_from_fd(*fd),
                patterns[i],
                "plane {} content preserved",
                i
            );
        }

        let _ = std::fs::remove_file(&socket_path);
    }

    /// Sending `MAX_DMA_BUF_PLANES + 1` fds returns an error and performs no
    /// syscall — caller-owned fds are not closed under our feet.
    #[test]
    fn send_rejects_oversize_fd_vec_without_closing_caller_fds() {
        let socket_path = tmp_socket_path("oversize");
        let listener = UnixListener::bind(&socket_path).expect("bind");
        let client = UnixStream::connect(&socket_path).expect("connect");
        let _accepted = listener.accept().expect("accept");

        let fds: Vec<RawFd> = (0..=MAX_DMA_BUF_PLANES)
            .map(|i| make_memfd_with(format!("plane-{}", i).as_bytes()))
            .collect();
        let err = send_message_with_fds(&client, b"payload", &fds).expect_err("must reject");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
        for fd in &fds {
            // Still readable — the error path did not close caller-owned fds.
            assert!(
                unsafe { libc::fcntl(*fd, libc::F_GETFD) } >= 0,
                "caller fd {} should still be valid after rejected send",
                fd
            );
            unsafe { libc::close(*fd) };
        }

        let _ = std::fs::remove_file(&socket_path);
    }

    /// `send_request_with_fds` composes send + length-prefixed recv into a
    /// JSON request / JSON response round-trip. Lock the full shape here so
    /// the three consumers (surface-share service, python-native, deno-native) all
    /// see identical serialization + deserialization behavior.
    #[test]
    fn send_request_round_trips_json_and_returns_response_fds() {
        let socket_path = tmp_socket_path("send-request");
        let listener = UnixListener::bind(&socket_path).expect("bind");

        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept");
            let mut len_buf = [0u8; 4];
            let mut total = 0;
            while total < 4 {
                let n = unsafe {
                    libc::read(
                        stream.as_raw_fd(),
                        len_buf[total..].as_mut_ptr() as *mut libc::c_void,
                        4 - total,
                    )
                };
                assert!(n > 0, "server read len");
                total += n as usize;
            }
            let req_len = u32::from_be_bytes(len_buf) as usize;
            let (req_bytes, req_fds) =
                recv_message_with_fds(&stream, req_len, MAX_DMA_BUF_PLANES).expect("recv req");
            assert!(req_fds.is_empty(), "this test sends no fd");

            let req: serde_json::Value = serde_json::from_slice(&req_bytes).expect("json");
            assert_eq!(req.get("op").and_then(|v| v.as_str()), Some("echo"));

            // Reply with a JSON payload + two fds so the multi-plane path is
            // exercised end-to-end at the send_request seam.
            let reply_fds = [
                make_memfd_with(b"reply-plane-0"),
                make_memfd_with(b"reply-plane-1"),
            ];
            let reply = serde_json::json!({"op": "echo", "ok": true, "echoed": req["payload"]});
            let reply_bytes = serde_json::to_vec(&reply).unwrap();
            send_message_with_fds(&stream, &reply_bytes, &reply_fds).expect("send reply");
            for fd in &reply_fds {
                unsafe { libc::close(*fd) };
            }
        });

        let client = connect_to_surface_share_socket(&socket_path).expect("connect");
        let request = serde_json::json!({"op": "echo", "payload": "hi"});
        let (response, response_fds) =
            send_request_with_fds(&client, &request, &[], MAX_DMA_BUF_PLANES)
                .expect("send_request_with_fds");
        assert_eq!(response.get("ok").and_then(|v| v.as_bool()), Some(true));
        assert_eq!(response.get("echoed").and_then(|v| v.as_str()), Some("hi"));
        assert_eq!(response_fds.len(), 2, "both reply planes delivered");
        assert_eq!(read_all_from_fd(response_fds[0]), b"reply-plane-0");
        assert_eq!(read_all_from_fd(response_fds[1]), b"reply-plane-1");

        server.join().expect("server thread");
        let _ = std::fs::remove_file(&socket_path);
    }
}
