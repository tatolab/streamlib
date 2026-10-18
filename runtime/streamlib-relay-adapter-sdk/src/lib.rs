// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The interface every relay adapter implements. An adapter is one crate
//! against this one, compiled into `tatolabd`; adding a relay never edits the
//! daemon.

use std::fmt;
use std::time::Duration;

use secrecy::{ExposeSecret, SecretString};
use serde::de::DeserializeOwned;
use url::Url;

pub use schemars;
pub use secrecy;
pub use streamlib::sdk::graph::OutputPortExposureLevel;
pub use streamlib::sdk::runtime::MachineName;
pub use streamlib_processor_schema::ProcessorConfigJsonSchema as RelaySettingsJsonSchema;
pub use url;

/// The query parameter a moq-auth server reads a session's token from.
pub const RELAY_DIAL_TOKEN_QUERY_PARAMETER: &str = "jwt";

/// Which adapter a relay instance is joined through, such as `local`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RelayAdapterKind(pub &'static str);

impl fmt::Display for RelayAdapterKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

/// One relay instance's id, unique among the instances `tatolabd` holds.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RelayInstanceId(pub String);

impl fmt::Display for RelayInstanceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The path at a relay every broadcast of one machine is named under:
/// `<root>/<machine>`, the root being whatever the relay assigned or the
/// adapter names, possibly empty.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BroadcastPath(String);

impl BroadcastPath {
    /// The machine's broadcasts under `root`; an empty root names them at the
    /// relay's top level.
    pub fn of_the_machine_under_the_root(root: &str, machine: &MachineName) -> Self {
        let root = root.trim_matches('/');
        if root.is_empty() {
            Self(machine.name().to_string())
        } else {
            Self(format!("{root}/{}", machine.name()))
        }
    }

    /// The path, with no leading or trailing `/`.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for BroadcastPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// What a joined relay hands the runtime: where to dial it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JoinedRelay {
    /// The relay's URL, carrying no credential.
    pub dial_url: Url,
}

/// The credential the runtime presents when it dials a relay.
pub enum RelayDialCredential {
    /// A token carried as the [`RELAY_DIAL_TOKEN_QUERY_PARAMETER`] query
    /// parameter, the shape every moq-auth server reads.
    TokenInTheDialUrlQuery(SecretString),
}

impl RelayDialCredential {
    /// `dial_url` carrying this credential. The whole URL is the secret once
    /// it carries a token.
    pub fn dial_url_carrying_the_credential(&self, dial_url: &Url) -> SecretString {
        match self {
            Self::TokenInTheDialUrlQuery(token) => {
                let mut carrying = dial_url.clone();
                carrying
                    .query_pairs_mut()
                    .append_pair(RELAY_DIAL_TOKEN_QUERY_PARAMETER, token.expose_secret());
                SecretString::from(String::from(carrying))
            }
        }
    }
}

impl fmt::Debug for RelayDialCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TokenInTheDialUrlQuery(_) => f.write_str("TokenInTheDialUrlQuery([REDACTED])"),
        }
    }
}

/// Where a relay instance stands, as its adapter last reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelayConnectionState {
    /// Not joined, or left.
    NotJoined,
    /// Joining, with nothing yet to dial.
    Joining,
    /// Joined: the relay answers at the dial URL the join returned.
    Joined,
    /// Lost after joining, and joining again after `rejoining_after`.
    Lost {
        why: String,
        rejoining_after: Duration,
    },
}

/// The latest [`RelayConnectionState`] of one relay instance, updated as it
/// changes.
pub type RelayConnectionStateReceiver = tokio::sync::watch::Receiver<RelayConnectionState>;

/// Why a relay instance could not be joined.
#[derive(Debug, thiserror::Error)]
pub enum RelayAdapterRefusal {
    /// The instance's settings are not the adapter's settings.
    #[error("the settings of the relay `{relay_id}` are refused: {why}")]
    SettingsRefused {
        relay_id: RelayInstanceId,
        why: String,
    },
    /// The relay could not be joined.
    #[error("the relay `{relay_id}` could not be joined: {why}")]
    JoinFailed {
        relay_id: RelayInstanceId,
        why: String,
    },
}

