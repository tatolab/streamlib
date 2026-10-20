// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A setting that is a secret: an API key, a token, a signing key, or a URL
//! carrying one.

use std::fmt;

use schemars::r#gen::SchemaGenerator;
use schemars::schema::{InstanceType, Metadata, Schema, SchemaObject};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Deserializer};

/// A secret field of an adapter's settings. Its schema is a `writeOnly`
/// string, so the runtime keeps it in the OS secret store and never in
/// `relays.json`, and lists it only as set or unset; it never prints.
#[derive(Clone)]
pub struct RelaySecretSetting(SecretString);

impl RelaySecretSetting {
    /// The secret, for the one call that hands it to the provider.
    pub fn expose_secret(&self) -> &str {
        self.0.expose_secret()
    }

    /// The secret as the [`SecretString`] it is held in.
    pub fn as_secret_string(&self) -> &SecretString {
        &self.0
    }
}

impl From<SecretString> for RelaySecretSetting {
    fn from(secret: SecretString) -> Self {
        Self(secret)
    }
}

impl fmt::Debug for RelaySecretSetting {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RelaySecretSetting([REDACTED])")
    }
}

impl<'de> Deserialize<'de> for RelaySecretSetting {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(|secret| Self(SecretString::from(secret)))
    }
}

impl schemars::JsonSchema for RelaySecretSetting {
    fn is_referenceable() -> bool {
        false
    }

    fn schema_name() -> String {
        "RelaySecretSetting".to_owned()
    }

    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        SchemaObject {
            instance_type: Some(InstanceType::String.into()),
            metadata: Some(Box::new(Metadata {
                write_only: true,
                ..Default::default()
            })),
            ..Default::default()
        }
        .into()
    }
}

/// The settings named by `settings_schema` whose schema is `writeOnly`: the
/// secrets.
pub fn secret_setting_names_in_the_schema(settings_schema: &serde_json::Value) -> Vec<String> {
    settings_schema["properties"]
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(_, property_schema)| {
            property_schema["writeOnly"] == serde_json::Value::Bool(true)
        })
        .map(|(setting_name, _)| setting_name.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RelaySettingsJsonSchema;

    /// A provider's settings: an endpoint whose URL carries a token, an
    /// optional key, and a region that is no secret.
    #[derive(Debug, Deserialize, schemars::JsonSchema)]
    #[serde(deny_unknown_fields)]
    struct ProviderSettings {
        /// The relay's URL, its token in the path.
        relay_url_carrying_a_token: RelaySecretSetting,
        #[serde(default)]
        #[allow(dead_code)]
        signing_key: Option<RelaySecretSetting>,
        #[serde(default)]
        #[allow(dead_code)]
        region: Option<String>,
    }

    #[test]
    fn each_secret_field_and_no_other_is_write_only_in_the_schema() {
        let schema = ProviderSettings::processor_config_schema_document();
        let mut secret_names = secret_setting_names_in_the_schema(&schema);
        secret_names.sort();
        assert_eq!(
            secret_names,
            ["relay_url_carrying_a_token", "signing_key"],
            "{schema:#}"
        );
        assert_eq!(
            schema["properties"]["relay_url_carrying_a_token"]["description"],
            "The relay's URL, its token in the path."
        );
    }

    #[test]
    fn a_url_carrying_a_token_never_prints() {
        let settings: ProviderSettings = serde_json::from_value(serde_json::json!({
            "relay_url_carrying_a_token": "https://relay.example/4f9c2token/",
            "region": "us"
        }))
        .unwrap();
        let printed = format!("{settings:?}");
        assert!(!printed.contains("4f9c2token"), "{printed}");
        assert!(!printed.contains("relay.example"), "{printed}");
        assert_eq!(
            settings.relay_url_carrying_a_token.expose_secret(),
            "https://relay.example/4f9c2token/"
        );
    }
}
