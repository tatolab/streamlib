// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Where a relay instance's secrets live: the OS secret store — the macOS
//! Keychain, the Secret Service on Linux — one item per relay instance and
//! setting under the `tatolab` service, or, where no store answers, an
//! owner-only file in the state directory.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

use streamlib_relay_adapter_sdk::secrecy::{ExposeSecret, SecretString};

use crate::owner_only_file_written_whole::write_an_owner_only_file_whole;

/// The service every relay secret is filed under in the OS secret store.
#[cfg(not(feature = "machine-directories-under-a-test-root"))]
const RELAY_SECRET_STORE_SERVICE_NAME: &str = "tatolab";

/// One relay instance's secrets, held by whichever store answers.
pub(crate) trait RelaySecretStore: Send + Sync {
    /// The secret `setting` of `relay_id`, or `None` when none is held.
    fn read(&self, relay_id: &str, setting: &str) -> Result<Option<SecretString>, String>;

    /// Hold `secret` as `setting` of `relay_id`, replacing what was held.
    fn write(&self, relay_id: &str, setting: &str, secret: &SecretString) -> Result<(), String>;

    /// Hold nothing for `setting` of `relay_id`.
    fn delete(&self, relay_id: &str, setting: &str) -> Result<(), String>;
}

/// The store this machine's relay secrets live in, said once: the OS secret
/// store when one answers, else the owner-only file at `fallback_file_path`.
pub(crate) fn the_relay_secret_store_of_this_machine(
    fallback_file_path: PathBuf,
) -> Box<dyn RelaySecretStore> {
    match the_os_secret_store_answering_now() {
        Ok(os_secret_store) => {
            tracing::info!("relay secrets are kept in the OS secret store");
            os_secret_store
        }
        Err(why_no_store_answers) => {
            tracing::warn!(
                "no OS secret store answers ({why_no_store_answers}), so relay secrets are kept \
                 in {}, readable by this user alone",
                fallback_file_path.display()
            );
            Box::new(RelaySecretsFallbackFile::at(fallback_file_path))
        }
    }
}

/// A test build never reaches the user's own secret store.
#[cfg(feature = "machine-directories-under-a-test-root")]
fn the_os_secret_store_answering_now() -> Result<Box<dyn RelaySecretStore>, String> {
    Err("a test build never reaches the user's own secret store".to_string())
}

/// The OS secret store, once a read of an item no relay holds is answered.
#[cfg(not(feature = "machine-directories-under-a-test-root"))]
fn the_os_secret_store_answering_now() -> Result<Box<dyn RelaySecretStore>, String> {
    let os_secret_store = OsRelaySecretStore {
        credential_store: this_platforms_credential_store()?,
    };
    os_secret_store.read("tatolab", "a-secret-no-relay-holds")?;
    Ok(Box::new(os_secret_store))
}

#[cfg(all(
    target_os = "linux",
    not(feature = "machine-directories-under-a-test-root")
))]
fn this_platforms_credential_store() -> Result<std::sync::Arc<keyring_core::CredentialStore>, String>
{
    zbus_secret_service_keyring_store::Store::new()
        .map(|store| store as std::sync::Arc<keyring_core::CredentialStore>)
        .map_err(|refusal| format!("the Secret Service did not answer: {refusal}"))
}

#[cfg(all(
    target_os = "macos",
    not(feature = "machine-directories-under-a-test-root")
))]
fn this_platforms_credential_store() -> Result<std::sync::Arc<keyring_core::CredentialStore>, String>
{
    apple_native_keyring_store::keychain::Store::new()
        .map(|store| store as std::sync::Arc<keyring_core::CredentialStore>)
        .map_err(|refusal| format!("the login keychain did not answer: {refusal}"))
}

/// The OS secret store, one item per relay instance and setting.
#[cfg(not(feature = "machine-directories-under-a-test-root"))]
struct OsRelaySecretStore {
    credential_store: std::sync::Arc<keyring_core::CredentialStore>,
}

#[cfg(not(feature = "machine-directories-under-a-test-root"))]
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

#[cfg(not(feature = "machine-directories-under-a-test-root"))]
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

/// Every relay secret in one owner-only JSON file, `{relay_id: {setting:
/// secret}}`, replaced whole on each change.
pub(crate) struct RelaySecretsFallbackFile {
    path: PathBuf,
    one_change_at_a_time: Mutex<()>,
}

type RelaySecretsByRelayAndSetting = BTreeMap<String, BTreeMap<String, String>>;

