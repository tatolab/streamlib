// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Cloudflare's hosted MoQ relays. Each instance is one relay Cloudflare
//! provisioned, dialed at the publish-and-subscribe URL Cloudflare returns for
//! it. Its token rides that URL's path, so the whole URL is the instance's one
//! secret; the adapter splits it into the relay's address and the token, and
//! the token is presented only in the URL the runtime dials. Cloudflare
//! assigns a machine no root, so its broadcasts sit at the relay's top level.
//! A token is replaced through `configure_relay`; minting one takes an account
//! API token, which the runtime never holds.

use std::time::Duration;

use streamlib_relay_adapter_sdk::secrecy::SecretString;
use streamlib_relay_adapter_sdk::url::Url;
use streamlib_relay_adapter_sdk::{
    BroadcastPath, JoinedRelay, MachineName, OutputPortExposureLevel, RelayAdapter,
    RelayAdapterCatalogEntry, RelayAdapterInstanceMaker, RelayAdapterKind, RelayAdapterRefusal,
    RelayConnectionState, RelayConnectionStateReceiver, RelayDialCredential, RelayInstanceId,
    RelaySecretSetting, RelaySettingsJsonSchema, relay_instance_settings_from_json,
};
use tokio::sync::watch;

/// The adapter kind every Cloudflare relay instance is joined through.
pub const CLOUDFLARE_RELAY_ADAPTER_KIND: RelayAdapterKind = RelayAdapterKind("cloudflare");

/// How often a broadcast's catalog is written again whole at a Cloudflare
/// relay. Its relays speak the IETF drafts, where a subscriber receives only
/// objects written after it subscribed, so a player joining late reads the
/// catalog within this.
const CLOUDFLARE_CATALOG_IS_REWRITTEN_WHOLE_EVERY: Duration = Duration::from_secs(1);

/// A Cloudflare relay serves public ports alone.
const LEVELS_A_CLOUDFLARE_RELAY_SERVES: [OutputPortExposureLevel; 1] =
    [OutputPortExposureLevel::Public];

/// A Cloudflare relay instance's settings.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct CloudflareRelayInstanceSettings {
    /// The relay's publish-and-subscribe URL as Cloudflare returns it,
    /// `https://draft-16.cloudflare.mediaoverquic.com/<token>`; its token rides
    /// the path, so the whole URL is a secret.
    pub(crate) relay_url: RelaySecretSetting,
}

/// A Cloudflare relay URL read as the relay's address and the token in its
/// path.
pub(crate) struct CloudflareRelayAddressAndToken {
    /// The relay's URL with the token taken off its path.
    pub(crate) dial_url: Url,
    /// The token the URL's path carried.
    pub(crate) token: SecretString,
}

impl std::fmt::Debug for CloudflareRelayAddressAndToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CloudflareRelayAddressAndToken")
            .field("dial_url", &self.dial_url.as_str())
            .field("token", &"[REDACTED]")
            .finish()
    }
}

/// Why a Cloudflare relay URL was refused; it never names the URL, which is
/// the secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CloudflareRelayUrlRefusal {
    /// It does not parse as a URL.
    NotAUrl,
    /// Its scheme is not `https`.
    NotHttps,
    /// Its path carries no segment to read as the token.
    NoTokenInThePath,
    /// It carries a query or a fragment.
    CarriesAQueryOrAFragment,
}

impl std::fmt::Display for CloudflareRelayUrlRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::NotAUrl => "`relay_url` is not a URL",
            Self::NotHttps => "`relay_url` is not an `https://` URL",
            Self::NoTokenInThePath => {
                "`relay_url` carries no token in its path; it is the relay's \
                 publish-and-subscribe URL Cloudflare returned, its token the last path segment"
            }
            Self::CarriesAQueryOrAFragment => {
                "`relay_url` carries a query or a fragment, which a Cloudflare relay URL never does"
            }
        })
    }
}

impl CloudflareRelayAddressAndToken {
    /// `relay_url` read as the relay's address and its token, the last path
    /// segment.
    pub(crate) fn read_from(relay_url: &str) -> Result<Self, CloudflareRelayUrlRefusal> {
        let mut dial_url =
            Url::parse(relay_url.trim()).map_err(|_| CloudflareRelayUrlRefusal::NotAUrl)?;
        if dial_url.scheme() != "https" {
            return Err(CloudflareRelayUrlRefusal::NotHttps);
        }
        if dial_url.query().is_some() || dial_url.fragment().is_some() {
            return Err(CloudflareRelayUrlRefusal::CarriesAQueryOrAFragment);
        }
        let token = dial_url
            .path_segments()
            .and_then(|mut path_segments| path_segments.rfind(|segment| !segment.is_empty()))
            .map(str::to_string)
            .ok_or(CloudflareRelayUrlRefusal::NoTokenInThePath)?;
        dial_url
            .path_segments_mut()
            .map_err(|_| CloudflareRelayUrlRefusal::NoTokenInThePath)?
            .pop_if_empty()
            .pop();
        Ok(Self {
            dial_url,
            token: SecretString::from(token),
        })
    }
}

