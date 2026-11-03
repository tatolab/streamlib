// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The shared native service lease verbs of the module host table: a lease on
//! a native service the runtime runs for the whole machine, such as the
//! browser host, each a round trip on the helper's native escalate channel.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::Mutex;
use tatolab_module_sdk::module_host_table_abi::{
    ModuleHostTableSharedNativeServiceLease, ModuleHostTableSharedNativeServiceLeaseHandle,
    ModuleHostTableUtf8,
};

use crate::helper_process_native_escalate_channel::HelperProcessNativeEscalateChannel;

/// The parent's spelling of a service that is accepting.
const SHARED_NATIVE_SERVICE_STATE_RUNNING: &str = "running";

/// The parent's spelling of a service between a death and its next start.
const SHARED_NATIVE_SERVICE_STATE_STARTING_AGAIN: &str = "starting_again";

/// The parent's spelling of a service that is not started again.
const SHARED_NATIVE_SERVICE_STATE_FAILED: &str = "failed";

/// Whether a leased service is there to connect to, as the parent answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum SharedNativeServiceStateTheParentAnswered {
    Running,
    StartingAgain,
    Failed(String),
}

/// One lease native code holds; its text stays where it is until released.
struct SharedNativeServiceLeaseHeldByNativeCode {
    lease_id: String,
    endpoint_socket_path: Box<str>,
    tenant_stream_id: Box<str>,
}

/// One node's shared native service leases, by the handle the table hands out.
pub(super) struct SharedNativeServiceLeasesHeldByNativeCode {
    leases_by_handle_value: Mutex<HashMap<u64, SharedNativeServiceLeaseHeldByNativeCode>>,
    next_handle_value: AtomicU64,
    native_escalate_channel: Option<Arc<HelperProcessNativeEscalateChannel>>,
}

impl SharedNativeServiceLeasesHeldByNativeCode {
    pub(super) fn new(
        native_escalate_channel: Option<Arc<HelperProcessNativeEscalateChannel>>,
    ) -> Self {
        Self {
            leases_by_handle_value: Mutex::new(HashMap::new()),
            next_handle_value: AtomicU64::new(1),
            native_escalate_channel,
        }
    }

    fn native_escalate_channel(&self) -> Result<&HelperProcessNativeEscalateChannel, String> {
        self.native_escalate_channel.as_deref().ok_or_else(|| {
            "this helper was started without a native escalate channel to its parent, so \
             native code reaches no shared native service"
                .to_string()
        })
    }

    /// Lease `service_name` from the parent.
    pub(super) fn acquire(
        &self,
        service_name: &str,
    ) -> Result<ModuleHostTableSharedNativeServiceLease, String> {
        let answer = self.native_escalate_channel()?.request_from_parent(serde_json::json!({
            "op": "acquire_shared_native_service_lease",
            "service_name": service_name,
        }))?;
        let text_field = |field: &str| {
            answer
                .get(field)
                .and_then(|value| value.as_str())
                .map(str::to_string)
                .ok_or_else(|| format!("the parent granted a lease on {service_name} without {field}"))
        };
        let lease_held = SharedNativeServiceLeaseHeldByNativeCode {
            lease_id: text_field("handle_id")?,
            endpoint_socket_path: text_field("endpoint_socket_path")?.into_boxed_str(),
            tenant_stream_id: text_field("tenant_stream_id")?.into_boxed_str(),
        };
        let granted_lease = ModuleHostTableSharedNativeServiceLease {
            lease: ModuleHostTableSharedNativeServiceLeaseHandle {
                value: self.next_handle_value.fetch_add(1, Ordering::Relaxed),
            },
            endpoint_socket_path: ModuleHostTableUtf8::of(&lease_held.endpoint_socket_path),
            tenant_stream_id: ModuleHostTableUtf8::of(&lease_held.tenant_stream_id),
        };
        self.leases_by_handle_value
            .lock()
            .insert(granted_lease.lease.value, lease_held);
        Ok(granted_lease)
    }

    /// Ask the parent whether the leased service is there to connect to.
    pub(super) fn state(
        &self,
        lease: ModuleHostTableSharedNativeServiceLeaseHandle,
    ) -> Result<SharedNativeServiceStateTheParentAnswered, String> {
        let lease_id = self.lease_id_of_a_handle(lease)?;
        let answer = self.native_escalate_channel()?.request_from_parent(serde_json::json!({
            "op": "read_shared_native_service_lease_state",
            "lease_id": lease_id,
        }))?;
        state_the_parent_answered(&answer)
    }

    /// Give a lease back; a handle that names none is let go silently.
    pub(super) fn release(&self, lease: ModuleHostTableSharedNativeServiceLeaseHandle) {
        let Some(lease_held) = self.leases_by_handle_value.lock().remove(&lease.value) else {
            return;
        };
        self.give_back_to_the_parent(&lease_held.lease_id);
    }

    fn give_back_to_the_parent(&self, lease_id: &str) {
        let Ok(channel) = self.native_escalate_channel() else {
            return;
        };
        if let Err(refusal) = channel.request_from_parent(serde_json::json!({
            "op": "release_shared_native_service_lease",
            "lease_id": lease_id,
        })) {
            tracing::warn!(
                "the shared native service lease {lease_id} was not given back, so the runtime \
                 holds it until this helper ends: {refusal}"
            );
        }
    }

    fn lease_id_of_a_handle(
        &self,
        lease: ModuleHostTableSharedNativeServiceLeaseHandle,
    ) -> Result<String, String> {
        self.leases_by_handle_value
            .lock()
            .get(&lease.value)
            .map(|lease_held| lease_held.lease_id.clone())
            .ok_or_else(|| {
                format!(
                    "shared native service lease handle {} names no lease this table granted",
                    lease.value
                )
            })
    }
}

