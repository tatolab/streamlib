// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! What a local relay instance's settings may say.

use serde::Deserialize;

/// A local relay instance's settings. Its bind address is never one.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LocalRelayInstanceSettings {
    /// The port the listener takes for QUIC and HTTP alike; absent, the
    /// listener's own default (4443 at loopback, 4445 at every interface).
    #[serde(default)]
    pub port: Option<u16>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use streamlib_relay_adapter_sdk::{
        RelayInstanceId, RelaySettingsJsonSchema, relay_instance_settings_from_json,
    };

    fn relay_id() -> RelayInstanceId {
        RelayInstanceId("local-loopback".to_string())
    }

    #[test]
    fn no_settings_take_the_listeners_default_port() {
        let settings: LocalRelayInstanceSettings =
            relay_instance_settings_from_json(&relay_id(), serde_json::json!({})).unwrap();
        assert_eq!(settings.port, None);
    }

    #[test]
    fn a_bind_address_is_not_a_setting() {
        let refusal = relay_instance_settings_from_json::<LocalRelayInstanceSettings>(
            &relay_id(),
            serde_json::json!({"bind": "0.0.0.0:4443"}),
        )
        .err()
        .unwrap();
        assert!(refusal.to_string().contains("bind"), "{refusal}");
    }

    #[test]
    fn the_schema_describes_the_port_alone() {
        let schema = LocalRelayInstanceSettings::processor_config_schema_document();
        let properties = schema["properties"].as_object().unwrap();
        assert_eq!(properties.keys().collect::<Vec<_>>(), vec!["port"]);
    }
}