impl RelaySecretsFallbackFile {
    /// The file at `path`.
    pub(crate) fn at(path: PathBuf) -> Self {
        Self {
            path,
            one_change_at_a_time: Mutex::new(()),
        }
    }

    fn read_every_secret(&self) -> Result<RelaySecretsByRelayAndSetting, String> {
        match std::fs::read(&self.path) {
            Ok(file_bytes) => serde_json::from_slice(&file_bytes)
                .map_err(|_| format!("{} is not a relay secrets file", self.path.display())),
            Err(not_read) if not_read.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
            Err(not_read) => Err(format!(
                "{} cannot be read: {not_read}",
                self.path.display()
            )),
        }
    }

    fn change_every_secret(
        &self,
        change: impl FnOnce(&mut RelaySecretsByRelayAndSetting),
    ) -> Result<(), String> {
        let _one_change = self
            .one_change_at_a_time
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut every_secret = self.read_every_secret()?;
        change(&mut every_secret);
        every_secret.retain(|_, secrets_of_the_relay| !secrets_of_the_relay.is_empty());
        let file_bytes = serde_json::to_vec(&every_secret)
            .map_err(|cannot_serialize| cannot_serialize.to_string())?;
        write_an_owner_only_file_whole(&self.path, &file_bytes).map_err(|not_written| {
            format!("{} was not written: {not_written}", self.path.display())
        })
    }

    #[cfg(test)]
    fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl RelaySecretStore for RelaySecretsFallbackFile {
    fn read(&self, relay_id: &str, setting: &str) -> Result<Option<SecretString>, String> {
        Ok(self
            .read_every_secret()?
            .remove(relay_id)
            .and_then(|mut secrets_of_the_relay| secrets_of_the_relay.remove(setting))
            .map(SecretString::from))
    }

    fn write(&self, relay_id: &str, setting: &str, secret: &SecretString) -> Result<(), String> {
        self.change_every_secret(|every_secret| {
            every_secret
                .entry(relay_id.to_string())
                .or_default()
                .insert(setting.to_string(), secret.expose_secret().to_string());
        })
    }

    fn delete(&self, relay_id: &str, setting: &str) -> Result<(), String> {
        self.change_every_secret(|every_secret| {
            if let Some(secrets_of_the_relay) = every_secret.get_mut(relay_id) {
                secrets_of_the_relay.remove(setting);
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn a_secret(text: &str) -> SecretString {
        SecretString::from(text.to_string())
    }

    #[test]
    fn a_secret_round_trips_through_the_fallback_file_owner_only_and_is_replaced_on_rotation() {
        let directory = tempfile::tempdir().unwrap();
        let fallback_file =
            RelaySecretsFallbackFile::at(directory.path().join("relay-credentials.json"));
        assert!(
            fallback_file
                .read("cloudflare-1", "token")
                .unwrap()
                .is_none()
        );

        fallback_file
            .write("cloudflare-1", "token", &a_secret("first"))
            .unwrap();
        fallback_file
            .write("cloudflare-1", "token", &a_secret("rotated"))
            .unwrap();
        fallback_file
            .write("moqpro-1", "signing_key", &a_secret("key"))
            .unwrap();

        let reopened = RelaySecretsFallbackFile::at(fallback_file.path().to_path_buf());
        assert_eq!(
            reopened
                .read("cloudflare-1", "token")
                .unwrap()
                .unwrap()
                .expose_secret(),
            "rotated"
        );
        let file_text = std::fs::read_to_string(fallback_file.path()).unwrap();
        assert!(!file_text.contains("first"), "{file_text}");
        assert_eq!(
            std::fs::metadata(fallback_file.path())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );

        reopened.delete("cloudflare-1", "token").unwrap();
        assert!(reopened.read("cloudflare-1", "token").unwrap().is_none());
        assert!(
            reopened.read("moqpro-1", "signing_key").unwrap().is_some(),
            "deleting one secret keeps the others"
        );
    }

    #[cfg(feature = "machine-directories-under-a-test-root")]
    #[test]
    fn a_test_build_keeps_secrets_in_the_fallback_file() {
        let directory = tempfile::tempdir().unwrap();
        let fallback_path = directory.path().join("relay-credentials.json");
        let store = the_relay_secret_store_of_this_machine(fallback_path.clone());
        store.write("relay-1", "token", &a_secret("abc")).unwrap();
        assert!(fallback_path.exists());
    }
}
