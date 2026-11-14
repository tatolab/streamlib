// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Length-prefixed msgpack messages over one Unix socket, with the descriptors
//! a message carries as `SCM_RIGHTS`.

use std::collections::VecDeque;
use std::io::Write;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use serde::Serialize;
use serde::de::DeserializeOwned;

/// The longest one message may be, room for one 3840×2160 RGBA page input
/// frame; a longer length prefix ends the connection.
pub const MOST_TAB_PROTOCOL_MESSAGE_BYTES: usize = 40 * 1024 * 1024;

/// The most descriptors one read takes in.
const MOST_DESCRIPTORS_RECEIVED_PER_READ: usize = 8;

const LENGTH_PREFIX_BYTES: usize = 4;

/// Why a message did not cross the socket.
#[derive(Debug)]
pub enum TabProtocolSocketFailure {
    /// The peer closed the connection between messages.
    Ended,
    Io(std::io::Error),
    Encoding(String),
    /// A message named more descriptors than arrived with it.
    MissingDescriptor,
}

impl std::fmt::Display for TabProtocolSocketFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ended => formatter.write_str("the connection ended"),
            Self::Io(failure) => write!(formatter, "{failure}"),
            Self::Encoding(failure) => write!(formatter, "a message did not decode: {failure}"),
            Self::MissingDescriptor => {
                formatter.write_str("a message arrived without the descriptor it carries")
            }
        }
    }
}

impl std::error::Error for TabProtocolSocketFailure {}

impl From<std::io::Error> for TabProtocolSocketFailure {
    fn from(failure: std::io::Error) -> Self {
        Self::Io(failure)
    }
}

/// The sending side of a tab connection.
pub struct TabProtocolMessageWriter {
    socket: UnixStream,
    longest_send: Option<Duration>,
}

impl TabProtocolMessageWriter {
    pub fn over(socket: UnixStream) -> Self {
        Self {
            socket,
            longest_send: None,
        }
    }

    /// Fail any send not finished within `longest_send`, however the peer drains it.
    pub fn sending_within(mut self, longest_send: Duration) -> Self {
        self.longest_send = Some(longest_send);
        self
    }

    /// Send `message`, with `attached_descriptors` riding on its first byte.
    pub fn send(
        &mut self,
        message: &impl Serialize,
        attached_descriptors: &[RawFd],
    ) -> Result<(), TabProtocolSocketFailure> {
        let encoded = rmp_serde::to_vec_named(message)
            .map_err(|failure| TabProtocolSocketFailure::Encoding(failure.to_string()))?;
        if encoded.len() > MOST_TAB_PROTOCOL_MESSAGE_BYTES {
            return Err(TabProtocolSocketFailure::Encoding(format!(
                "a {} byte message is past the {MOST_TAB_PROTOCOL_MESSAGE_BYTES} byte bound",
                encoded.len()
            )));
        }
        let mut framed = Vec::with_capacity(LENGTH_PREFIX_BYTES + encoded.len());
        framed.extend_from_slice(&(encoded.len() as u32).to_le_bytes());
        framed.extend_from_slice(&encoded);
        let send_deadline = self
            .longest_send
            .map(|longest_send| Instant::now() + longest_send);
        let sent = if attached_descriptors.is_empty() {
            0
        } else {
            self.limit_the_next_write_to(send_deadline)?;
            send_with_descriptors(self.socket.as_raw_fd(), &framed, attached_descriptors)?
        };
        let mut unsent = &framed[sent..];
        while !unsent.is_empty() {
            self.limit_the_next_write_to(send_deadline)?;
            match self.socket.write(unsent) {
                Ok(0) => return Err(std::io::Error::from(std::io::ErrorKind::WriteZero).into()),
                Ok(written) => unsent = &unsent[written..],
                Err(failure) if failure.kind() == std::io::ErrorKind::Interrupted => {}
                Err(failure) => return Err(failure.into()),
            }
        }
        Ok(())
    }

    fn limit_the_next_write_to(
        &self,
        send_deadline: Option<Instant>,
    ) -> Result<(), TabProtocolSocketFailure> {
        let Some(send_deadline) = send_deadline else {
            return Ok(());
        };
        let remaining = send_deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(std::io::Error::from(std::io::ErrorKind::TimedOut).into());
        }
        self.socket.set_write_timeout(Some(remaining))?;
        Ok(())
    }

    /// Shut both directions down, so the peer's reader sees the end.
    pub fn shut_down(&self) {
        let _ = self.socket.shutdown(std::net::Shutdown::Both);
    }
}

/// The receiving side of a tab connection, keeping the descriptors that
/// arrive until the message carrying them takes them.
pub struct TabProtocolMessageReader {
    socket: UnixStream,
    received_bytes: Vec<u8>,
    received_descriptors: VecDeque<OwnedFd>,
    receive_deadline: Option<Instant>,
}