/// One Cloudflare relay.
pub struct CloudflareRelayAdapter {
    relay_id: RelayInstanceId,
    connection_state: watch::Sender<RelayConnectionState>,
    token: Option<SecretString>,
}

impl CloudflareRelayAdapter {
    /// The instance `relay_id`, not yet joined.
    pub fn new(relay_id: RelayInstanceId) -> Self {
        Self {
            relay_id,
            connection_state: watch::channel(RelayConnectionState::NotJoined).0,
            token: None,
        }
    }
}

impl RelayAdapter for CloudflareRelayAdapter {
    fn kind() -> RelayAdapterKind {
        CLOUDFLARE_RELAY_ADAPTER_KIND
    }

    fn doc() -> &'static str {
        "A relay Cloudflare provisioned, dialed at its publish-and-subscribe URL, which carries \
         its token and is kept whole as a secret. Serves public ports, at the relay's top level."
    }

    fn relay_id(&self) -> &RelayInstanceId {
        &self.relay_id
    }

    fn settings_schema() -> serde_json::Value {
        CloudflareRelayInstanceSettings::processor_config_schema_document()
    }

    fn join(&mut self, settings: serde_json::Value) -> Result<JoinedRelay, RelayAdapterRefusal> {
        let settings: CloudflareRelayInstanceSettings =
            relay_instance_settings_from_json(&self.relay_id, settings)?;
        let address_and_token = CloudflareRelayAddressAndToken::read_from(
            settings.relay_url.expose_secret(),
        )
        .map_err(|refusal| RelayAdapterRefusal::settings_refused(&self.relay_id, refusal))?;
        self.token = Some(address_and_token.token);
        self.connection_state
            .send_replace(RelayConnectionState::Joined);
        Ok(JoinedRelay {
            dial_url: address_and_token.dial_url,
        })
    }

    fn leave(&mut self) {
        self.token = None;
        self.connection_state
            .send_replace(RelayConnectionState::NotJoined);
    }

    fn levels_served(&self) -> &[OutputPortExposureLevel] {
        &LEVELS_A_CLOUDFLARE_RELAY_SERVES
    }

    fn broadcast_root(&self, machine: &MachineName) -> BroadcastPath {
        BroadcastPath::of_the_machine_under_the_root("", machine)
    }

    fn credential_for_dial(&self) -> Option<RelayDialCredential> {
        self.token
            .clone()
            .map(RelayDialCredential::token_as_the_dial_url_path_segment)
    }

    fn events(&self) -> RelayConnectionStateReceiver {
        self.connection_state.subscribe()
    }

    fn the_catalog_is_rewritten_whole_every(&self) -> Option<Duration> {
        Some(CLOUDFLARE_CATALOG_IS_REWRITTEN_WHOLE_EVERY)
    }
}

/// Makes Cloudflare relay instances, as many as are added.
pub struct CloudflareRelayAdapterInstanceMaker;

impl RelayAdapterInstanceMaker for CloudflareRelayAdapterInstanceMaker {
    fn catalog_entry(&self) -> RelayAdapterCatalogEntry {
        RelayAdapterCatalogEntry::of_the_adapter::<CloudflareRelayAdapter>()
    }

