// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The OS secret store relay secrets are kept in — the macOS Keychain, the
//! Secret Service on Linux — one item per relay instance and setting under the
//! `tatolab` service. A test build never links it.

use std::sync::Arc;

use streamlib_relay_adapter_sdk::secrecy::{ExposeSecret, SecretString};

use crate::relay_secret_store::RelaySecretStore;

/// The service every relay secret is filed under in the OS secret store.
const RELAY_SECRET_STORE_SERVICE_NAME: &str = "tatolab";

/// The OS secret store, once a read of an item no relay holds is answered.
pub(crate) fn the_os_secret_store_answering_now() -> Result<Box<dyn RelaySecretStore>, String> {
    let os_secret_store = OsRelaySecretStore {
        credential_store: this_platforms_credential_store()?,
    };
    os_secret_store.read("tatolab", "a-secret-no-relay-holds")?;
    Ok(Box::new(os_secret_store))
}

#[cfg(target_os = "linux")]
fn this_platforms_credential_store() -> Result<Arc<keyring_core::CredentialStore>, String> {
    zbus_secret_service_keyring_store::Store::new()
        .map(|store| store as Arc<keyring_core::CredentialStore>)
        .map_err(|refusal| format!("the Secret Service did not answer: {refusal}"))
}

#[cfg(target_os = "macos")]
fn this_platforms_credential_store() -> Result<Arc<keyring_core::CredentialStore>, String> {
    apple_native_keyring_store::keychain::Store::new()
        .map(|store| store as Arc<keyring_core::CredentialStore>)
        .map_err(|refusal| format!("the login keychain did not answer: {refusal}"))
}

/// The OS secret store, one item per relay instance and setting.
struct OsRelaySecretStore {
    credential_store: Arc<keyring_core::CredentialStore>,
}

impl OsRelaySecretStore {
    fn entry(&self, relay_id: &str, setting: &str) -> Result<keyring_core::Entry, String> {
        self.credential_store
            .build(
                RELAY_SECRET_STORE_SERVICE_NAME,
                &format!("{relay_id}/{setting}"),
                None,
            )
            .map_err(|refusal| refusal.to_string())
    }
}

impl RelaySecretStore for OsRelaySecretStore {
    fn read(&self, relay_id: &str, setting: &str) -> Result<Option<SecretString>, String> {
        match self.entry(relay_id, setting)?.get_password() {
            Ok(secret) => Ok(Some(SecretString::from(secret))),
            Err(keyring_core::Error::NoEntry) => Ok(None),
            Err(refusal) => Err(refusal.to_string()),
        }
    }

    fn write(&self, relay_id: &str, setting: &str, secret: &SecretString) -> Result<(), String> {
        self.entry(relay_id, setting)?
            .set_password(secret.expose_secret())
            .map_err(|refusal| refusal.to_string())
    }

    fn delete(&self, relay_id: &str, setting: &str) -> Result<(), String> {
        match self.entry(relay_id, setting)?.delete_credential() {
            Ok(()) | Err(keyring_core::Error::NoEntry) => Ok(()),
            Err(refusal) => Err(refusal.to_string()),
        }
    }
}