impl TabProtocolMessageReader {
    pub fn over(socket: UnixStream) -> Self {
        Self {
            socket,
            received_bytes: Vec::new(),
            received_descriptors: VecDeque::new(),
            receive_deadline: None,
        }
    }

    /// The next message, waiting for it.
    pub fn receive<Message: DeserializeOwned>(
        &mut self,
    ) -> Result<Message, TabProtocolSocketFailure> {
        self.receive_deadline = None;
        self.receive_framed()
    }

    /// The next message, failing with a timeout unless all of it arrives
    /// within `longest_wait`, however the peer paces its bytes; the socket
    /// waits without bound again afterwards.
    pub fn receive_within<Message: DeserializeOwned>(
        &mut self,
        longest_wait: Duration,
    ) -> Result<Message, TabProtocolSocketFailure> {
        self.receive_deadline = Some(Instant::now() + longest_wait);
        let received = self.receive_framed();
        self.receive_deadline = None;
        let unbounded_again = self.socket.set_read_timeout(None);
        let received = received?;
        unbounded_again?;
        Ok(received)
    }

    /// Close every descriptor that arrived and no message took.
    pub fn discard_attached_descriptors(&mut self) {
        self.received_descriptors.clear();
    }

    fn receive_framed<Message: DeserializeOwned>(
        &mut self,
    ) -> Result<Message, TabProtocolSocketFailure> {
        self.fill_to(LENGTH_PREFIX_BYTES)?;
        let mut length_prefix = [0u8; LENGTH_PREFIX_BYTES];
        length_prefix.copy_from_slice(&self.received_bytes[..LENGTH_PREFIX_BYTES]);
        let message_length = u32::from_le_bytes(length_prefix) as usize;
        if message_length > MOST_TAB_PROTOCOL_MESSAGE_BYTES {
            return Err(TabProtocolSocketFailure::Encoding(format!(
                "a {message_length} byte message is past the {MOST_TAB_PROTOCOL_MESSAGE_BYTES} \
                 byte bound"
            )));
        }
        self.fill_to(LENGTH_PREFIX_BYTES + message_length)?;
        let message = rmp_serde::from_slice(
            &self.received_bytes[LENGTH_PREFIX_BYTES..LENGTH_PREFIX_BYTES + message_length],
        )
        .map_err(|failure| TabProtocolSocketFailure::Encoding(failure.to_string()));
        self.received_bytes
            .drain(..LENGTH_PREFIX_BYTES + message_length);
        message
    }

    /// The oldest descriptor received and not yet taken.
    pub fn take_attached_descriptor(&mut self) -> Result<OwnedFd, TabProtocolSocketFailure> {
        self.received_descriptors
            .pop_front()
            .ok_or(TabProtocolSocketFailure::MissingDescriptor)
    }

    fn fill_to(&mut self, wanted_bytes: usize) -> Result<(), TabProtocolSocketFailure> {
        let mut chunk = [0u8; 64 * 1024];
        while self.received_bytes.len() < wanted_bytes {
            if let Some(receive_deadline) = self.receive_deadline {
                let remaining = receive_deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err(std::io::Error::from(std::io::ErrorKind::TimedOut).into());
                }
                self.socket.set_read_timeout(Some(remaining))?;
            }
            let read = receive_with_descriptors(
                self.socket.as_raw_fd(),
                &mut chunk,
                &mut self.received_descriptors,
            )?;
            if read == 0 {
                return Err(if self.received_bytes.is_empty() {
                    TabProtocolSocketFailure::Ended
                } else {
                    TabProtocolSocketFailure::Io(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "the connection ended inside a message",
                    ))
                });
            }
            self.received_bytes.extend_from_slice(&chunk[..read]);
        }
        Ok(())
    }
}

fn control_message_space(descriptor_count: usize) -> usize {
    // SAFETY: CMSG_SPACE only computes a size.
    unsafe { libc::CMSG_SPACE((descriptor_count * std::mem::size_of::<RawFd>()) as u32) as usize }
}

