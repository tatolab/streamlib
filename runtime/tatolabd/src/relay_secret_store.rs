// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Where a relay instance's secrets live: the OS secret store — the macOS
//! data-protection keychain, the Secret Service on Linux — one item per relay
//! instance and setting under the `tatolab` service, or, where no store
//! answers, an owner-only file in the state directory. Which one is chosen at
//! the first secret read, written or deleted, never as the runtime starts, so
//! a runtime that needs no relay secret never touches the OS secret store.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use streamlib_relay_adapter_sdk::secrecy::{ExposeSecret, SecretString};

use streamlib_runtime_client_contract::owner_only_file_written_whole::write_an_owner_only_file_whole;

/// One relay instance's secrets, held by whichever store answers.
pub(crate) trait RelaySecretStore: Send + Sync {
    /// The secret `setting` of `relay_id`, or `None` when none is held.
    fn read(&self, relay_id: &str, setting: &str) -> Result<Option<SecretString>, String>;

    /// Hold `secret` as `setting` of `relay_id`, replacing what was held.
    fn write(&self, relay_id: &str, setting: &str, secret: &SecretString) -> Result<(), String>;

    /// Hold nothing for `setting` of `relay_id`.
    fn delete(&self, relay_id: &str, setting: &str) -> Result<(), String>;
}

/// The store this machine's relay secrets live in, chosen at its first use:
/// the OS secret store when one answers, else the owner-only file at
/// `fallback_file_path`.
pub(crate) fn the_relay_secret_store_of_this_machine(
    fallback_file_path: PathBuf,
) -> Box<dyn RelaySecretStore> {
    Box::new(RelaySecretStoreChosenAtItsFirstUse::choosing_between(
        the_os_secret_store_answering_now,
        fallback_file_path,
    ))
}

/// The OS secret store, or why none answers.
type OsSecretStoreAnsweringNow = fn() -> Result<Box<dyn RelaySecretStore>, String>;

/// A relay secret store chosen, and said, at the first secret read, written
/// or deleted.
struct RelaySecretStoreChosenAtItsFirstUse {
    os_secret_store_answering_now: OsSecretStoreAnsweringNow,
    fallback_file_path: PathBuf,
    chosen_store: OnceLock<Box<dyn RelaySecretStore>>,
}

impl RelaySecretStoreChosenAtItsFirstUse {
    fn choosing_between(
        os_secret_store_answering_now: OsSecretStoreAnsweringNow,
        fallback_file_path: PathBuf,
    ) -> Self {
        Self {
            os_secret_store_answering_now,
            fallback_file_path,
            chosen_store: OnceLock::new(),
        }
    }

    fn chosen_store(&self) -> &dyn RelaySecretStore {
        self.chosen_store
            .get_or_init(|| {
                the_store_answering_now(
                    self.os_secret_store_answering_now,
                    self.fallback_file_path.clone(),
                )
            })
            .as_ref()
    }
}

impl RelaySecretStore for RelaySecretStoreChosenAtItsFirstUse {
    fn read(&self, relay_id: &str, setting: &str) -> Result<Option<SecretString>, String> {
        self.chosen_store().read(relay_id, setting)
    }

    fn write(&self, relay_id: &str, setting: &str, secret: &SecretString) -> Result<(), String> {
        self.chosen_store().write(relay_id, setting, secret)
    }

    fn delete(&self, relay_id: &str, setting: &str) -> Result<(), String> {
        self.chosen_store().delete(relay_id, setting)
    }
}

