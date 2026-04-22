// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::os::unix::io::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::path::Path;

/// Connect to the broker's Unix socket.
pub fn connect_to_broker(socket_path: &Path) -> std::io::Result<UnixStream> {
    UnixStream::connect(socket_path)
}

/// Send a length-prefixed message with an optional SCM_RIGHTS fd.
pub fn send_message_with_fd(
    stream: &UnixStream,
    data: &[u8],
    fd: Option<RawFd>,
) -> std::io::Result<()> {
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

    // Then send the data payload with optional fd
    let mut iov = libc::iovec {
        iov_base: data.as_ptr() as *mut libc::c_void,
        iov_len: data.len(),
    };

    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;

    const CMSG_SPACE_SIZE: usize =
        unsafe { libc::CMSG_SPACE(std::mem::size_of::<RawFd>() as u32) } as usize;

    // Aligned control message buffer for SCM_RIGHTS (one fd)
    #[repr(C)]
    union CmsgBuf {
        buf: [u8; CMSG_SPACE_SIZE],
        _align: libc::cmsghdr,
    }
    let mut cmsg_buf = CmsgBuf {
        buf: [0u8; CMSG_SPACE_SIZE],
    };

    if let Some(send_fd) = fd {
        msg.msg_control = unsafe { cmsg_buf.buf.as_mut_ptr() } as *mut libc::c_void;
        msg.msg_controllen = CMSG_SPACE_SIZE;

        let cmsg_ptr = unsafe { libc::CMSG_FIRSTHDR(&msg) };
        if !cmsg_ptr.is_null() {
            unsafe {
                (*cmsg_ptr).cmsg_level = libc::SOL_SOCKET;
                (*cmsg_ptr).cmsg_type = libc::SCM_RIGHTS;
                (*cmsg_ptr).cmsg_len =
                    libc::CMSG_LEN(std::mem::size_of::<RawFd>() as u32) as usize;
                let fd_ptr = libc::CMSG_DATA(cmsg_ptr) as *mut RawFd;
                *fd_ptr = send_fd;
            }
            msg.msg_controllen = CMSG_SPACE_SIZE;
        }
    }

    let n = unsafe { libc::sendmsg(stream.as_raw_fd(), &msg, 0) };
    if n < 0 {
        return Err(std::io::Error::last_os_error());
    }

    Ok(())
}

/// Receive a length-prefixed message payload with an optional SCM_RIGHTS fd.
///
/// `msg_len` is the payload byte count, which callers obtain from the 4-byte
/// big-endian length prefix (read by `send_request` for the response, or
/// directly by the server's connection loop for each request).
pub fn recv_message_with_fd(
    stream: &UnixStream,
    msg_len: usize,
) -> std::io::Result<(Vec<u8>, Option<RawFd>)> {
    const CMSG_SPACE_SIZE: usize =
        unsafe { libc::CMSG_SPACE(std::mem::size_of::<RawFd>() as u32) } as usize;

    // Aligned control message buffer for SCM_RIGHTS (one fd)
    #[repr(C)]
    union CmsgBuf {
        buf: [u8; CMSG_SPACE_SIZE],
        _align: libc::cmsghdr,
    }
    let mut cmsg_buf = CmsgBuf {
        buf: [0u8; CMSG_SPACE_SIZE],
    };

    let mut buf = vec![0u8; msg_len];

    let mut iov = libc::iovec {
        iov_base: buf.as_mut_ptr() as *mut libc::c_void,
        iov_len: msg_len,
    };

    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = unsafe { cmsg_buf.buf.as_mut_ptr() } as *mut libc::c_void;
    msg.msg_controllen = CMSG_SPACE_SIZE;

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

    // Check for truncated control message (fd silently lost)
    if msg.msg_flags & libc::MSG_CTRUNC != 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "SCM_RIGHTS control message truncated",
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
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "Connection closed during message read",
            ));
        }
        total_read += n as usize;
    }

    // Extract fd from control message if present
    let mut received_fd = None;
    let mut cmsg_ptr = unsafe { libc::CMSG_FIRSTHDR(&msg) };
    while !cmsg_ptr.is_null() {
        let cmsg = unsafe { &*cmsg_ptr };
        if cmsg.cmsg_level == libc::SOL_SOCKET && cmsg.cmsg_type == libc::SCM_RIGHTS {
            let fd_ptr = unsafe { libc::CMSG_DATA(cmsg_ptr) } as *const RawFd;
            received_fd = Some(unsafe { *fd_ptr });
        }
        cmsg_ptr = unsafe { libc::CMSG_NXTHDR(&msg, cmsg_ptr) };
    }

    Ok((buf, received_fd))
}

