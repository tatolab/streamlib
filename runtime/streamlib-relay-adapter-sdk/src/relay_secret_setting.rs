// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A setting that is a secret: an API key, a token, a signing key, or a URL
//! carrying one.

use std::fmt;
use std::marker::PhantomData;

use schemars::r#gen::SchemaGenerator;
use schemars::schema::{InstanceType, Schema, SchemaObject};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Deserializer};

use crate::relay_adapter_setting_field::{
    RELAY_SETTING_FIELD_TYPE_SCHEMA_KEYWORD, RELAY_SETTING_SECRET_SCHEMA_KEYWORD,
    RelaySettingFieldType,
};

/// How a person enters a [`RelaySecretSetting`]: the field type it is listed
/// as.
pub trait RelaySecretSettingEntry {
    /// The field type the relay-adapter catalog lists the secret as.
    fn field_type() -> RelaySettingFieldType;
}

/// A secret entered as one line of text, such as an API key.
pub struct RelaySecretEnteredAsString;

impl RelaySecretSettingEntry for RelaySecretEnteredAsString {
    fn field_type() -> RelaySettingFieldType {
        RelaySettingFieldType::String
    }
}

/// A secret entered as a URL, such as one whose path carries a token.
pub struct RelaySecretEnteredAsUrl;

impl RelaySecretSettingEntry for RelaySecretEnteredAsUrl {
    fn field_type() -> RelaySettingFieldType {
        RelaySettingFieldType::Url
    }
}

/// A secret entered as a file's text, such as a signing key.
pub struct RelaySecretEnteredAsFile;

impl RelaySecretSettingEntry for RelaySecretEnteredAsFile {
    fn field_type() -> RelaySettingFieldType {
        RelaySettingFieldType::File
    }
}

/// A secret field of an adapter's settings, entered as `Entry` says. The
/// relay-adapter catalog flags it secret, so the runtime keeps it in the OS
/// secret store and never in `relays.json`, and lists it only as set or
/// unset; it never prints.
pub struct RelaySecretSetting<Entry: RelaySecretSettingEntry> {
    secret: SecretString,
    entered_as: PhantomData<fn() -> Entry>,
}

impl<Entry: RelaySecretSettingEntry> RelaySecretSetting<Entry> {
    /// The secret, for the one call that hands it to the provider.
    pub fn expose_secret(&self) -> &str {
        self.secret.expose_secret()
    }

    /// The secret as the [`SecretString`] it is held in.
    pub fn as_secret_string(&self) -> &SecretString {
        &self.secret
    }
}

impl<Entry: RelaySecretSettingEntry> Clone for RelaySecretSetting<Entry> {
    fn clone(&self) -> Self {
        Self::from(self.secret.clone())
    }
}

impl<Entry: RelaySecretSettingEntry> From<SecretString> for RelaySecretSetting<Entry> {
    fn from(secret: SecretString) -> Self {
        Self {
            secret,
            entered_as: PhantomData,
        }
    }
}

impl<Entry: RelaySecretSettingEntry> fmt::Debug for RelaySecretSetting<Entry> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RelaySecretSetting([REDACTED])")
    }
}

impl<'de, Entry: RelaySecretSettingEntry> Deserialize<'de> for RelaySecretSetting<Entry> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(|secret| Self::from(SecretString::from(secret)))
    }
}

impl<Entry: RelaySecretSettingEntry> schemars::JsonSchema for RelaySecretSetting<Entry> {
    fn is_referenceable() -> bool {
        false
    }

    fn schema_name() -> String {
        "RelaySecretSetting".to_owned()
    }

    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        let mut schema = SchemaObject {
            instance_type: Some(InstanceType::String.into()),
            ..Default::default()
        };
        schema.extensions.insert(
            RELAY_SETTING_FIELD_TYPE_SCHEMA_KEYWORD.to_string(),
            serde_json::to_value(Entry::field_type())
                .expect("a relay setting field type always serializes"),
        );
        schema.extensions.insert(
            RELAY_SETTING_SECRET_SCHEMA_KEYWORD.to_string(),
            serde_json::Value::Bool(true),
        );
        schema.into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Deserialize)]
    struct ProviderSettings {
        relay_url_carrying_a_token: RelaySecretSetting<RelaySecretEnteredAsUrl>,
        #[allow(dead_code)]
        region: String,
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
