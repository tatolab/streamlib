// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The helper process's end of the escalate socket, framed the way
//! `streamlib/_helper.py` frames it: a four-byte big-endian length, then the
//! JSON, with one correlation id per request.
//!
//! Shared by the present-class op tests, which each need their own process
//! because minting a window mints the process's one event loop.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use serde_json::{Value, json};

use streamlib_engine::core::helper_process_transport::SubprocessBridge;

/// Bounded so a wedged parent fails a test rather than hanging it.
const HOW_LONG_THE_HELPER_WAITS_FOR_THE_PARENT: Duration = Duration::from_secs(30);

pub struct HelperProcessEndOfTheEscalateSocket {
    socket: UnixStream,
    escalate_requests_sent_so_far: u64,
}

impl HelperProcessEndOfTheEscalateSocket {
    pub fn new(socket: UnixStream) -> Self {
        socket
            .set_read_timeout(Some(HOW_LONG_THE_HELPER_WAITS_FOR_THE_PARENT))
            .expect("the helper's socket takes a read timeout");
        Self {
            socket,
            escalate_requests_sent_so_far: 0,
        }
    }

    pub fn read_one_frame(&mut self) -> Value {
        let mut length_prefix = [0u8; 4];
        self.socket
            .read_exact(&mut length_prefix)
            .expect("the parent answers within the helper's timeout");
        let mut payload = vec![0u8; u32::from_be_bytes(length_prefix) as usize];
        self.socket
            .read_exact(&mut payload)
            .expect("the parent's frame arrives whole");
        serde_json::from_slice(&payload).expect("the parent's frame decodes")
    }

    /// One escalate round trip, correlated by `request_id` exactly as the
    /// helper's own bridge correlates it.
    pub fn escalate_request_to_the_parent(&mut self, op: Value) -> Value {
        self.escalate_requests_sent_so_far += 1;
        let request_id = format!("req-{}", self.escalate_requests_sent_so_far);
        let mut request = op;
        request["rpc"] = json!("escalate_request");
        request["request_id"] = json!(request_id);
        self.write_one_frame(&request);

        let response = self.read_one_frame();
        assert_eq!(
            response["rpc"],
            json!("escalate_response"),
            "an escalate request must be answered by an escalate response, got {response}"
        );
        assert_eq!(
            response["request_id"],
            json!(request_id),
            "the parent must correlate its answer, got {response}"
        );
        response
    }

    fn write_one_frame(&mut self, message: &Value) {
        let payload = serde_json::to_vec(message).expect("a helper frame encodes");
        self.socket
            .write_all(&(payload.len() as u32).to_be_bytes())
            .expect("the length prefix reaches the parent");
        self.socket
            .write_all(&payload)
            .expect("the payload reaches the parent");
        self.socket.flush().expect("the frame flushes");
    }
}

pub fn refusal_message_of(response: &Value, what_was_asked: &str) -> String {
    assert_eq!(
        response["result"],
        json!("err"),
        "{what_was_asked} must be refused, got {response}"
    );
    let message = response["message"]
        .as_str()
        .expect("a refusal carries a message")
        .to_string();
    // A refusal a person reads, so a hand-wrapped format string that lost its
    // line continuation is a defect rather than cosmetics — and one no other
    // gate catches, because `cargo fmt` does not reflow string literals and
    // clippy does not read them.
    assert!(
        !message.contains("  "),
        "{what_was_asked}: the refusal carries a run of literal spaces, so a line continuation \
         was lost when it was wrapped: {message:?}"
    );
    message
}

/// Drive one lifecycle command out of the parent and let the helper consume
/// it, so the phase the parent has put the child in is the phase the next
/// escalate request arrives in.
pub fn send_lifecycle_command_and_let_the_helper_read_it(
    bridge: &SubprocessBridge,
    helper: &mut HelperProcessEndOfTheEscalateSocket,
    lifecycle_command: &str,
) {
    bridge
        .send(&json!({ "cmd": lifecycle_command, "capability": "full" }))
        .expect("the parent's lifecycle command reaches the helper");
    let received = helper.read_one_frame();
    assert_eq!(
        received["cmd"],
        json!(lifecycle_command),
        "the helper must see the lifecycle command the parent sent, got {received}"
    );
}