fn send_with_descriptors(
    socket: RawFd,
    bytes: &[u8],
    descriptors: &[RawFd],
) -> std::io::Result<usize> {
    let mut control = vec![0u8; control_message_space(descriptors.len())];
    let mut io_vector = libc::iovec {
        iov_base: bytes.as_ptr().cast_mut().cast(),
        iov_len: bytes.len(),
    };
    // SAFETY: zeroed is a valid msghdr; every pointer set below outlives the
    // sendmsg call, and the control buffer is CMSG_SPACE for the descriptors.
    let mut header: libc::msghdr = unsafe { std::mem::zeroed() };
    header.msg_iov = &mut io_vector;
    header.msg_iovlen = 1;
    header.msg_control = control.as_mut_ptr().cast();
    header.msg_controllen = control.len() as _;
    unsafe {
        let control_header = libc::CMSG_FIRSTHDR(&header);
        (*control_header).cmsg_level = libc::SOL_SOCKET;
        (*control_header).cmsg_type = libc::SCM_RIGHTS;
        (*control_header).cmsg_len = libc::CMSG_LEN(std::mem::size_of_val(descriptors) as u32) as _;
        std::ptr::copy_nonoverlapping(
            descriptors.as_ptr(),
            libc::CMSG_DATA(control_header).cast::<RawFd>(),
            descriptors.len(),
        );
    }
    loop {
        // SAFETY: the header names live buffers for the length of the call.
        let sent = unsafe { libc::sendmsg(socket, &header, 0) };
        if sent >= 0 {
            return Ok(sent as usize);
        }
        let failure = std::io::Error::last_os_error();
        if failure.kind() != std::io::ErrorKind::Interrupted {
            return Err(failure);
        }
    }
}

