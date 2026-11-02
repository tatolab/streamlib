// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The socket a helper's native code sends escalate requests to its parent
//! over, with no Python on either end of the call.
//!
//! The parent answers each request in arrival order and every caller holds the
//! socket for its whole round trip, so an answer always belongs to the request
//! just written.

use std::io::{Read, Write};
use std::os::fd::FromRawFd;
use std::os::unix::net::UnixStream;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use parking_lot::Mutex;

/// The variable the parent names the inherited socket's descriptor in.
const NATIVE_ESCALATE_FD_ENVIRONMENT_VARIABLE: &str = "STREAMLIB_NATIVE_ESCALATE_FD";

/// How long a request waits for its answer: the bridge's own escalate bound.
const NATIVE_ESCALATE_ANSWER_TIMEOUT: Duration = Duration::from_secs(60);

/// The escalate frame tag the parent dispatches on.
const ESCALATE_REQUEST_RPC: &str = "escalate_request";

/// This helper's native escalate socket, once a round trip broke it `None`:
/// a socket that timed out may still deliver the late answer, which would be
/// read as the next request's.
pub(crate) struct HelperProcessNativeEscalateChannel {
    socket_to_parent: Mutex<Option<UnixStream>>,
    next_request_number: std::sync::atomic::AtomicU64,
}

impl HelperProcessNativeEscalateChannel {
    /// The channel the parent handed this process, opened once; `None` when
    /// the parent named none.
    pub(crate) fn of_this_helper_process() -> Option<Arc<Self>> {
        static CHANNEL: OnceLock<Option<Arc<HelperProcessNativeEscalateChannel>>> = OnceLock::new();
        CHANNEL
            .get_or_init(|| {
                let raw_fd: i32 = std::env::var(NATIVE_ESCALATE_FD_ENVIRONMENT_VARIABLE)
                    .ok()?
                    .parse()
                    .ok()?;
                // SAFETY: an fstat on a descriptor number touches nothing.
                let mut status = std::mem::MaybeUninit::<libc::stat>::uninit();
                if unsafe { libc::fstat(raw_fd, status.as_mut_ptr()) } != 0 {
                    return None;
                }
                // SAFETY: the parent handed this process the descriptor for
                // this channel alone, and this is the one place it is owned.
                let socket_to_parent = unsafe { UnixStream::from_raw_fd(raw_fd) };
                Some(Arc::new(Self::over(socket_to_parent)))
            })
            .clone()
    }

    pub(crate) fn over(socket_to_parent: UnixStream) -> Self {
        Self {
            socket_to_parent: Mutex::new(Some(socket_to_parent)),
            next_request_number: std::sync::atomic::AtomicU64::new(1),
        }
    }