    fn make_the_instance(
        &self,
        relay_id: &RelayInstanceId,
    ) -> Result<Box<dyn RelayAdapter>, RelayAdapterRefusal> {
        Ok(Box::new(CloudflareRelayAdapter::new(relay_id.clone())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use streamlib_relay_adapter_sdk::secrecy::ExposeSecret;

    const TOKEN: &str = "cf0Tk-7d2e_91ab";

    fn relay_url() -> String {
        format!("https://draft-16.cloudflare.mediaoverquic.com/{TOKEN}")
    }

    fn joined_instance() -> (CloudflareRelayAdapter, JoinedRelay) {
        let mut adapter = CloudflareRelayAdapter::new(RelayInstanceId("cloudflare-1".to_string()));
        let joined = adapter
            .join(serde_json::json!({ "relay_url": relay_url() }))
            .unwrap();
        (adapter, joined)
    }

    #[test]
    fn the_schema_takes_the_relay_url_alone_as_a_secret() {
        let entry = CloudflareRelayAdapterInstanceMaker.catalog_entry();
        assert_eq!(entry.kind, "cloudflare");
        let properties = entry.settings_schema["properties"].as_object().unwrap();
        assert_eq!(properties.keys().collect::<Vec<_>>(), ["relay_url"]);
        assert_eq!(entry.secret_setting_names(), ["relay_url"]);
        assert_eq!(
            entry.settings_schema["required"],
            serde_json::json!(["relay_url"])
        );
    }

    #[test]
    fn the_token_is_taken_off_the_path_and_presented_as_it_again() {
        let (adapter, joined) = joined_instance();
        assert_eq!(
            joined.dial_url.as_str(),
            "https://draft-16.cloudflare.mediaoverquic.com/"
        );
        assert!(!joined.dial_url.as_str().contains(TOKEN));
        let credential = adapter.credential_for_dial().unwrap();
        assert_eq!(credential.renew_by(), None);
        assert_eq!(
            credential
                .dial_url_carrying_the_credential(&joined.dial_url)
                .expose_secret(),
            relay_url()
        );
        assert_eq!(*adapter.events().borrow(), RelayConnectionState::Joined);
    }

    #[test]
    fn a_trailing_slash_or_a_deeper_path_keeps_the_last_segment_as_the_token() {
        let read = CloudflareRelayAddressAndToken::read_from(&format!("{}/", relay_url())).unwrap();
        assert_eq!(read.token.expose_secret(), TOKEN);
        let read = CloudflareRelayAddressAndToken::read_from(&format!(
            "https://relay.example/moq/{TOKEN}"
        ))
        .unwrap();
        assert_eq!(read.dial_url.as_str(), "https://relay.example/moq");
        assert_eq!(read.token.expose_secret(), TOKEN);
    }

    #[test]
    fn a_url_that_is_no_cloudflare_relay_url_is_refused_without_naming_it() {
        for (relay_url, refusal) in [
            (
                format!("{TOKEN} is no url"),
                CloudflareRelayUrlRefusal::NotAUrl,
            ),
            (
                format!("http://relay.example/{TOKEN}"),
                CloudflareRelayUrlRefusal::NotHttps,
            ),
            (
                "https://draft-16.cloudflare.mediaoverquic.com/".to_string(),
                CloudflareRelayUrlRefusal::NoTokenInThePath,
            ),
            (
                format!("https://relay.example/{TOKEN}?jwt={TOKEN}"),
                CloudflareRelayUrlRefusal::CarriesAQueryOrAFragment,
            ),
        ] {
            assert_eq!(
                CloudflareRelayAddressAndToken::read_from(&relay_url).unwrap_err(),
                refusal
            );
            let mut adapter =
                CloudflareRelayAdapter::new(RelayInstanceId("cloudflare-1".to_string()));
            let refused = adapter
                .join(serde_json::json!({ "relay_url": relay_url }))
                .err()
                .unwrap()
                .to_string();
            assert!(refused.contains("`cloudflare-1`"), "{refused}");
            assert!(!refused.contains(TOKEN), "{refused}");
            assert!(adapter.credential_for_dial().is_none());
        }
    }

    #[test]
    fn neither_the_settings_nor_the_split_url_ever_print_the_token() {
        let settings: CloudflareRelayInstanceSettings =
            serde_json::from_value(serde_json::json!({ "relay_url": relay_url() })).unwrap();
        assert!(!format!("{settings:?}").contains(TOKEN));
        let read = CloudflareRelayAddressAndToken::read_from(&relay_url()).unwrap();
        assert!(!format!("{read:?}").contains(TOKEN));
        let (adapter, _) = joined_instance();
        assert!(!format!("{:?}", adapter.credential_for_dial().unwrap()).contains(TOKEN));
    }

    #[test]
    fn it_serves_public_ports_at_the_relays_top_level_and_forgets_its_token_on_leaving() {
        let (mut adapter, _) = joined_instance();
        assert_eq!(adapter.levels_served(), [OutputPortExposureLevel::Public]);
        let machine = MachineName::from_a_hostname("Desk").unwrap();
        assert_eq!(adapter.broadcast_root(&machine).as_str(), "desk");
        adapter.leave();
        assert!(adapter.credential_for_dial().is_none());
        assert_eq!(*adapter.events().borrow(), RelayConnectionState::NotJoined);
    }

    #[test]
    fn its_catalog_is_written_again_whole_every_second() {
        let (adapter, _) = joined_instance();
        assert_eq!(
            adapter.the_catalog_is_rewritten_whole_every(),
            Some(Duration::from_secs(1))
        );
    }

    #[test]
    fn a_setting_it_does_not_take_is_refused_naming_it() {
        let mut adapter = CloudflareRelayAdapter::new(RelayInstanceId("cloudflare-1".to_string()));
        let refused = adapter
            .join(serde_json::json!({ "relay_url": relay_url(), "region": "us" }))
            .err()
            .unwrap()
            .to_string();
        assert!(refused.contains("region"), "{refused}");
    }
}
