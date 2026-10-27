// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A relay adapter whose settings hold a secret, for the tests of how the
//! runtime keeps one. It joins nothing and serves no exposure level, so the
//! publisher puts no port at it. Compiled into a test build alone.

use streamlib_relay_adapter_sdk::url::Url;
use streamlib_relay_adapter_sdk::{
    BroadcastPath, JoinedRelay, MachineName, OutputPortExposureLevel, RelayAdapter,
    RelayAdapterCatalogEntry, RelayAdapterInstanceMaker, RelayAdapterKind, RelayAdapterRefusal,
    RelayConnectionState, RelayConnectionStateReceiver, RelayDialCredential, RelayInstanceId,
    RelaySecretSetting, RelaySettingsJsonSchema, relay_instance_settings_from_json,
};
use tokio::sync::watch;

/// A secret-holding instance's settings.
#[derive(serde::Deserialize, streamlib_relay_adapter_sdk::schemars::JsonSchema)]
#[schemars(crate = "streamlib_relay_adapter_sdk::schemars")]
#[serde(deny_unknown_fields)]
struct SecretHoldingTestRelaySettings {
    /// The secret the tests look for everywhere it must not be.
    #[allow(dead_code)]
    token: RelaySecretSetting,
    #[serde(default)]
    #[allow(dead_code)]
    label: Option<String>,
}

struct SecretHoldingTestRelayAdapter {
    relay_id: RelayInstanceId,
    connection_state: watch::Sender<RelayConnectionState>,
}

impl RelayAdapter for SecretHoldingTestRelayAdapter {
    fn kind() -> RelayAdapterKind {
        RelayAdapterKind("test-secret-holder")
    }

    fn doc() -> &'static str {
        "A test build's relay whose settings hold a secret; it joins nothing."
    }

    fn display_name() -> &'static str {
        "Secret-holding test relay"
    }

    fn display_icon_svg() -> &'static str {
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1 1"><rect/></svg>"#
    }

    fn relay_id(&self) -> &RelayInstanceId {
        &self.relay_id
    }

    fn settings_schema() -> serde_json::Value {
        SecretHoldingTestRelaySettings::processor_config_schema_document()
    }

    fn join(&mut self, settings: serde_json::Value) -> Result<JoinedRelay, RelayAdapterRefusal> {
        relay_instance_settings_from_json::<SecretHoldingTestRelaySettings>(
            &self.relay_id,
            settings,
        )?;
        self.connection_state
            .send_replace(RelayConnectionState::Joined);
        Ok(JoinedRelay {
            dial_url: Url::parse("http://127.0.0.1:9/").expect("a URL"),
        })
    }

    fn leave(&mut self) {
        self.connection_state
            .send_replace(RelayConnectionState::NotJoined);
    }

    fn levels_served(&self) -> &[OutputPortExposureLevel] {
        &[]
    }

    fn broadcast_root(&self, machine: &MachineName) -> BroadcastPath {
        BroadcastPath::of_the_machine_under_the_root("", machine)
    }

    fn credential_for_dial(&self) -> Option<RelayDialCredential> {
        None
    }

    fn events(&self) -> RelayConnectionStateReceiver {
        self.connection_state.subscribe()
    }
}

/// Makes the secret-holding test relay's instances.
pub(crate) struct SecretHoldingTestRelayAdapterInstanceMaker;

impl RelayAdapterInstanceMaker for SecretHoldingTestRelayAdapterInstanceMaker {
    fn catalog_entry(&self) -> RelayAdapterCatalogEntry {
        RelayAdapterCatalogEntry::of_the_adapter::<SecretHoldingTestRelayAdapter>()
    }

    fn make_the_instance(
        &self,
        relay_id: &RelayInstanceId,
    ) -> Result<Box<dyn RelayAdapter>, RelayAdapterRefusal> {
        Ok(Box::new(SecretHoldingTestRelayAdapter {
            relay_id: relay_id.clone(),
            connection_state: watch::channel(RelayConnectionState::NotJoined).0,
        }))
    }
}