/// The OS secret store when one answers now, else the owner-only file at
/// `fallback_file_path`, said once.
fn the_store_answering_now(
    os_secret_store_answering_now: OsSecretStoreAnsweringNow,
    fallback_file_path: PathBuf,
) -> Box<dyn RelaySecretStore> {
    match os_secret_store_answering_now() {
        Ok(os_secret_store) => {
            tracing::info!("relay secrets are kept in the OS secret store");
            if fallback_file_path.exists() {
                move_the_fallback_files_secrets_into(
                    os_secret_store.as_ref(),
                    &RelaySecretsFallbackFile::at(fallback_file_path),
                );
            }
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

/// Every secret `fallback_file` holds written to `os_secret_store`, then the
/// file removed; a secret not written leaves the file whole, said.
fn move_the_fallback_files_secrets_into(
    os_secret_store: &dyn RelaySecretStore,
    fallback_file: &RelaySecretsFallbackFile,
) {
    let moved = fallback_file.read_every_secret().and_then(|every_secret| {
        for (relay_id, secrets_of_the_relay) in every_secret {
            for (setting, secret) in secrets_of_the_relay {
                os_secret_store.write(&relay_id, &setting, &SecretString::from(secret))?;
            }
        }
        std::fs::remove_file(&fallback_file.path).map_err(|not_removed| {
            format!(
                "{} was not removed: {not_removed}",
                fallback_file.path.display()
            )
        })
    });
    match moved {
        Ok(()) => tracing::info!(
            "the relay secrets kept in {} are moved into the OS secret store",
            fallback_file.path.display()
        ),
        Err(why) => tracing::warn!(
            "the relay secrets kept in {} are not moved into the OS secret store, and are not \
             read while it answers: {why}",
            fallback_file.path.display()
        ),
    }
}

#[cfg(not(feature = "machine-directories-under-a-test-root"))]
use crate::os_relay_secret_store::the_os_secret_store_answering_now;

/// A test build never reaches the user's own secret store.
#[cfg(feature = "machine-directories-under-a-test-root")]
fn the_os_secret_store_answering_now() -> Result<Box<dyn RelaySecretStore>, String> {
    Err("a test build never reaches the user's own secret store".to_string())
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

    static OS_SECRET_STORE_ASKED: std::sync::atomic::AtomicUsize =
        std::sync::atomic::AtomicUsize::new(0);

    fn an_os_secret_store_that_never_answers() -> Result<Box<dyn RelaySecretStore>, String> {
        OS_SECRET_STORE_ASKED.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Err("no OS secret store on this machine".to_string())
    }

    #[test]
    fn the_os_secret_store_is_first_asked_at_the_first_secret_read_and_only_once() {
        let directory = tempfile::tempdir().unwrap();
        let fallback_path = directory.path().join("relay-credentials.json");
        let store = RelaySecretStoreChosenAtItsFirstUse::choosing_between(
            an_os_secret_store_that_never_answers,
            fallback_path.clone(),
        );
        let asked = || OS_SECRET_STORE_ASKED.load(std::sync::atomic::Ordering::SeqCst);
        assert_eq!(
            asked(),
            0,
            "making the store asks the OS secret store nothing"
        );

        assert!(store.read("relay-1", "token").unwrap().is_none());
        assert_eq!(asked(), 1);
        store.write("relay-1", "token", &a_secret("abc")).unwrap();
        store.delete("relay-1", "token").unwrap();
        assert_eq!(asked(), 1, "the store is chosen once");
        assert!(fallback_path.exists());
    }

    #[test]
    fn once_an_os_secret_store_answers_the_fallback_files_secrets_move_into_it() {
        let directory = tempfile::tempdir().unwrap();
        let fallback_path = directory.path().join("relay-credentials.json");
        let fallback_file = RelaySecretsFallbackFile::at(fallback_path.clone());
        fallback_file
            .write("cloudflare-1", "token", &a_secret("cf"))
            .unwrap();
        fallback_file
            .write("moqpro-1", "signing_key", &a_secret("key"))
            .unwrap();
        let os_secret_store =
            RelaySecretsFallbackFile::at(directory.path().join("os-secret-store.json"));

        move_the_fallback_files_secrets_into(&os_secret_store, &fallback_file);

        assert!(
            !fallback_path.exists(),
            "the fallback file is removed once moved"
        );
        assert_eq!(
            os_secret_store
                .read("cloudflare-1", "token")
                .unwrap()
                .unwrap()
                .expose_secret(),
            "cf"
        );
        assert_eq!(
            os_secret_store
                .read("moqpro-1", "signing_key")
                .unwrap()
                .unwrap()
                .expose_secret(),
            "key"
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