impl Drop for SharedNativeServiceLeasesHeldByNativeCode {
    fn drop(&mut self) {
        let leases_held = std::mem::take(&mut *self.leases_by_handle_value.lock());
        for lease_held in leases_held.into_values() {
            self.give_back_to_the_parent(&lease_held.lease_id);
        }
    }
}

fn state_the_parent_answered(
    answer: &serde_json::Value,
) -> Result<SharedNativeServiceStateTheParentAnswered, String> {
    match answer
        .get("shared_native_service_state")
        .and_then(|state| state.as_str())
    {
        Some(SHARED_NATIVE_SERVICE_STATE_RUNNING) => {
            Ok(SharedNativeServiceStateTheParentAnswered::Running)
        }
        Some(SHARED_NATIVE_SERVICE_STATE_STARTING_AGAIN) => {
            Ok(SharedNativeServiceStateTheParentAnswered::StartingAgain)
        }
        Some(SHARED_NATIVE_SERVICE_STATE_FAILED) => {
            Ok(SharedNativeServiceStateTheParentAnswered::Failed(
                answer
                    .get("shared_native_service_failure")
                    .and_then(|why| why.as_str())
                    .unwrap_or("the runtime gave no reason")
                    .to_string(),
            ))
        }
        other => Err(format!(
            "the parent answered a shared native service state of {other:?}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;

    fn read_one_request(parent_end: &mut UnixStream) -> serde_json::Value {
        let mut length = [0u8; 4];
        parent_end.read_exact(&mut length).expect("a request length");
        let mut request = vec![0u8; u32::from_be_bytes(length) as usize];
        parent_end.read_exact(&mut request).expect("a request");
        serde_json::from_slice(&request).expect("a JSON request")
    }

    fn answer_ok(parent_end: &mut UnixStream, request: &serde_json::Value, mut fields: serde_json::Value) {
        fields["request_id"] = request["request_id"].clone();
        fields["result"] = "ok".into();
        let payload = serde_json::to_vec(&fields).expect("an answer encodes");
        parent_end
            .write_all(&(payload.len() as u32).to_be_bytes())
            .expect("an answer length");
        parent_end.write_all(&payload).expect("an answer");
    }

    #[test]
    fn a_lease_is_acquired_read_and_given_back_over_the_native_escalate_channel() {
        let (child_end, mut parent_end) = UnixStream::pair().expect("socketpair");
        let leases = SharedNativeServiceLeasesHeldByNativeCode::new(Some(Arc::new(
            HelperProcessNativeEscalateChannel::over(child_end),
        )));
        let parent = std::thread::spawn(move || {
            let acquire = read_one_request(&mut parent_end);
            assert_eq!(acquire["op"], "acquire_shared_native_service_lease");
            assert_eq!(acquire["service_name"], "browser-host");
            answer_ok(
                &mut parent_end,
                &acquire,
                serde_json::json!({
                    "handle_id": "browser-host-1",
                    "endpoint_socket_path": "/run/user/1000/streamlib/services/browser-host.sock",
                    "tenant_stream_id": "lower-third",
                    "shared_native_service_state": "running",
                }),
            );
            let state = read_one_request(&mut parent_end);
            assert_eq!(state["op"], "read_shared_native_service_lease_state");
            assert_eq!(state["lease_id"], "browser-host-1");
            answer_ok(
                &mut parent_end,
                &state,
                serde_json::json!({
                    "handle_id": "browser-host-1",
                    "shared_native_service_state": "failed",
                    "shared_native_service_failure": "it died 5 times",
                }),
            );
            let release = read_one_request(&mut parent_end);
            assert_eq!(release["op"], "release_shared_native_service_lease");
            assert_eq!(release["lease_id"], "browser-host-1");
            answer_ok(&mut parent_end, &release, serde_json::json!({"handle_id": "browser-host-1"}));
        });

        let granted = leases.acquire("browser-host").expect("the parent grants the lease");
        // SAFETY: the lease is held, so its text is alive.
        unsafe {
            assert_eq!(
                granted.endpoint_socket_path.as_str().unwrap(),
                "/run/user/1000/streamlib/services/browser-host.sock"
            );
            assert_eq!(granted.tenant_stream_id.as_str().unwrap(), "lower-third");
        }
        assert_eq!(
            leases.state(granted.lease),
            Ok(SharedNativeServiceStateTheParentAnswered::Failed(
                "it died 5 times".to_string()
            ))
        );
        leases.release(granted.lease);
        parent.join().expect("the parent stand-in");
        assert!(leases.state(granted.lease).unwrap_err().contains("names no lease"));
    }

    #[test]
    fn a_helper_without_a_native_escalate_channel_is_refused() {
        let leases = SharedNativeServiceLeasesHeldByNativeCode::new(None);
        let refusal = leases.acquire("browser-host").unwrap_err();
        assert!(refusal.contains("without a native escalate channel"), "{refusal}");
    }

    #[test]
    fn each_state_the_parent_spells_is_read_and_another_is_refused() {
        assert_eq!(
            state_the_parent_answered(&serde_json::json!({"shared_native_service_state": "starting_again"})),
            Ok(SharedNativeServiceStateTheParentAnswered::StartingAgain)
        );
        assert_eq!(
            state_the_parent_answered(&serde_json::json!({"shared_native_service_state": "running"})),
            Ok(SharedNativeServiceStateTheParentAnswered::Running)
        );
        assert!(state_the_parent_answered(&serde_json::json!({})).is_err());
    }
}