/// An adapter's settings type: read from an instance's JSON, described by its
/// JSON Schema.
pub trait RelayInstanceSettings:
    DeserializeOwned + RelaySettingsJsonSchema + Send + 'static
{
}

impl<T: DeserializeOwned + RelaySettingsJsonSchema + Send + 'static> RelayInstanceSettings for T {}

/// `settings` read as `S`, or refused naming `relay_id`.
pub fn relay_instance_settings_from_json<S: RelayInstanceSettings>(
    relay_id: &RelayInstanceId,
    settings: serde_json::Value,
) -> Result<S, RelayAdapterRefusal> {
    serde_json::from_value(settings).map_err(|unreadable| RelayAdapterRefusal::SettingsRefused {
        relay_id: relay_id.clone(),
        why: unreadable.to_string(),
    })
}

/// One relay instance, joined and left by `tatolabd`.
pub trait RelayAdapter: Send {
    /// The adapter this instance is joined through, known with no instance.
    fn kind() -> RelayAdapterKind
    where
        Self: Sized;

    /// This instance's id.
    fn relay_id(&self) -> &RelayInstanceId;

    /// The JSON Schema of the settings [`join`](Self::join) reads, known with
    /// no instance.
    fn settings_schema() -> serde_json::Value
    where
        Self: Sized;

    /// Join the relay with `settings`, returning where it is dialed.
    fn join(&mut self, settings: serde_json::Value) -> Result<JoinedRelay, RelayAdapterRefusal>;

    /// Leave the relay; nothing of this instance runs afterwards.
    fn leave(&mut self);

    /// The exposure levels whose ports this relay carries.
    fn levels_served(&self) -> &[OutputPortExposureLevel];

    /// Where `machine`'s broadcasts are named at this relay.
    fn broadcast_root(&self, machine: &MachineName) -> BroadcastPath;

    /// What the runtime presents when it dials this relay, or `None` when it
    /// dials with nothing.
    fn credential_for_dial(&self) -> Option<RelayDialCredential>;

    /// This instance's connection state, as it changes.
    fn events(&self) -> RelayConnectionStateReceiver;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_token_rides_the_dial_urls_query_as_the_moq_auth_parameter() {
        let credential =
            RelayDialCredential::TokenInTheDialUrlQuery(SecretString::from("abc 123".to_string()));
        let dial_url = Url::parse("http://127.0.0.1:4443/").unwrap();
        assert_eq!(
            credential
                .dial_url_carrying_the_credential(&dial_url)
                .expose_secret(),
            "http://127.0.0.1:4443/?jwt=abc+123"
        );
    }

    #[test]
    fn a_credential_never_prints_its_token() {
        let credential =
            RelayDialCredential::TokenInTheDialUrlQuery(SecretString::from("abc123".to_string()));
        assert!(!format!("{credential:?}").contains("abc123"));
    }

    #[test]
    fn a_machines_broadcasts_sit_under_the_root_or_at_the_top_level() {
        let machine = MachineName::from_a_hostname("Desk").unwrap();
        assert_eq!(
            BroadcastPath::of_the_machine_under_the_root("", &machine).as_str(),
            "desk"
        );
        assert_eq!(
            BroadcastPath::of_the_machine_under_the_root("/team/a/", &machine).as_str(),
            "team/a/desk"
        );
    }

    #[derive(serde::Deserialize, schemars::JsonSchema)]
    #[serde(deny_unknown_fields)]
    struct PortOnlySettings {
        #[allow(dead_code)]
        port: u16,
    }

    #[test]
    fn settings_that_are_not_the_adapters_are_refused_naming_the_relay() {
        let relay_id = RelayInstanceId("local-loopback".to_string());
        let refusal = relay_instance_settings_from_json::<PortOnlySettings>(
            &relay_id,
            serde_json::json!({"port": "4443"}),
        )
        .err()
        .unwrap();
        assert!(
            refusal.to_string().contains("`local-loopback`"),
            "{refusal}"
        );
    }
}
