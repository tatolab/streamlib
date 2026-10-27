// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The interface every relay adapter implements. An adapter is one crate
//! against this one, compiled into `tatolabd`; adding a relay never edits the
//! daemon.

mod relay_instances_of_this_machine;
mod relay_secret_setting;

use std::fmt;
use std::time::{Duration, Instant};

use secrecy::{ExposeSecret, SecretString};
use serde::de::DeserializeOwned;
use streamlib_runtime_client_contract::local_api_wire_contract::ListedRelayConnectionState;
use url::Url;

pub use schemars;
pub use secrecy;
pub use streamlib::sdk::graph::OutputPortExposureLevel;
pub use streamlib::sdk::runtime::MachineName;
pub use streamlib_processor_schema::ProcessorConfigJsonSchema as RelaySettingsJsonSchema;
pub use url;

pub use relay_instances_of_this_machine::{
    BuiltInRelayInstance, RelayAdapterCatalogEntry, RelayAdapterInstanceMaker,
    RelayInstanceControlRefusal, RelayInstancesOfThisMachine, display_icon_svg_canvas_size,
};
pub use relay_secret_setting::{RelaySecretSetting, secret_setting_names_in_the_schema};

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

/// Where a [`RelayDialCredential`]'s token rides in the dial URL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RelayDialTokenPlacement {
    /// The [`RELAY_DIAL_TOKEN_QUERY_PARAMETER`] query parameter, the shape
    /// every moq-auth server reads.
    InTheQuery,
    /// One path segment after the dial URL's path, the shape a relay that
    /// reads the token from the path takes.
    AsThePathSegment,
}

/// The credential the runtime presents when it dials a relay: a token, where
/// it rides in the dial URL, and when it must be renewed, if ever.
#[derive(Clone)]
pub struct RelayDialCredential {
    token: SecretString,
    placement: RelayDialTokenPlacement,
    renew_by: Option<Instant>,
}

impl RelayDialCredential {
    /// `token` carried as the [`RELAY_DIAL_TOKEN_QUERY_PARAMETER`] query
    /// parameter.
    pub fn token_in_the_dial_url_query(token: SecretString) -> Self {
        Self {
            token,
            placement: RelayDialTokenPlacement::InTheQuery,
            renew_by: None,
        }
    }

    /// `token` carried as one path segment after the dial URL's path.
    pub fn token_as_the_dial_url_path_segment(token: SecretString) -> Self {
        Self {
            token,
            placement: RelayDialTokenPlacement::AsThePathSegment,
            renew_by: None,
        }
    }

    /// This credential, asked for again by `renew_by`: the adapter answers
    /// [`RelayAdapter::credential_for_dial`] with a fresh one then, and the
    /// runtime dials with it.
    pub fn renewed_by(mut self, renew_by: Instant) -> Self {
        self.renew_by = Some(renew_by);
        self
    }

    /// When this credential must be replaced by a fresh one, if ever.
    pub fn renew_by(&self) -> Option<Instant> {
        self.renew_by
    }

    /// `dial_url` carrying this credential. The whole URL is the secret once
    /// it carries a token.
    pub fn dial_url_carrying_the_credential(&self, dial_url: &Url) -> SecretString {
        let mut carrying = dial_url.clone();
        match self.placement {
            RelayDialTokenPlacement::InTheQuery => {
                carrying
                    .query_pairs_mut()
                    .append_pair(RELAY_DIAL_TOKEN_QUERY_PARAMETER, self.token.expose_secret());
            }
            RelayDialTokenPlacement::AsThePathSegment => {
                if let Ok(mut path_segments) = carrying.path_segments_mut() {
                    path_segments
                        .pop_if_empty()
                        .push(self.token.expose_secret());
                }
            }
        }
        SecretString::from(String::from(carrying))
    }
}

impl fmt::Debug for RelayDialCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RelayDialCredential")
            .field("token", &"[REDACTED]")
            .field("placement", &self.placement)
            .field("renew_by", &self.renew_by)
            .finish()
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