    /// Send `op` (an object naming its `op`) and return the parent's `ok`
    /// answer, or the parent's refusal or the channel's failure as text.
    pub(crate) fn request_from_parent(
        &self,
        mut op: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let op_name = op
            .get("op")
            .and_then(|op_name| op_name.as_str())
            .unwrap_or("unnamed")
            .to_string();
        let request_id = format!(
            "native-{}-{}",
            std::process::id(),
            self.next_request_number
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        let fields = op
            .as_object_mut()
            .ok_or_else(|| format!("escalate op {op_name:?} is not an object"))?;
        fields.insert("rpc".into(), ESCALATE_REQUEST_RPC.into());
        fields.insert("request_id".into(), request_id.clone().into());

        let mut socket_slot = self.socket_to_parent.lock();
        let socket_to_parent = socket_slot.as_mut().ok_or_else(|| {
            format!(
                "the native escalate channel to the parent broke on an earlier request, so \
                 {op_name:?} cannot be sent"
            )
        })?;
        let answer = round_trip(socket_to_parent, &op);
        let answer = match answer {
            Ok(answer) => answer,
            Err(channel_failure) => {
                *socket_slot = None;
                return Err(format!(
                    "the native escalate channel to the parent failed on {op_name:?}: \
                     {channel_failure}"
                ));
            }
        };
        if answer.get("request_id").and_then(|id| id.as_str()) != Some(request_id.as_str()) {
            *socket_slot = None;
            return Err(format!(
                "the parent answered {op_name:?} with another request's answer: {answer}"
            ));
        }
        if answer.get("result").and_then(|result| result.as_str()) == Some("ok") {
            return Ok(answer);
        }
        Err(answer
            .get("message")
            .and_then(|message| message.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| format!("the parent refused {op_name:?}")))
    }
}

fn round_trip(
    socket_to_parent: &mut UnixStream,
    request: &serde_json::Value,
) -> std::io::Result<serde_json::Value> {
    let payload = serde_json::to_vec(request)?;
    let payload_length = u32::try_from(payload.len())
        .map_err(|_| std::io::Error::other("an escalate request past 4 GiB"))?;
    socket_to_parent.write_all(&payload_length.to_be_bytes())?;
    socket_to_parent.write_all(&payload)?;
    socket_to_parent.flush()?;
    socket_to_parent.set_read_timeout(Some(NATIVE_ESCALATE_ANSWER_TIMEOUT))?;
    let mut answer_length = [0u8; 4];
    socket_to_parent.read_exact(&mut answer_length)?;
    let mut answer = vec![0u8; u32::from_be_bytes(answer_length) as usize];
    socket_to_parent.read_exact(&mut answer)?;
    Ok(serde_json::from_slice(&answer)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_one_request(parent_end: &mut UnixStream) -> serde_json::Value {
        let mut length = [0u8; 4];
        parent_end
            .read_exact(&mut length)
            .expect("a request length");
        let mut request = vec![0u8; u32::from_be_bytes(length) as usize];
        parent_end.read_exact(&mut request).expect("a request");
        serde_json::from_slice(&request).expect("a JSON request")
    }

    fn answer(parent_end: &mut UnixStream, answer: &serde_json::Value) {
        let payload = serde_json::to_vec(answer).expect("an answer encodes");
        parent_end
            .write_all(&(payload.len() as u32).to_be_bytes())
            .expect("an answer length");
        parent_end.write_all(&payload).expect("an answer");
    }

    #[test]
    fn an_ok_answer_comes_back_and_a_refusal_carries_the_parents_message() {
        let (child_end, mut parent_end) = UnixStream::pair().expect("socketpair");
        let channel = HelperProcessNativeEscalateChannel::over(child_end);
        let parent = std::thread::spawn(move || {
            let request = read_one_request(&mut parent_end);
            assert_eq!(request["rpc"], "escalate_request");
            assert_eq!(request["op"], "wait_device_idle");
            answer(
                &mut parent_end,
                &serde_json::json!({"request_id": request["request_id"], "result": "ok"}),
            );
            let request = read_one_request(&mut parent_end);
            answer(
                &mut parent_end,
                &serde_json::json!({
                    "request_id": request["request_id"],
                    "result": "err",
                    "message": "no such surface",
                }),
            );
        });
        channel
            .request_from_parent(serde_json::json!({"op": "wait_device_idle"}))
            .expect("the parent answers ok");
        let refusal = channel
            .request_from_parent(serde_json::json!({"op": "release_handle"}))
            .expect_err("the parent refuses");
        assert_eq!(refusal, "no such surface");
        parent.join().expect("the parent stand-in");
    }

    #[test]
    fn a_closed_parent_breaks_the_channel_for_every_later_request() {
        let (child_end, parent_end) = UnixStream::pair().expect("socketpair");
        let channel = HelperProcessNativeEscalateChannel::over(child_end);
        drop(parent_end);
        let first = channel
            .request_from_parent(serde_json::json!({"op": "wait_device_idle"}))
            .expect_err("a closed parent answers nothing");
        assert!(first.contains("failed"), "{first}");
        let later = channel
            .request_from_parent(serde_json::json!({"op": "wait_device_idle"}))
            .expect_err("a broken channel stays broken");
        assert!(later.contains("broke on an earlier request"), "{later}");
    }
}
