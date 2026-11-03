// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A helper's leases on the runtime's shared native services: acquired, read
//! and released over its bridge, and every one it still holds given back when
//! the bridge goes.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::Mutex;

use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::EscalateResponse;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_request::{
    EscalateRequestAcquireSharedNativeServiceLease,
    EscalateRequestReadSharedNativeServiceLeaseState,
    EscalateRequestReleaseSharedNativeServiceLease,
};
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_response::{
    EscalateResponseErr, EscalateResponseOk,
};
use crate::core::shared_native_services::{
    SharedNativeServiceLease, SharedNativeServiceState, SharedNativeServicesOfTheEngine,
};

/// The wire spelling of a service that is accepting.
const SHARED_NATIVE_SERVICE_STATE_RUNNING: &str = "running";

/// The wire spelling of a service between a death and its next start.
const SHARED_NATIVE_SERVICE_STATE_STARTING_AGAIN: &str = "starting_again";

/// The wire spelling of a service that is not started again.
const SHARED_NATIVE_SERVICE_STATE_FAILED: &str = "failed";

/// The shared native service leases one helper process holds, for the stream
/// it belongs to.
pub(crate) struct SharedNativeServiceLeasesOfOneHelper {
    shared_native_services: Arc<SharedNativeServicesOfTheEngine>,
    tenant_stream_id: String,
    leases_by_id: Mutex<BTreeMap<String, SharedNativeServiceLease>>,
    next_lease_number: AtomicU64,
}

impl SharedNativeServiceLeasesOfOneHelper {
    /// None held yet; each lease is granted to `tenant_stream_id`.
    pub(crate) fn for_a_helper_of_the_stream(
        shared_native_services: Arc<SharedNativeServicesOfTheEngine>,
        tenant_stream_id: String,
    ) -> Self {
        Self {
            shared_native_services,
            tenant_stream_id,
            leases_by_id: Mutex::new(BTreeMap::new()),
            next_lease_number: AtomicU64::new(1),
        }
    }

    /// Give back every lease the helper still holds.
    pub(crate) fn release_every_lease(&self) {
        let leases = std::mem::take(&mut *self.leases_by_id.lock());
        drop(leases);
    }
}

impl Default for SharedNativeServiceLeasesOfOneHelper {
    /// For a helper offered no service.
    fn default() -> Self {
        Self::for_a_helper_of_the_stream(
            SharedNativeServicesOfTheEngine::in_directory(std::path::PathBuf::new()),
            String::new(),
        )
    }
}

impl std::fmt::Debug for SharedNativeServiceLeasesOfOneHelper {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SharedNativeServiceLeasesOfOneHelper")
            .field("tenant_stream_id", &self.tenant_stream_id)
            .field("leases_held", &self.leases_by_id.lock().len())
            .finish()
    }
}

/// Lease the named service for the helper's stream.
pub(super) fn handle_acquire_shared_native_service_lease(
    leases: &SharedNativeServiceLeasesOfOneHelper,
    rid: String,
    request: EscalateRequestAcquireSharedNativeServiceLease,
) -> EscalateResponse {
    let lease = match leases.shared_native_services.lease(&request.service_name) {
        Ok(lease) => lease,
        Err(refusal) => return refused(rid, refusal.to_string()),
    };
    let lease_id = format!(
        "{}-{}",
        request.service_name,
        leases.next_lease_number.fetch_add(1, Ordering::Relaxed)
    );
    let endpoint_socket_path = lease.endpoint_socket_path().to_string_lossy().into_owned();
    let state = lease.state();
    leases.leases_by_id.lock().insert(lease_id.clone(), lease);
    let mut answer = EscalateResponseOk {
        handle_id: lease_id,
        request_id: rid,
        endpoint_socket_path: Some(endpoint_socket_path),
        tenant_stream_id: Some(leases.tenant_stream_id.clone()),
        ..Default::default()
    };
    write_the_state_into(&mut answer, &state);
    EscalateResponse::Ok(answer)
}

/// Answer whether the leased service is there to connect to.
pub(super) fn handle_read_shared_native_service_lease_state(
    leases: &SharedNativeServiceLeasesOfOneHelper,
    rid: String,
    request: EscalateRequestReadSharedNativeServiceLeaseState,
) -> EscalateResponse {
    let state = match leases.leases_by_id.lock().get(&request.lease_id) {
        Some(lease) => lease.state(),
        None => return refused(rid, no_such_lease(&request.lease_id)),
    };
    let mut answer = EscalateResponseOk {
        handle_id: request.lease_id,
        request_id: rid,
        ..Default::default()
    };
    write_the_state_into(&mut answer, &state);
    EscalateResponse::Ok(answer)
}