impl RelayConnectionState {
    /// This state as `list_relays` reports it.
    pub fn as_listed(&self) -> ListedRelayConnectionState {
        match self {
            Self::NotJoined => ListedRelayConnectionState::NotJoined { why: None },
            Self::Joining => ListedRelayConnectionState::Joining,
            Self::Joined => ListedRelayConnectionState::Joined,
            Self::Lost {
                why,
                rejoining_after,
            } => ListedRelayConnectionState::Lost {
                why: why.clone(),
                rejoining_after_ms: u64::try_from(rejoining_after.as_millis()).unwrap_or(u64::MAX),
            },
        }
    }
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

impl RelayAdapterRefusal {
    /// The settings of `relay_id` refused for `why`.
    pub fn settings_refused(relay_id: &RelayInstanceId, why: impl fmt::Display) -> Self {
        Self::SettingsRefused {
            relay_id: relay_id.clone(),
            why: why.to_string(),
        }
    }
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
    serde_json::from_value(settings)
        .map_err(|unreadable| RelayAdapterRefusal::settings_refused(relay_id, unreadable))
}

/// One relay instance, joined and left by `tatolabd`.
pub trait RelayAdapter: Send {
    /// The adapter this instance is joined through, known with no instance.
    fn kind() -> RelayAdapterKind
    where
        Self: Sized;

    /// What this adapter joins, one sentence, as the relay-adapter catalog
    /// lists it.
    fn doc() -> &'static str
    where
        Self: Sized;

    /// This adapter's name as people read it, such as `moq.pro`, as the
    /// relay-adapter catalog lists it.
    fn display_name() -> &'static str
    where
        Self: Sized;

    /// This adapter's icon, an SVG document of the provider's own logo on a
    /// square canvas, as the relay-adapter catalog lists it.
    fn display_icon_svg() -> &'static str
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
    /// dials with nothing; asked again once the credential's
    /// [`renew_by`](RelayDialCredential::renew_by) passes.
    fn credential_for_dial(&self) -> Option<RelayDialCredential>;

    /// This instance's connection state, as it changes.
    fn events(&self) -> RelayConnectionStateReceiver;

    /// How often each broadcast's catalog is written again whole at this
    /// relay, or `None` for a relay that hands a subscriber joining late the
    /// latest group: one that hands it only objects written after it
    /// subscribed would otherwise never show it a catalog left unchanged.
    fn the_catalog_is_rewritten_whole_every(&self) -> Option<Duration> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_token_rides_the_dial_urls_query_as_the_moq_auth_parameter() {
        let credential = RelayDialCredential::token_in_the_dial_url_query(SecretString::from(
            "abc 123".to_string(),
        ));
        let dial_url = Url::parse("http://127.0.0.1:4443/").unwrap();
        assert_eq!(
            credential
                .dial_url_carrying_the_credential(&dial_url)
                .expose_secret(),
            "http://127.0.0.1:4443/?jwt=abc+123"
        );
    }

    #[test]
    fn a_token_rides_as_one_path_segment_after_the_dial_urls_path() {
        let credential = RelayDialCredential::token_as_the_dial_url_path_segment(
            SecretString::from("Tk-123_abc".to_string()),
        );
        for dial_url in [
            "https://draft-16.relay.example/",
            "https://draft-16.relay.example",
        ] {
            assert_eq!(
                credential
                    .dial_url_carrying_the_credential(&Url::parse(dial_url).unwrap())
                    .expose_secret(),
                "https://draft-16.relay.example/Tk-123_abc"
            );
        }
    }

    #[test]
    fn a_credential_never_prints_its_token() {
        for credential in [
            RelayDialCredential::token_in_the_dial_url_query(SecretString::from(
                "abc123".to_string(),
            )),
            RelayDialCredential::token_as_the_dial_url_path_segment(SecretString::from(
                "abc123".to_string(),
            )),
        ] {
            assert!(!format!("{credential:?}").contains("abc123"));
        }
    }

    #[test]
    fn a_credential_names_when_it_is_renewed_or_none() {
        let token = || SecretString::from("abc".to_string());
        assert_eq!(
            RelayDialCredential::token_in_the_dial_url_query(token()).renew_by(),
            None
        );
        let renew_by = Instant::now() + Duration::from_secs(60);
        assert_eq!(
            RelayDialCredential::token_in_the_dial_url_query(token())
                .renewed_by(renew_by)
                .renew_by(),
            Some(renew_by)
        );
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