fn receive_with_descriptors(
    socket: RawFd,
    buffer: &mut [u8],
    received_descriptors: &mut VecDeque<OwnedFd>,
) -> std::io::Result<usize> {
    let mut control = vec![0u8; control_message_space(MOST_DESCRIPTORS_RECEIVED_PER_READ)];
    let mut io_vector = libc::iovec {
        iov_base: buffer.as_mut_ptr().cast(),
        iov_len: buffer.len(),
    };
    // SAFETY: as in `send_with_descriptors`.
    let mut header: libc::msghdr = unsafe { std::mem::zeroed() };
    header.msg_iov = &mut io_vector;
    header.msg_iovlen = 1;
    header.msg_control = control.as_mut_ptr().cast();
    header.msg_controllen = control.len() as _;
    #[cfg(target_os = "linux")]
    let flags = libc::MSG_CMSG_CLOEXEC;
    #[cfg(not(target_os = "linux"))]
    let flags = 0;
    let read = loop {
        // SAFETY: the header names live buffers for the length of the call.
        let read = unsafe { libc::recvmsg(socket, &mut header, flags) };
        if read >= 0 {
            break read as usize;
        }
        let failure = std::io::Error::last_os_error();
        if failure.kind() != std::io::ErrorKind::Interrupted {
            return Err(failure);
        }
    };
    // SAFETY: the kernel filled `control` up to `msg_controllen` with whole
    // control messages, which the CMSG macros walk.
    unsafe {
        let mut control_header = libc::CMSG_FIRSTHDR(&header);
        while !control_header.is_null() {
            if (*control_header).cmsg_level == libc::SOL_SOCKET
                && (*control_header).cmsg_type == libc::SCM_RIGHTS
            {
                let data_bytes = (*control_header).cmsg_len as usize - libc::CMSG_LEN(0) as usize;
                let data = libc::CMSG_DATA(control_header).cast::<RawFd>();
                for index in 0..data_bytes / std::mem::size_of::<RawFd>() {
                    let descriptor = std::ptr::read_unaligned(data.add(index));
                    #[cfg(not(target_os = "linux"))]
                    libc::fcntl(descriptor, libc::F_SETFD, libc::FD_CLOEXEC);
                    received_descriptors.push_back(OwnedFd::from_raw_fd(descriptor));
                }
            }
            control_header = libc::CMSG_NXTHDR(&header, control_header);
        }
    }
    Ok(read)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{TabProtocolMessageFromBrowserHost, TabProtocolMessageFromNode};
    use std::io::{Read, Seek};

    #[test]
    fn a_send_the_peer_never_reads_fails_by_its_deadline() {
        let (node_end, _host_end_that_never_reads) = UnixStream::pair().unwrap();
        let mut writer =
            TabProtocolMessageWriter::over(node_end).sending_within(Duration::from_millis(200));
        let message_larger_than_the_socket_buffer = TabProtocolMessageFromBrowserHost::TabLog {
            level: crate::BrowserHostTabLogLevel::Info,
            message: "x".repeat(8 * 1024 * 1024),
        };

        let started = Instant::now();
        let sent = writer.send(&message_larger_than_the_socket_buffer, &[]);

        assert!(sent.is_err());
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn messages_cross_in_order() {
        let (node_end, host_end) = UnixStream::pair().unwrap();
        let mut writer = TabProtocolMessageWriter::over(node_end);
        let mut reader = TabProtocolMessageReader::over(host_end);
        for slot in 0..100 {
            writer
                .send(&TabProtocolMessageFromNode::ReleaseFrameSlot { slot }, &[])
                .unwrap();
        }
        for slot in 0..100 {
            assert_eq!(
                reader.receive::<TabProtocolMessageFromNode>().unwrap(),
                TabProtocolMessageFromNode::ReleaseFrameSlot { slot }
            );
        }
    }

    #[test]
    fn a_descriptor_arrives_with_the_message_that_carries_it() {
        let (host_end, node_end) = UnixStream::pair().unwrap();
        let mut writer = TabProtocolMessageWriter::over(host_end);
        let mut reader = TabProtocolMessageReader::over(node_end);
        let mut shared_file = tempfile_with(b"painted");
        writer
            .send(
                &TabProtocolMessageFromBrowserHost::TabOpened {
                    tab_protocol_version: 1,
                    browser_host_process_id: 9,
                },
                &[shared_file.as_raw_fd()],
            )
            .unwrap();
        let message: TabProtocolMessageFromBrowserHost = reader.receive().unwrap();
        assert_eq!(message.attached_descriptor_count(), 1);
        let received = std::fs::File::from(reader.take_attached_descriptor().unwrap());
        let mut received = received;
        received.rewind().unwrap();
        let mut contents = String::new();
        received.read_to_string(&mut contents).unwrap();
        assert_eq!(contents, "painted");
        shared_file.rewind().unwrap();
        assert!(reader.take_attached_descriptor().is_err());
    }

    #[test]
    fn a_closed_peer_reads_as_the_end() {
        let (node_end, host_end) = UnixStream::pair().unwrap();
        drop(node_end);
        let mut reader = TabProtocolMessageReader::over(host_end);
        assert!(matches!(
            reader.receive::<TabProtocolMessageFromNode>(),
            Err(TabProtocolSocketFailure::Ended)
        ));
    }

    #[test]
    fn a_length_past_the_bound_is_refused() {
        let (mut node_end, host_end) = UnixStream::pair().unwrap();
        node_end
            .write_all(&((MOST_TAB_PROTOCOL_MESSAGE_BYTES as u32) + 1).to_le_bytes())
            .unwrap();
        let mut reader = TabProtocolMessageReader::over(host_end);
        assert!(matches!(
            reader.receive::<TabProtocolMessageFromNode>(),
            Err(TabProtocolSocketFailure::Encoding(_))
        ));
    }

    #[test]
    fn a_message_trickled_a_byte_at_a_time_fails_by_one_deadline() {
        let (mut node_end, host_end) = UnixStream::pair().unwrap();
        let trickling = std::thread::spawn(move || {
            if node_end.write_all(&64u32.to_le_bytes()).is_err() {
                return;
            }
            for _ in 0..40 {
                if node_end.write_all(&[0]).is_err() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        });
        let mut reader = TabProtocolMessageReader::over(host_end);

        let started = Instant::now();
        let received =
            reader.receive_within::<TabProtocolMessageFromNode>(Duration::from_millis(300));

        assert!(
            matches!(
                &received,
                Err(TabProtocolSocketFailure::Io(failure))
                    if matches!(
                        failure.kind(),
                        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                    )
            ),
            "{received:?}"
        );
        assert!(started.elapsed() < Duration::from_secs(1));
        drop(reader);
        trickling.join().unwrap();
    }

    #[test]
    fn after_a_bounded_receive_the_next_receive_waits_without_bound() {
        let (host_end, node_end) = UnixStream::pair().unwrap();
        let mut reader = TabProtocolMessageReader::over(host_end);
        let mut writer = TabProtocolMessageWriter::over(node_end);
        writer
            .send(&TabProtocolMessageFromNode::CloseTab, &[])
            .unwrap();
        reader
            .receive_within::<TabProtocolMessageFromNode>(Duration::from_millis(50))
            .unwrap();
        let late_sender = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            writer
                .send(&TabProtocolMessageFromNode::CloseTab, &[])
                .unwrap();
        });

        let received = reader.receive::<TabProtocolMessageFromNode>();

        assert!(
            matches!(received, Ok(TabProtocolMessageFromNode::CloseTab)),
            "{received:?}"
        );
        late_sender.join().unwrap();
    }

    #[test]
    fn descriptors_no_message_takes_are_discarded() {
        let (host_end, node_end) = UnixStream::pair().unwrap();
        let mut writer = TabProtocolMessageWriter::over(node_end);
        let mut reader = TabProtocolMessageReader::over(host_end);
        let unrequested_file = tempfile_with(b"unrequested");
        writer
            .send(
                &TabProtocolMessageFromNode::ReleaseFrameSlot { slot: 0 },
                &[unrequested_file.as_raw_fd()],
            )
            .unwrap();
        reader.receive::<TabProtocolMessageFromNode>().unwrap();

        reader.discard_attached_descriptors();

        assert!(reader.take_attached_descriptor().is_err());
    }

    fn tempfile_with(contents: &[u8]) -> std::fs::File {
        let path = std::env::temp_dir().join(format!(
            "tab-protocol-descriptor-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)
            .unwrap();
        std::fs::remove_file(&path).unwrap();
        file.write_all(contents).unwrap();
        file
    }
}
