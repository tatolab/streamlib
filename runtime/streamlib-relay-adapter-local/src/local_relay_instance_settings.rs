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
        RelayInstanceId, RelaySettingFieldType, relay_adapter_setting_fields_of,
        relay_instance_settings_from_json,
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
    fn the_port_alone_is_a_setting_an_optional_number() {
        let fields = relay_adapter_setting_fields_of::<LocalRelayInstanceSettings>();
        let [port] = fields.as_slice() else {
            panic!("{fields:#?}")
        };
        assert_eq!(port.name, "port");
        assert_eq!(port.field_type, RelaySettingFieldType::Number);
        assert!(!port.secret);
        assert!(!port.required);
    }
}
