// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Where the publisher's session with each relay stands, reported from the
//! session's own status as it changes, and read by `list_relays` beside the
//! adapter's own state.

use std::time::Duration;

use streamlib_relay_adapter_sdk::RelayInstanceId;
use streamlib_runtime_client_contract::local_api_wire_contract::ListedRelayConnectionState;
use tokio::sync::watch;

/// The first wait before the publisher redials a relay whose session failed or
/// dropped, doubling to [`A_RELAY_IS_REDIALED_AT_LEAST_EVERY`].
pub(crate) const A_RELAY_IS_FIRST_REDIALED_AFTER: Duration = Duration::from_secs(1);

/// The longest the publisher waits between two dials of a relay it cannot
/// reach; it never stops redialing one that refuses no credential.
pub(crate) const A_RELAY_IS_REDIALED_AT_LEAST_EVERY: Duration = Duration::from_secs(10);

/// Where the publisher's session with one relay stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ExposedPortPublisherSessionState {
    /// The publisher holds no session with the relay.
    NotDialed,
    /// Dialed, and no session has connected yet.
    Dialing,
    /// A session is connected.
    Connected,
    /// The session dropped or could not be opened; the relay is redialed with
    /// backoff.
    Redialing,
    /// The relay refused the session for good — its credential refused, or an
    /// answer no redial changes — and the publisher dials it no more until the
    /// relay is handed over again.
    Ended { why: String },
}

/// The publisher's session state of one relay instance, its sender handed to
/// the publisher with the relay and kept for the instance's whole life.
pub(crate) type ExposedPortPublisherSessionStateSender =
    watch::Sender<ExposedPortPublisherSessionState>;

/// A fresh session state for one relay instance, not dialed.
pub(crate) fn a_relay_instances_session_state_not_dialed() -> ExposedPortPublisherSessionStateSender
{
    watch::channel(ExposedPortPublisherSessionState::NotDialed).0
}

impl ExposedPortPublisherSessionState {
    /// The state `list_relays` reports for a relay its adapter has joined,
    /// where this is the publisher's session with it.
    pub(crate) fn as_listed_for_a_joined_relay(&self) -> ListedRelayConnectionState {
        match self {
            Self::NotDialed | Self::Dialing => ListedRelayConnectionState::Joining,
            Self::Connected => ListedRelayConnectionState::Joined,
            Self::Redialing => ListedRelayConnectionState::Lost {
                why: "the publisher's session with the relay is not connected".to_string(),
                rejoining_after_ms: u64::try_from(A_RELAY_IS_REDIALED_AT_LEAST_EVERY.as_millis())
                    .unwrap_or(u64::MAX),
            },
            Self::Ended { why } => ListedRelayConnectionState::NotJoined {
                why: Some(format!("the relay ended the publisher's session: {why}")),
            },
        }
    }
}

/// The session state `connection` reports, sent to `session_state` as it
/// changes, until the session ends or the task holding this is dropped.
pub(crate) async fn report_the_session_state_as_it_changes(
    relay_id: RelayInstanceId,
    mut connection: moq_tokio::Connection,
    session_state: ExposedPortPublisherSessionStateSender,
) {
    session_state.send_replace(ExposedPortPublisherSessionState::Dialing);
    loop {
        match connection.status().await {
            Ok(moq_tokio::Status::Connected) => {
                tracing::info!(relay = %relay_id, "the publisher's session with the relay is connected");
                session_state.send_replace(ExposedPortPublisherSessionState::Connected);
            }
            Ok(moq_tokio::Status::Migrating) => {}
            Ok(_) => {
                tracing::warn!(relay = %relay_id, "the publisher's session with the relay dropped; it is redialed");
                session_state.send_replace(ExposedPortPublisherSessionState::Redialing);
            }
            Err(ended) => {
                let why = ended.to_string();
                tracing::error!(relay = %relay_id, "the relay ended the publisher's session: {why}");
                session_state.send_replace(ExposedPortPublisherSessionState::Ended { why });
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_joined_relay_is_listed_joined_only_once_the_publishers_session_connects() {
        assert_eq!(
            ExposedPortPublisherSessionState::Dialing.as_listed_for_a_joined_relay(),
            ListedRelayConnectionState::Joining
        );
        assert_eq!(
            ExposedPortPublisherSessionState::Connected.as_listed_for_a_joined_relay(),
            ListedRelayConnectionState::Joined
        );
        assert!(matches!(
            ExposedPortPublisherSessionState::Redialing.as_listed_for_a_joined_relay(),
            ListedRelayConnectionState::Lost {
                rejoining_after_ms: 10_000,
                ..
            }
        ));
        assert_eq!(
            ExposedPortPublisherSessionState::Ended {
                why: "unauthorized".to_string()
            }
            .as_listed_for_a_joined_relay(),
            ListedRelayConnectionState::NotJoined {
                why: Some("the relay ended the publisher's session: unauthorized".to_string())
            }
        );
    }
}