/// Give one lease back.
pub(super) fn handle_release_shared_native_service_lease(
    leases: &SharedNativeServiceLeasesOfOneHelper,
    rid: String,
    request: EscalateRequestReleaseSharedNativeServiceLease,
) -> EscalateResponse {
    let released = leases.leases_by_id.lock().remove(&request.lease_id);
    match released {
        Some(lease) => {
            drop(lease);
            EscalateResponse::Ok(EscalateResponseOk {
                handle_id: request.lease_id,
                request_id: rid,
                ..Default::default()
            })
        }
        None => refused(rid, no_such_lease(&request.lease_id)),
    }
}

/// A service between leases or still starting is starting again as far as a
/// node is concerned: it is not there yet, and will be.
fn write_the_state_into(answer: &mut EscalateResponseOk, state: &SharedNativeServiceState) {
    let (wire_state, failure) = match state {
        SharedNativeServiceState::Running => (SHARED_NATIVE_SERVICE_STATE_RUNNING, None),
        SharedNativeServiceState::Starting
        | SharedNativeServiceState::StartingAgain
        | SharedNativeServiceState::Stopped => (SHARED_NATIVE_SERVICE_STATE_STARTING_AGAIN, None),
        SharedNativeServiceState::Failed(why) => {
            (SHARED_NATIVE_SERVICE_STATE_FAILED, Some(why.clone()))
        }
    };
    answer.shared_native_service_state = Some(wire_state.to_string());
    answer.shared_native_service_failure = failure;
}

fn no_such_lease(lease_id: &str) -> String {
    format!("this helper holds no shared native service lease {lease_id}")
}

fn refused(rid: String, message: String) -> EscalateResponse {
    EscalateResponse::Err(EscalateResponseErr {
        request_id: rid,
        message,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leases_offered_nothing() -> SharedNativeServiceLeasesOfOneHelper {
        SharedNativeServiceLeasesOfOneHelper::default()
    }

    #[test]
    fn a_service_nobody_offered_is_refused_by_name() {
        let response = handle_acquire_shared_native_service_lease(
            &leases_offered_nothing(),
            "r1".to_string(),
            EscalateRequestAcquireSharedNativeServiceLease {
                request_id: "r1".to_string(),
                service_name: "browser-host".to_string(),
            },
        );
        match response {
            EscalateResponse::Err(refusal) => {
                assert_eq!(refusal.request_id, "r1");
                assert!(
                    refusal
                        .message
                        .contains("no shared native service named browser-host"),
                    "{}",
                    refusal.message
                );
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_lease_the_helper_does_not_hold_is_refused_on_read_and_release() {
        let leases = leases_offered_nothing();
        let read = handle_read_shared_native_service_lease_state(
            &leases,
            "r2".to_string(),
            EscalateRequestReadSharedNativeServiceLeaseState {
                lease_id: "browser-host-9".to_string(),
                request_id: "r2".to_string(),
            },
        );
        let released = handle_release_shared_native_service_lease(
            &leases,
            "r3".to_string(),
            EscalateRequestReleaseSharedNativeServiceLease {
                lease_id: "browser-host-9".to_string(),
                request_id: "r3".to_string(),
            },
        );
        for response in [read, released] {
            match response {
                EscalateResponse::Err(refusal) => assert!(
                    refusal
                        .message
                        .contains("holds no shared native service lease browser-host-9"),
                    "{}",
                    refusal.message
                ),
                other => panic!("{other:?}"),
            }
        }
    }

    #[test]
    fn each_state_has_its_wire_spelling_and_a_failure_carries_why() {
        for (state, wire_state, failure) in [
            (SharedNativeServiceState::Running, "running", None),
            (SharedNativeServiceState::Starting, "starting_again", None),
            (
                SharedNativeServiceState::StartingAgain,
                "starting_again",
                None,
            ),
            (SharedNativeServiceState::Stopped, "starting_again", None),
            (
                SharedNativeServiceState::Failed("it crashed".to_string()),
                "failed",
                Some("it crashed".to_string()),
            ),
        ] {
            let mut answer = EscalateResponseOk::default();
            write_the_state_into(&mut answer, &state);
            assert_eq!(
                answer.shared_native_service_state.as_deref(),
                Some(wire_state)
            );
            assert_eq!(answer.shared_native_service_failure, failure);
        }
    }
}