/// Send a request and receive a response from the broker.
pub fn send_request(
    stream: &UnixStream,
    request: &serde_json::Value,
    fd: Option<RawFd>,
) -> std::io::Result<(serde_json::Value, Option<RawFd>)> {
    let request_bytes = serde_json::to_vec(request).map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("Failed to serialize request: {}", e),
        )
    })?;

    // Send request with optional fd
    send_message_with_fd(stream, &request_bytes, fd)?;

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

    // Read response with optional fd
    let (response_bytes, response_fd) = recv_message_with_fd(stream, response_len)?;

    let response: serde_json::Value = serde_json::from_slice(&response_bytes).map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("Invalid JSON response: {}", e),
        )
    })?;

    Ok((response, response_fd))
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
            "streamlib-broker-client-test-{}-{}-{}.sock",
            label,
            std::process::id(),
            nanos
        ));
        p
    }

    /// Create an anonymous kernel fd (memfd) seeded with `contents`.
    fn make_memfd_with(contents: &[u8]) -> RawFd {
        use std::io::{Seek, SeekFrom, Write};

        let name = std::ffi::CString::new("streamlib-broker-client-test").unwrap();
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
        send_message_with_fd(&client, payload, None).expect("send");
        drop(client);

        let (len_buf, received) = server.join().expect("server thread");
        assert_eq!(len_buf, (payload.len() as u32).to_be_bytes());
        assert_eq!(received, payload);

        let _ = std::fs::remove_file(&socket_path);
    }

    /// Round-trip an fd via SCM_RIGHTS: the receiving side must see a fd that
    /// refers to the same kernel file object (memfd contents match byte for
    /// byte). This is the core consumer-client guarantee.
    #[test]
    fn send_recv_preserves_fd_content_via_scm_rights() {
        let socket_path = tmp_socket_path("fd-roundtrip");
        let listener = UnixListener::bind(&socket_path).expect("bind");

        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept");
            // Read the 4-byte length prefix first, then recv the payload + fd.
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
            recv_message_with_fd(&stream, payload_len).expect("recv")
        });

        let pattern = b"streamlib-broker-client-scm-rights-fixture-0123456789";
        let send_fd = make_memfd_with(pattern);
        let client = UnixStream::connect(&socket_path).expect("connect");
        let payload = br#"{"op":"noop"}"#;
        send_message_with_fd(&client, payload, Some(send_fd)).expect("send");
        unsafe { libc::close(send_fd) };
        drop(client);

        let (received_payload, received_fd) = server.join().expect("server thread");
        assert_eq!(received_payload, payload);
        let received_fd = received_fd.expect("fd should be delivered");
        assert!(received_fd >= 0);
        assert_eq!(read_all_from_fd(received_fd), pattern);

        let _ = std::fs::remove_file(&socket_path);
    }

    /// `send_request` composes send + length-prefixed recv into a JSON request
    /// / JSON response round-trip. Lock the full request/response shape here
    /// so the three consumers (broker proper, python-native, deno-native) all
    /// see identical serialization + deserialization behavior.
    #[test]
    fn send_request_round_trips_json_and_returns_response_fd() {
        let socket_path = tmp_socket_path("send-request");
        let listener = UnixListener::bind(&socket_path).expect("bind");

        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept");
            // Read request length + payload.
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
            let (req_bytes, req_fd) = recv_message_with_fd(&stream, req_len).expect("recv req");
            assert!(req_fd.is_none(), "this test sends no fd");

            let req: serde_json::Value = serde_json::from_slice(&req_bytes).expect("json");
            assert_eq!(req.get("op").and_then(|v| v.as_str()), Some("echo"));

            // Reply with a JSON payload + an fd.
            let reply_fd = make_memfd_with(b"reply-fd-contents");
            let reply = serde_json::json!({"op": "echo", "ok": true, "echoed": req["payload"]});
            let reply_bytes = serde_json::to_vec(&reply).unwrap();
            send_message_with_fd(&stream, &reply_bytes, Some(reply_fd)).expect("send reply");
            unsafe { libc::close(reply_fd) };
        });

        let client = connect_to_broker(&socket_path).expect("connect");
        let request = serde_json::json!({"op": "echo", "payload": "hi"});
        let (response, response_fd) =
            send_request(&client, &request, None).expect("send_request");
        assert_eq!(response.get("ok").and_then(|v| v.as_bool()), Some(true));
        assert_eq!(response.get("echoed").and_then(|v| v.as_str()), Some("hi"));
        let response_fd = response_fd.expect("reply fd delivered");
        assert_eq!(read_all_from_fd(response_fd), b"reply-fd-contents");

        server.join().expect("server thread");
        let _ = std::fs::remove_file(&socket_path);
    }
}
