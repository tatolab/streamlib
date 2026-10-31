// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The OS secret store relay secrets are kept in — the macOS data-protection
//! keychain, the Secret Service on Linux — one item per relay instance and
//! setting under the `tatolab` service. A test build never links it.

use std::collections::HashMap;
use std::sync::Arc;

use streamlib_relay_adapter_sdk::secrecy::{ExposeSecret, SecretString};

use crate::relay_secret_store::RelaySecretStore;

/// The service every relay secret is filed under in the OS secret store.
const RELAY_SECRET_STORE_SERVICE_NAME: &str = "tatolab";

/// The relay id and setting of the item written and deleted again to learn
/// whether the OS secret store answers.
const OS_SECRET_STORE_PROBE_RELAY_ID: &str = "tatolab";
const OS_SECRET_STORE_PROBE_SETTING: &str = "a-secret-no-relay-holds";

/// The OS secret store, once an item no relay holds is written to it and
/// deleted again. A write is the probe because the data-protection keychain
/// can answer a read for a program not entitled to it as if the item were
/// absent, then refuse its writes.
pub(crate) fn the_os_secret_store_answering_now() -> Result<Box<dyn RelaySecretStore>, String> {
    let os_secret_store = OsRelaySecretStore {
        credential_store: this_platforms_credential_store()?,
    };
    os_secret_store.write(
        OS_SECRET_STORE_PROBE_RELAY_ID,
        OS_SECRET_STORE_PROBE_SETTING,
        &SecretString::from("a probe no relay reads".to_string()),
    )?;
    os_secret_store.delete(
        OS_SECRET_STORE_PROBE_RELAY_ID,
        OS_SECRET_STORE_PROBE_SETTING,
    )?;
    Ok(Box::new(os_secret_store))
}

#[cfg(target_os = "linux")]
fn this_platforms_credential_store() -> Result<Arc<keyring_core::CredentialStore>, String> {
    zbus_secret_service_keyring_store::Store::new()
        .map(|store| store as Arc<keyring_core::CredentialStore>)
        .map_err(|refusal| format!("the Secret Service did not answer: {refusal}"))
}

#[cfg(target_os = "linux")]
fn this_platforms_entry_modifiers() -> Option<HashMap<&'static str, &'static str>> {
    None
}

/// The data-protection keychain, in the default keychain access group of the
/// runtime's signature: macOS grants it to the signed runtime by its
/// entitlement and never shows a prompt, which the file-based login keychain
/// does for any program but the one that wrote an item.
#[cfg(target_os = "macos")]
fn this_platforms_credential_store() -> Result<Arc<keyring_core::CredentialStore>, String> {
    apple_native_keyring_store::protected::Store::new()
        .map(|store| store as Arc<keyring_core::CredentialStore>)
        .map_err(|refusal| format!("the data-protection keychain did not answer: {refusal}"))
}

/// Readable by the runtime, a launch agent, while the screen is locked, and
/// never carried to another Mac by a backup or iCloud.
#[cfg(target_os = "macos")]
fn this_platforms_entry_modifiers() -> Option<HashMap<&'static str, &'static str>> {
    Some(HashMap::from([(
        "access-policy",
        "after-first-unlock-this-device-only",
    )]))
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
                this_platforms_entry_modifiers().as_ref(),
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
