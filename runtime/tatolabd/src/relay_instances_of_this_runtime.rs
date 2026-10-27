// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The relay instances of this runtime: every built-in instance and every one
//! recorded in `relays.json`, joined as `tatolabd` starts; added, configured
//! and removed live through the local API; each joined one handed to the
//! publisher. Secrets stay in the [`RelaySecretStore`], never in the record,
//! a listing or a log line.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Mutex, Weak};
use std::time::{Duration, Instant};

use serde_json::{Map, Value};
use streamlib_relay_adapter_sdk::secrecy::{ExposeSecret, SecretString};
use streamlib_relay_adapter_sdk::{
    JoinedRelay, MachineName, RelayAdapter, RelayAdapterCatalogEntry, RelayAdapterInstanceMaker,
    RelayAdapterRefusal, RelayInstanceControlRefusal, RelayInstanceId, RelayInstancesOfThisMachine,
};
use streamlib_runtime_client_contract::local_api_wire_contract::{
    ListedRelayConnectionState, ListedRelayInstance, ListedRelaySecretPresence,
};
use tokio::sync::watch;

use crate::exposed_port_publisher::RelayTheExposedPortPublisherDials;
use crate::exposed_port_publisher_session_state::{
    ExposedPortPublisherSessionStateSender, a_relay_instances_session_state_not_dialed,
};
use crate::relay_instances_record::{RecordedRelayInstance, RelayInstancesRecordFile};
use crate::relay_secret_store::RelaySecretStore;

/// The longest a renewing relay's credential goes unchecked: a session its
/// relay ended, or a renewal a machine's sleep let pass, is renewed within it.
pub(crate) const A_RENEWING_RELAYS_CREDENTIAL_IS_CHECKED_AT_LEAST_EVERY: Duration =
    Duration::from_secs(30);

/// The relays the publisher dials, as they change.
pub(crate) type RelaysThePublisherDialsReceiver =
    watch::Receiver<Vec<RelayTheExposedPortPublisherDials>>;

/// One adapter compiled into this runtime, its catalog entry read once.
struct RelayAdapterOfThisRuntime {
    catalog_entry: RelayAdapterCatalogEntry,
    secret_setting_names: Vec<String>,
    maker: Box<dyn RelayAdapterInstanceMaker>,
}

/// One join of a relay instance: where its adapter said to dial it, and how
/// the publisher dials it, its credential asked for once per join and again
/// once due.
struct JoinOfARelayInstance {
    joined: JoinedRelay,
    dialed_as: Option<RelayTheExposedPortPublisherDials>,
}

impl JoinOfARelayInstance {
    fn just_joined(joined: JoinedRelay) -> Self {
        Self {
            joined,
            dialed_as: None,
        }
    }
}

/// One relay instance this runtime holds.
struct HeldRelayInstance {
    recorded: RecordedRelayInstance,
    built_in: bool,
    secret_settings_set: BTreeSet<String>,
    adapter: Option<Box<dyn RelayAdapter>>,
    join: Option<JoinOfARelayInstance>,
    why_not_joined: Option<String>,
    session_state: ExposedPortPublisherSessionStateSender,
}

/// Whether `adapter` carries any exposed port, so has something to publish.
fn serves_a_level(adapter: &dyn RelayAdapter) -> bool {
    !adapter.levels_served().is_empty()
}

impl HeldRelayInstance {
    fn unjoined(
        recorded: RecordedRelayInstance,
        built_in: bool,
        secret_settings_set: BTreeSet<String>,
    ) -> Self {
        Self {
            recorded,
            built_in,
            secret_settings_set,
            adapter: None,
            join: None,
            why_not_joined: None,
            session_state: a_relay_instances_session_state_not_dialed(),
        }
    }

    fn leave(&mut self) {
        if let Some(adapter) = &mut self.adapter {
            adapter.leave();
        }
        self.join = None;
    }

    /// How the publisher dials this instance, or `None` while it is not
    /// joined or serves no level, so has nothing to publish: the one asked
    /// for at its join, or a fresh one once its credential is due at `now` —
    /// its renewal passed, or the session it opened ended.
    fn dialed_as_at(
        &mut self,
        machine: &MachineName,
        now: Instant,
    ) -> Option<RelayTheExposedPortPublisherDials> {
        let adapter = self
            .adapter
            .as_deref()
            .filter(|adapter| serves_a_level(*adapter))?;
        let join = self.join.as_mut()?;
        let due = join
            .dialed_as
            .as_ref()
            .is_none_or(|dialed_as| dialed_as.its_credential_is_due_at(now));
        if due {
            join.dialed_as = Some(RelayTheExposedPortPublisherDials::of_a_joined_relay(
                adapter,
                &join.joined,
                machine,
                self.session_state.clone(),
            ));
        }
        join.dialed_as.clone()
    }

    /// Where this instance stands, its adapter's state and, once joined, the
    /// publisher's session with it if it serves a level.
    fn connection_state_as_listed(&self) -> ListedRelayConnectionState {
        match (&self.why_not_joined, &self.adapter) {
            (Some(why), _) => ListedRelayConnectionState::NotJoined {
                why: Some(why.clone()),
            },
            (None, Some(adapter)) => match adapter.events().borrow().as_listed() {
                ListedRelayConnectionState::Joined if serves_a_level(adapter.as_ref()) => {
                    self.session_state.borrow().as_listed_for_a_joined_relay()
                }
                not_joined => not_joined,
            },
            (None, None) => ListedRelayConnectionState::NotJoined { why: None },
        }
    }
}

/// Every relay instance of this runtime.
pub(crate) struct RelayInstancesOfThisRuntime {
    adapters: Vec<RelayAdapterOfThisRuntime>,
    record_file: RelayInstancesRecordFile,
    why_the_record_is_not_written: Option<String>,
    secret_store: Box<dyn RelaySecretStore>,
    machine_name: MachineName,
    held_instances: Mutex<Vec<HeldRelayInstance>>,
    relays_the_publisher_dials: watch::Sender<Vec<RelayTheExposedPortPublisherDials>>,
}

/// An instance's settings split from the secrets among them; a secret named
/// `None` is one to hold nothing for.
struct SettingsAndSecrets {
    settings: Map<String, Value>,
    secrets: BTreeMap<String, Option<SecretString>>,
}

/// What `configure_relay` changes on one instance, and what it had.
struct RelayInstanceReconfiguration {
    settings_before: Map<String, Value>,
    secrets_before: BTreeMap<String, SecretString>,
    settings_after: Map<String, Value>,
    secrets_after: BTreeMap<String, SecretString>,
    secrets_named: BTreeMap<String, Option<SecretString>>,
}

impl RelayInstancesOfThisRuntime {
    /// Every built-in instance and every recorded one, each joined; one that
    /// does not join is held unjoined, said by name.
    pub(crate) fn join_every_relay_instance(
        adapter_makers: Vec<Box<dyn RelayAdapterInstanceMaker>>,
        record_file: RelayInstancesRecordFile,
        secret_store: Box<dyn RelaySecretStore>,
        machine_name: MachineName,
    ) -> Self {
        let (recorded_instances, why_the_record_is_not_written) = match record_file.read() {
            Ok(recorded_instances) => (recorded_instances, None),
            Err(unreadable) => {
                tracing::error!(
                    "{unreadable}; only the built-in relays are joined, and no relay change is \
                     recorded until it is fixed or removed"
                );
                (Vec::new(), Some(unreadable))
            }
        };
        let (relays_the_publisher_dials, _) = watch::channel(Vec::new());
        let relay_instances = Self {
            adapters: adapter_makers
                .into_iter()
                .map(|maker| {
                    let catalog_entry = maker.catalog_entry();
                    RelayAdapterOfThisRuntime {
                        secret_setting_names: catalog_entry.secret_setting_names(),
                        catalog_entry,
                        maker,
                    }
                })
                .collect(),
            record_file,
            why_the_record_is_not_written,
            secret_store,
            machine_name,
            held_instances: Mutex::new(Vec::new()),
            relays_the_publisher_dials,
        };

        let mut held_instances = Vec::new();
        for adapter in &relay_instances.adapters {
            for built_in in adapter.maker.built_in_instances() {
                let recorded = recorded_instances
                    .iter()
                    .find(|recorded| recorded.relay_id == built_in.relay_id.0)
                    .cloned()
                    .unwrap_or_else(|| RecordedRelayInstance {
                        relay_id: built_in.relay_id.0,
                        adapter: adapter.catalog_entry.kind.clone(),
                        settings: built_in.settings,
                    });
                held_instances.push(HeldRelayInstance::unjoined(recorded, true, BTreeSet::new()));
            }
        }
        for recorded in recorded_instances {
            if !held_instances
                .iter()
                .any(|held| held.recorded.relay_id == recorded.relay_id)
            {
                held_instances.push(HeldRelayInstance::unjoined(
                    recorded,
                    false,
                    BTreeSet::new(),
                ));
            }
        }
        for held in &mut held_instances {
            relay_instances.join_with_its_held_settings(held);
        }
        *relay_instances.lock_the_held_instances() = held_instances;
        relay_instances.hand_the_publisher_every_joined_relay();
        relay_instances
    }

    /// The relays the publisher dials, now and as they change.
    pub(crate) fn relays_the_publisher_dials(&self) -> RelaysThePublisherDialsReceiver {
        self.relays_the_publisher_dials.subscribe()
    }

    /// Hand the publisher a fresh credential for each relay whose credential
    /// falls due — its renewal passed, or the session it opened ended — until
    /// `relay_instances` is dropped. A renewing relay is checked at least every
    /// `checked_at_least_every` — [`A_RENEWING_RELAYS_CREDENTIAL_IS_CHECKED_AT_LEAST_EVERY`]
    /// in the runtime — since its renewal is timed on a clock that stops while
    /// the machine sleeps and its token's expiry on one that does not.
    pub(crate) async fn renew_each_dial_credential_once_due(
        relay_instances: Weak<Self>,
        checked_at_least_every: Duration,
    ) {
        let Some(mut relays) = relay_instances
            .upgrade()
            .map(|relay_instances| relay_instances.relays_the_publisher_dials())
        else {
            return;
        };
        loop {
            let next_check = relays
                .borrow_and_update()
                .iter()
                .filter_map(|relay| relay.renew_by)
                .min()
                .map(|renew_by| renew_by.min(Instant::now() + checked_at_least_every));
            let a_check_is_due = async {
                match next_check {
                    Some(check_at) => tokio::time::sleep_until(check_at.into()).await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                changed = relays.changed() => if changed.is_err() {
                    return;
                },
                () = a_check_is_due => {
                    let now = Instant::now();
                    let relays_due = relays
                        .borrow()
                        .iter()
                        .filter(|relay| relay.its_credential_is_due_at(now))
                        .map(|relay| relay.relay_id.to_string())
                        .collect::<Vec<_>>();
                    if relays_due.is_empty() {
                        continue;
                    }
                    let Some(relay_instances) = relay_instances.upgrade() else {
                        return;
                    };
                    tracing::info!(
                        relays = %relays_due.join(", "),
                        "a dial credential is due; the publisher is handed a fresh one"
                    );
                    relay_instances.hand_the_publisher_every_joined_relay();
                }
            }
        }
    }

    /// Leave every relay; the publisher dials none afterwards.
    pub(crate) fn leave_every_relay(&self) {
        for held in self.lock_the_held_instances().iter_mut() {
            held.leave();
        }
        self.relays_the_publisher_dials.send_replace(Vec::new());
    }

    fn lock_the_held_instances(&self) -> std::sync::MutexGuard<'_, Vec<HeldRelayInstance>> {
        self.held_instances
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn adapter_named(&self, adapter: &str) -> Option<&RelayAdapterOfThisRuntime> {
        self.adapters
            .iter()
            .find(|candidate| candidate.catalog_entry.kind == adapter)
    }

    fn secret_setting_names_of(&self, adapter: &str) -> &[String] {
        self.adapter_named(adapter)
            .map(|adapter| adapter.secret_setting_names.as_slice())
            .unwrap_or_default()
    }

    /// `held` joined with its recorded settings and its stored secrets, or
    /// held unjoined saying why.
    fn join_with_its_held_settings(&self, held: &mut HeldRelayInstance) {
        let joined = self
            .the_stored_secrets_of(&held.recorded)
            .and_then(|secrets| {
                held.secret_settings_set = secrets.keys().cloned().collect();
                let settings = held.recorded.settings.clone();
                self.join(held, &settings, &secrets)
            });
        if let Err(refusal) = joined {
            tracing::error!("{refusal}");
            held.why_not_joined = Some(refusal.to_string());
        }
    }

    fn the_stored_secrets_of(
        &self,
        recorded: &RecordedRelayInstance,
    ) -> Result<BTreeMap<String, SecretString>, RelayInstanceControlRefusal> {
        let mut secrets = BTreeMap::new();
        for setting in self.secret_setting_names_of(&recorded.adapter) {
            let stored = self
                .secret_store
                .read(&recorded.relay_id, setting)
                .map_err(|why| RelayInstanceControlRefusal::SecretNotStored {
                    relay_id: recorded.relay_id.clone(),
                    setting: setting.clone(),
                    why: format!("it cannot be read: {why}"),
                })?;
            if let Some(secret) = stored {
                secrets.insert(setting.clone(), secret);
            }
        }
        Ok(secrets)
    }

    fn make_the_adapter_of(
        &self,
        recorded: &RecordedRelayInstance,
    ) -> Result<Box<dyn RelayAdapter>, RelayAdapterRefusal> {
        let relay_id = RelayInstanceId(recorded.relay_id.clone());
        match self.adapter_named(&recorded.adapter) {
            Some(adapter) => adapter.maker.make_the_instance(&relay_id),
            None => Err(RelayAdapterRefusal::JoinFailed {
                relay_id,
                why: format!(
                    "no relay adapter is named `{}` in this runtime",
                    recorded.adapter
                ),
            }),
        }
    }

    /// Join `held` with `settings` and `secrets`, making its adapter first
    /// when it has none; a refusal is held as why it is not joined.
    fn join(
        &self,
        held: &mut HeldRelayInstance,
        settings: &Map<String, Value>,
        secrets: &BTreeMap<String, SecretString>,
    ) -> Result<(), RelayInstanceControlRefusal> {
        let joined = match &mut held.adapter {
            Some(adapter) => adapter.join(settings_with_the_secrets_among_them(settings, secrets)),
            no_adapter_yet => self.make_the_adapter_of(&held.recorded).and_then(|made| {
                no_adapter_yet
                    .insert(made)
                    .join(settings_with_the_secrets_among_them(settings, secrets))
            }),
        };
        match joined {
            Ok(joined) => {
                tracing::info!(relay = %held.recorded.relay_id, "the relay is joined");
                held.join = Some(JoinOfARelayInstance::just_joined(joined));
                held.why_not_joined = None;
                Ok(())
            }
            Err(refusal) => {
                held.join = None;
                held.why_not_joined = Some(refusal.to_string());
                Err(refusal.into())
            }
        }
    }

    fn split_the_settings_from_the_secrets(
        &self,
        relay_id: &str,
        adapter: &str,
        settings: Map<String, Value>,
    ) -> Result<SettingsAndSecrets, RelayInstanceControlRefusal> {
        let secret_setting_names = self.secret_setting_names_of(adapter);
        let mut split = SettingsAndSecrets {
            settings: Map::new(),
            secrets: BTreeMap::new(),
        };
        for (setting, value) in settings {
            if !secret_setting_names.contains(&setting) {
                split.settings.insert(setting, value);
                continue;
            }
            let secret = match value {
                Value::Null => None,
                Value::String(secret) => Some(SecretString::from(secret)),
                _ => {
                    return Err(RelayAdapterRefusal::SettingsRefused {
                        relay_id: RelayInstanceId(relay_id.to_string()),
                        why: format!("the secret `{setting}` is a string"),
                    }
                    .into());
                }
            };
            split.secrets.insert(setting, secret);
        }
        Ok(split)
    }

    /// Hold `secret` as `setting` of `relay_id`, or nothing for `None`.
    fn store_one_secret(
        &self,
        relay_id: &str,
        setting: &str,
        secret: Option<&SecretString>,
    ) -> Result<(), String> {
        match secret {
            Some(secret) => self.secret_store.write(relay_id, setting, secret),
            None => self.secret_store.delete(relay_id, setting),
        }
    }

    /// Hold each of `secrets` for `relay_id`, putting back what `held_before`
    /// had for each already stored when one cannot be.
    fn store_the_secrets(
        &self,
        relay_id: &str,
        secrets: &BTreeMap<String, Option<SecretString>>,
        held_before: &BTreeMap<String, SecretString>,
    ) -> Result<(), RelayInstanceControlRefusal> {
        let mut stored_so_far = Vec::new();
        for (setting, secret) in secrets {
            if let Err(why) = self.store_one_secret(relay_id, setting, secret.as_ref()) {
                self.put_back_the_secrets(relay_id, stored_so_far, held_before);
                return Err(RelayInstanceControlRefusal::SecretNotStored {
                    relay_id: relay_id.to_string(),
                    setting: setting.clone(),
                    why,
                });
            }
            stored_so_far.push(setting.as_str());
        }
        Ok(())
    }

    /// Hold again what `held_before` had for each of `settings`, saying so
    /// where it cannot be.
    fn put_back_the_secrets<'setting>(
        &self,
        relay_id: &str,
        settings: impl IntoIterator<Item = &'setting str>,
        held_before: &BTreeMap<String, SecretString>,
    ) {
        for setting in settings {
            if let Err(why) = self.store_one_secret(relay_id, setting, held_before.get(setting)) {
                tracing::warn!(
                    relay = %relay_id,
                    "the secret `{setting}` could not be put back as it was: {why}"
                );
            }
        }
    }

    fn delete_every_secret_of(&self, recorded: &RecordedRelayInstance) {
        for setting in self.secret_setting_names_of(&recorded.adapter) {
            if let Err(why) = self.secret_store.delete(&recorded.relay_id, setting) {
                tracing::warn!(
                    relay = %recorded.relay_id,
                    "the secret `{setting}` of a relay no longer held was not deleted: {why}"
                );
            }
        }
    }

    /// Refuse a change to `relay_id` before it touches a relay or the store
    /// when `relays.json` could not be read at start: the instances it holds
    /// are not held, so an id or a secret of theirs could be taken.
    fn refuse_while_the_record_is_not_written(
        &self,
        relay_id: &str,
    ) -> Result<(), RelayInstanceControlRefusal> {
        match &self.why_the_record_is_not_written {
            Some(why) => Err(RelayInstanceControlRefusal::NotRecorded {
                relay_id: relay_id.to_string(),
                why: why.clone(),
            }),
            None => Ok(()),
        }
    }

    /// Record exactly `instances` in `relays.json`, or refuse naming `relay_id`.
    fn record<'recorded>(
        &self,
        relay_id: &str,
        instances: impl IntoIterator<Item = &'recorded RecordedRelayInstance>,
    ) -> Result<(), RelayInstanceControlRefusal> {
        let not_recorded = |why: String| RelayInstanceControlRefusal::NotRecorded {
            relay_id: relay_id.to_string(),
            why,
        };
        self.refuse_while_the_record_is_not_written(relay_id)?;
        self.record_file.write(instances).map_err(not_recorded)
    }

    fn hand_the_publisher_every_joined_relay(&self) {
        let now = Instant::now();
        let relays: Vec<RelayTheExposedPortPublisherDials> = self
            .lock_the_held_instances()
            .iter_mut()
            .filter_map(|held| held.dialed_as_at(&self.machine_name, now))
            .collect();
        self.relays_the_publisher_dials.send_replace(relays);
    }

    /// Where `relay_id` is among `held_instances`, or the refusal naming them.
    fn position_of(
        relay_id: &str,
        held_instances: &[HeldRelayInstance],
    ) -> Result<usize, RelayInstanceControlRefusal> {
        held_instances
            .iter()
            .position(|held| held.recorded.relay_id == relay_id)
            .ok_or_else(|| RelayInstanceControlRefusal::UnknownRelay {
                relay_id: relay_id.to_string(),
                known: names_in_backticks(
                    held_instances
                        .iter()
                        .map(|held| held.recorded.relay_id.as_str()),
                ),
            })
    }

    fn the_lowest_free_relay_id_of(adapter: &str, held_instances: &[HeldRelayInstance]) -> String {
        let mut number: u64 = 1;
        loop {
            let relay_id = format!("{adapter}-{number}");
            if !held_instances
                .iter()
                .any(|held| held.recorded.relay_id == relay_id)
            {
                return relay_id;
            }
            number += 1;
        }
    }

    /// Join the instance at `position` with what `reconfiguration` makes of
    /// it, store its secrets and record it; on a refusal the caller puts back
    /// what it had.
    fn reconfigure_the_held_instance(
        &self,
        held_instances: &mut [HeldRelayInstance],
        position: usize,
        reconfiguration: &RelayInstanceReconfiguration,
    ) -> Result<(), RelayInstanceControlRefusal> {
        let held = &mut held_instances[position];
        let relay_id = held.recorded.relay_id.clone();
        self.join(
            held,
            &reconfiguration.settings_after,
            &reconfiguration.secrets_after,
        )?;
        self.store_the_secrets(
            &relay_id,
            &reconfiguration.secrets_named,
            &reconfiguration.secrets_before,
        )?;
        held.recorded.settings = reconfiguration.settings_after.clone();
        held.secret_settings_set = reconfiguration.secrets_after.keys().cloned().collect();
        self.record(&relay_id, held_instances.iter().map(|held| &held.recorded))
            .inspect_err(|_| {
                self.put_back_the_secrets(
                    &relay_id,
                    reconfiguration.secrets_named.keys().map(String::as_str),
                    &reconfiguration.secrets_before,
                );
            })
    }
}

/// `names`, each in backticks, comma-separated.
fn names_in_backticks<'name>(names: impl IntoIterator<Item = &'name str>) -> String {
    names
        .into_iter()
        .map(|name| format!("`{name}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// `settings` with each of `secrets` put among them, as the adapter reads them.
fn settings_with_the_secrets_among_them(
    settings: &Map<String, Value>,
    secrets: &BTreeMap<String, SecretString>,
) -> Value {
    let mut settings = settings.clone();
    for (setting, secret) in secrets {
        settings.insert(
            setting.clone(),
            Value::String(secret.expose_secret().to_string()),
        );
    }
    Value::Object(settings)
}

impl RelayInstancesOfThisMachine for RelayInstancesOfThisRuntime {
    fn relay_adapter_catalog(&self) -> Vec<RelayAdapterCatalogEntry> {
        self.adapters
            .iter()
            .map(|adapter| adapter.catalog_entry.clone())
            .collect()
    }

    fn add_relay(
        &self,
        adapter: &str,
        settings: Map<String, Value>,
    ) -> Result<RelayInstanceId, RelayInstanceControlRefusal> {
        let Some(adapter_of_this_runtime) = self.adapter_named(adapter) else {
            return Err(RelayInstanceControlRefusal::UnknownAdapter {
                adapter: adapter.to_string(),
                known: names_in_backticks(
                    self.adapters
                        .iter()
                        .map(|adapter| adapter.catalog_entry.kind.as_str()),
                ),
            });
        };
        if let Some(why) = adapter_of_this_runtime.maker.why_no_instance_is_added() {
            return Err(RelayInstanceControlRefusal::AdapterAddsNoInstance {
                adapter: adapter.to_string(),
                why,
            });
        }
        let mut held_instances = self.lock_the_held_instances();
        let relay_id = Self::the_lowest_free_relay_id_of(adapter, &held_instances);
        self.refuse_while_the_record_is_not_written(&relay_id)?;
        let split = self.split_the_settings_from_the_secrets(&relay_id, adapter, settings)?;
        let secrets: BTreeMap<String, SecretString> = split
            .secrets
            .iter()
            .filter_map(|(setting, secret)| Some((setting.clone(), secret.clone()?)))
            .collect();
        let mut held = HeldRelayInstance::unjoined(
            RecordedRelayInstance {
                relay_id: relay_id.clone(),
                adapter: adapter.to_string(),
                settings: split.settings,
            },
            false,
            secrets.keys().cloned().collect(),
        );
        let settings = held.recorded.settings.clone();
        let added = self
            .join(&mut held, &settings, &secrets)
            .and_then(|()| self.store_the_secrets(&relay_id, &split.secrets, &BTreeMap::new()))
            .and_then(|()| {
                self.record(
                    &relay_id,
                    held_instances
                        .iter()
                        .map(|held| &held.recorded)
                        .chain(std::iter::once(&held.recorded)),
                )
                .inspect_err(|_| self.delete_every_secret_of(&held.recorded))
            });
        if let Err(refusal) = added {
            held.leave();
            return Err(refusal);
        }
        held_instances.push(held);
        drop(held_instances);
        tracing::info!(relay = %relay_id, "the relay is added through the `{adapter}` adapter");
        self.hand_the_publisher_every_joined_relay();
        Ok(RelayInstanceId(relay_id))
    }

    fn configure_relay(
        &self,
        relay_id: &str,
        settings: Map<String, Value>,
    ) -> Result<(), RelayInstanceControlRefusal> {
        let mut held_instances = self.lock_the_held_instances();
        let position = Self::position_of(relay_id, &held_instances)?;
        self.refuse_while_the_record_is_not_written(relay_id)?;
        let held = &held_instances[position];
        let split =
            self.split_the_settings_from_the_secrets(relay_id, &held.recorded.adapter, settings)?;
        let secrets_before = self.the_stored_secrets_of(&held.recorded)?;
        let settings_before = held.recorded.settings.clone();

        let mut settings_after = settings_before.clone();
        for (setting, value) in split.settings {
            if value.is_null() {
                settings_after.remove(&setting);
            } else {
                settings_after.insert(setting, value);
            }
        }
        let mut secrets_after = secrets_before.clone();
        for (setting, secret) in &split.secrets {
            match secret {
                Some(secret) => secrets_after.insert(setting.clone(), secret.clone()),
                None => secrets_after.remove(setting),
            };
        }
        let reconfiguration = RelayInstanceReconfiguration {
            settings_before,
            secrets_before,
            settings_after,
            secrets_after,
            secrets_named: split.secrets,
        };

        let reconfigured =
            self.reconfigure_the_held_instance(&mut held_instances, position, &reconfiguration);
        if reconfigured.is_err() {
            let held = &mut held_instances[position];
            held.recorded.settings = reconfiguration.settings_before.clone();
            held.secret_settings_set = reconfiguration.secrets_before.keys().cloned().collect();
            if let Err(refusal) = self.join(
                held,
                &reconfiguration.settings_before,
                &reconfiguration.secrets_before,
            ) {
                tracing::error!("{refusal}");
            }
        }
        drop(held_instances);
        self.hand_the_publisher_every_joined_relay();
        reconfigured?;
        tracing::info!(relay = %relay_id, "the relay is configured");
        Ok(())
    }

    fn remove_relay(&self, relay_id: &str) -> Result<(), RelayInstanceControlRefusal> {
        let mut held_instances = self.lock_the_held_instances();
        let position = Self::position_of(relay_id, &held_instances)?;
        if held_instances[position].built_in {
            return Err(RelayInstanceControlRefusal::BuiltInRelayIsNeverRemoved {
                relay_id: relay_id.to_string(),
            });
        }
        self.record(
            relay_id,
            held_instances
                .iter()
                .enumerate()
                .filter(|(held_position, _)| *held_position != position)
                .map(|(_, held)| &held.recorded),
        )?;
        let mut removed = held_instances.remove(position);
        drop(held_instances);
        removed.leave();
        self.delete_every_secret_of(&removed.recorded);
        tracing::info!(relay = %relay_id, "the relay is removed");
        self.hand_the_publisher_every_joined_relay();
        Ok(())
    }

    fn list_relays(&self) -> Vec<ListedRelayInstance> {
        self.lock_the_held_instances()
            .iter()
            .map(|held| ListedRelayInstance {
                relay_id: held.recorded.relay_id.clone(),
                adapter: held.recorded.adapter.clone(),
                built_in: held.built_in,
                settings: held.recorded.settings.clone(),
                secrets: self
                    .secret_setting_names_of(&held.recorded.adapter)
                    .iter()
                    .map(|setting| {
                        let presence = if held.secret_settings_set.contains(setting) {
                            ListedRelaySecretPresence::Set
                        } else {
                            ListedRelaySecretPresence::Unset
                        };
                        (setting.clone(), presence)
                    })
                    .collect(),
                connection_state: held.connection_state_as_listed(),
            })
            .collect()
    }
}

impl Drop for RelayInstancesOfThisRuntime {
    fn drop(&mut self) {
        self.leave_every_relay();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex as StdMutex};
    use std::time::Duration;

    use serde::Deserialize;
    use serde_json::json;
    use streamlib_relay_adapter_sdk::url::Url;
    use streamlib_relay_adapter_sdk::{
        BroadcastPath, BuiltInRelayInstance, OutputPortExposureLevel, RelayAdapterKind,
        RelayConnectionState, RelayConnectionStateReceiver, RelayDialCredential,
        RelaySecretSetting, relay_instance_settings_from_json,
    };

    use super::*;
    use crate::exposed_port_publisher_session_state::ExposedPortPublisherSessionState;
    use crate::relay_secret_store::RelaySecretsFallbackFile;

    /// The token a provider's relay URL carries in its path.
    const URL_BORNE_TOKEN: &str = "tok-7c1f0e9b";

    /// The icon every test adapter carries: a one-rectangle SVG.
    const TEST_RELAY_ADAPTER_DISPLAY_ICON_SVG: &str =
        r#"<svg xmlns="http://www.w3.org/2000/svg"><rect/></svg>"#;

    /// A hosted relay standing in for a provider: its URL carries a token,
    /// so the whole URL is a secret.
    #[derive(Deserialize, streamlib_relay_adapter_sdk::schemars::JsonSchema)]
    #[schemars(crate = "streamlib_relay_adapter_sdk::schemars")]
    #[serde(deny_unknown_fields)]
    struct ProviderRelaySettings {
        /// The relay's URL, its token in the path.
        relay_url_carrying_a_token: RelaySecretSetting,
        #[serde(default)]
        region: Option<String>,
    }

    /// What each join of a provider instance was handed, the secret exposed.
    type JoinsSeenByTheProvider = Arc<StdMutex<Vec<(String, String, Option<String>)>>>;

    struct ProviderRelayAdapter {
        relay_id: RelayInstanceId,
        joins_seen: JoinsSeenByTheProvider,
        connection_state: watch::Sender<RelayConnectionState>,
        credential_renewed_every: Option<Duration>,
        credentials_minted: Arc<AtomicUsize>,
    }

    impl RelayAdapter for ProviderRelayAdapter {
        fn kind() -> RelayAdapterKind {
            RelayAdapterKind("provider")
        }

        fn doc() -> &'static str {
            "A hosted relay standing in for a provider."
        }

        fn display_name() -> &'static str {
            "Provider"
        }

        fn display_icon_svg() -> &'static str {
            TEST_RELAY_ADAPTER_DISPLAY_ICON_SVG
        }

        fn relay_id(&self) -> &RelayInstanceId {
            &self.relay_id
        }

        fn settings_schema() -> Value {
            use streamlib_relay_adapter_sdk::RelaySettingsJsonSchema;
            ProviderRelaySettings::processor_config_schema_document()
        }

        fn join(&mut self, settings: Value) -> Result<JoinedRelay, RelayAdapterRefusal> {
            let settings: ProviderRelaySettings =
                relay_instance_settings_from_json(&self.relay_id, settings)?;
            self.joins_seen.lock().unwrap().push((
                self.relay_id.0.clone(),
                settings
                    .relay_url_carrying_a_token
                    .expose_secret()
                    .to_string(),
                settings.region,
            ));
            self.connection_state
                .send_replace(RelayConnectionState::Joined);
            Ok(JoinedRelay {
                dial_url: Url::parse("https://relay.example/").unwrap(),
            })
        }

        fn leave(&mut self) {
            self.connection_state
                .send_replace(RelayConnectionState::NotJoined);
        }

        fn levels_served(&self) -> &[OutputPortExposureLevel] {
            &[OutputPortExposureLevel::Public]
        }

        fn broadcast_root(&self, machine: &MachineName) -> BroadcastPath {
            BroadcastPath::of_the_machine_under_the_root("", machine)
        }

        fn credential_for_dial(&self) -> Option<RelayDialCredential> {
            let renewed_every = self.credential_renewed_every?;
            let minted = self.credentials_minted.fetch_add(1, Ordering::SeqCst) + 1;
            Some(
                RelayDialCredential::token_in_the_dial_url_query(SecretString::from(format!(
                    "minted-{minted}"
                )))
                .renewed_by(Instant::now() + renewed_every),
            )
        }

        fn events(&self) -> RelayConnectionStateReceiver {
            self.connection_state.subscribe()
        }
    }

    struct ProviderRelayAdapterInstanceMaker {
        joins_seen: JoinsSeenByTheProvider,
        credential_renewed_every: Option<Duration>,
        credentials_minted: Arc<AtomicUsize>,
    }

    impl RelayAdapterInstanceMaker for ProviderRelayAdapterInstanceMaker {
        fn catalog_entry(&self) -> RelayAdapterCatalogEntry {
            RelayAdapterCatalogEntry::of_the_adapter::<ProviderRelayAdapter>()
        }

        fn make_the_instance(
            &self,
            relay_id: &RelayInstanceId,
        ) -> Result<Box<dyn RelayAdapter>, RelayAdapterRefusal> {
            Ok(Box::new(ProviderRelayAdapter {
                relay_id: relay_id.clone(),
                joins_seen: Arc::clone(&self.joins_seen),
                connection_state: watch::channel(RelayConnectionState::NotJoined).0,
                credential_renewed_every: self.credential_renewed_every,
                credentials_minted: Arc::clone(&self.credentials_minted),
            }))
        }
    }

    /// One built-in instance that never joins, as the local relay is on a
    /// machine with no `moq-relay`.
    struct BuiltInOnlyRelayAdapterInstanceMaker;

    impl RelayAdapterInstanceMaker for BuiltInOnlyRelayAdapterInstanceMaker {
        fn catalog_entry(&self) -> RelayAdapterCatalogEntry {
            RelayAdapterCatalogEntry {
                kind: "built-in-only".to_string(),
                display_name: "Built-in only".to_string(),
                display_icon_data_uri: RelayAdapterCatalogEntry::display_icon_data_uri_of_the_svg(
                    TEST_RELAY_ADAPTER_DISPLAY_ICON_SVG,
                ),
                doc: "A relay every machine has.".to_string(),
                settings_schema: json!({"type": "object", "properties": {}}),
            }
        }

        fn built_in_instances(&self) -> Vec<BuiltInRelayInstance> {
            vec![BuiltInRelayInstance {
                relay_id: RelayInstanceId("built-in-only-relay".to_string()),
                settings: Map::new(),
            }]
        }

        fn why_no_instance_is_added(&self) -> Option<String> {
            Some("it is built in".to_string())
        }

        fn make_the_instance(
            &self,
            relay_id: &RelayInstanceId,
        ) -> Result<Box<dyn RelayAdapter>, RelayAdapterRefusal> {
            Err(RelayAdapterRefusal::JoinFailed {
                relay_id: relay_id.clone(),
                why: "no relay program on this machine".to_string(),
            })
        }
    }

    /// A state directory, and every runtime started over it.
    struct RelayInstancesOverAStateDirectory {
        state_directory: tempfile::TempDir,
        joins_seen: JoinsSeenByTheProvider,
        credential_renewed_every: Option<Duration>,
        credentials_minted: Arc<AtomicUsize>,
    }

    impl RelayInstancesOverAStateDirectory {
        fn new() -> Self {
            Self {
                state_directory: tempfile::tempdir().unwrap(),
                joins_seen: Arc::default(),
                credential_renewed_every: None,
                credentials_minted: Arc::default(),
            }
        }

        /// A machine whose provider relays dial with a credential minted
        /// fresh at each ask and renewed every `renewed_every`.
        fn whose_provider_renews_its_credential_every(renewed_every: Duration) -> Self {
            Self {
                credential_renewed_every: Some(renewed_every),
                ..Self::new()
            }
        }

        fn record_path(&self) -> std::path::PathBuf {
            self.state_directory.path().join("relays.json")
        }

        fn fallback_secrets_path(&self) -> std::path::PathBuf {
            self.state_directory.path().join("relay-credentials.json")
        }

        fn start_a_runtime(&self) -> RelayInstancesOfThisRuntime {
            RelayInstancesOfThisRuntime::join_every_relay_instance(
                vec![
                    Box::new(BuiltInOnlyRelayAdapterInstanceMaker),
                    Box::new(ProviderRelayAdapterInstanceMaker {
                        joins_seen: Arc::clone(&self.joins_seen),
                        credential_renewed_every: self.credential_renewed_every,
                        credentials_minted: Arc::clone(&self.credentials_minted),
                    }),
                ],
                RelayInstancesRecordFile::at(self.record_path()),
                Box::new(RelaySecretsFallbackFile::at(self.fallback_secrets_path())),
                MachineName::from_a_hostname("desk").unwrap(),
            )
        }

        fn last_join_seen(&self) -> (String, String, Option<String>) {
            self.joins_seen.lock().unwrap().last().cloned().unwrap()
        }

        fn every_file_written(&self) -> String {
            [self.record_path(), self.fallback_secrets_path()]
                .iter()
                .filter_map(|path| std::fs::read_to_string(path).ok())
                .collect::<Vec<_>>()
                .join("\n")
        }
    }

    fn provider_settings(relay_url: &str) -> Map<String, Value> {
        json!({"relay_url_carrying_a_token": relay_url, "region": "us"})
            .as_object()
            .unwrap()
            .clone()
    }

    fn a_relay_url_carrying(token: &str) -> String {
        format!("https://relay.example/{token}/")
    }

    fn listed<'listing>(
        listing: &'listing [ListedRelayInstance],
        relay_id: &str,
    ) -> &'listing ListedRelayInstance {
        listing
            .iter()
            .find(|listed| listed.relay_id == relay_id)
            .unwrap_or_else(|| panic!("`{relay_id}` is not listed: {listing:?}"))
    }

    /// The publisher's session with each relay handed over connects.
    fn the_publishers_session_connects_to_every_relay_handed_over(
        relay_instances: &RelayInstancesOfThisRuntime,
    ) {
        report_the_publishers_session_with(
            relay_instances,
            ExposedPortPublisherSessionState::Connected,
        );
    }

    fn report_the_publishers_session_with(
        relay_instances: &RelayInstancesOfThisRuntime,
        session_state: ExposedPortPublisherSessionState,
    ) {
        for relay in relay_instances.relays_the_publisher_dials().borrow().iter() {
            relay.session_state.send_replace(session_state.clone());
        }
    }

    fn the_dial_url_handed_over_for(
        relay_instances: &RelayInstancesOfThisRuntime,
        relay_id: &str,
    ) -> String {
        relay_instances
            .relays_the_publisher_dials()
            .borrow()
            .iter()
            .find(|relay| relay.relay_id.0 == relay_id)
            .map(|relay| {
                relay
                    .dial_url_carrying_the_credential
                    .expose_secret()
                    .to_string()
            })
            .unwrap_or_else(|| panic!("`{relay_id}` is not handed to the publisher"))
    }

    #[test]
    fn an_adapters_catalog_entry_carries_its_display_name_and_its_icon_as_a_data_uri() {
        let catalog_entry = RelayAdapterCatalogEntry::of_the_adapter::<ProviderRelayAdapter>();
        assert_eq!(catalog_entry.display_name, "Provider");
        assert_eq!(
            catalog_entry.display_icon_data_uri,
            RelayAdapterCatalogEntry::display_icon_data_uri_of_the_svg(
                TEST_RELAY_ADAPTER_DISPLAY_ICON_SVG
            )
        );
    }

    #[test]
    fn a_joined_relay_is_listed_as_the_publishers_session_with_it_stands() {
        let machine = RelayInstancesOverAStateDirectory::new();
        let relay_instances = machine.start_a_runtime();
        relay_instances
            .add_relay("provider", provider_settings(&a_relay_url_carrying("a")))
            .unwrap();
        let state_listed = || {
            listed(&relay_instances.list_relays(), "provider-1")
                .connection_state
                .clone()
        };
        assert_eq!(state_listed(), ListedRelayConnectionState::Joining);
        the_publishers_session_connects_to_every_relay_handed_over(&relay_instances);
        assert_eq!(state_listed(), ListedRelayConnectionState::Joined);
        report_the_publishers_session_with(
            &relay_instances,
            ExposedPortPublisherSessionState::Redialing,
        );
        assert!(
            matches!(state_listed(), ListedRelayConnectionState::Lost { .. }),
            "{:?}",
            state_listed()
        );
        report_the_publishers_session_with(
            &relay_instances,
            ExposedPortPublisherSessionState::Ended {
                why: "unauthorized".to_string(),
            },
        );
        assert_eq!(
            state_listed(),
            ListedRelayConnectionState::NotJoined {
                why: Some("the relay ended the publisher's session: unauthorized".to_string())
            }
        );
        assert_eq!(
            listed(&relay_instances.list_relays(), "built-in-only-relay").connection_state,
            ListedRelayConnectionState::NotJoined {
                why: Some(
                    "the relay `built-in-only-relay` could not be joined: no relay program on \
                     this machine"
                        .to_string()
                )
            },
            "an instance its adapter never joined is listed by its adapter's refusal"
        );
    }

    #[test]
    fn a_credential_not_yet_due_is_kept_through_another_relays_change() {
        let machine = RelayInstancesOverAStateDirectory::whose_provider_renews_its_credential_every(
            Duration::from_secs(3600),
        );
        let relay_instances = machine.start_a_runtime();
        relay_instances
            .add_relay("provider", provider_settings(&a_relay_url_carrying("a")))
            .unwrap();
        let first_dial_url = the_dial_url_handed_over_for(&relay_instances, "provider-1");
        assert_eq!(first_dial_url, "https://relay.example/?jwt=minted-1");
        relay_instances
            .add_relay("provider", provider_settings(&a_relay_url_carrying("b")))
            .unwrap();
        assert_eq!(
            the_dial_url_handed_over_for(&relay_instances, "provider-1"),
            first_dial_url,
            "a hand-over for another relay asks no fresh credential of one not yet due"
        );
        relay_instances
            .configure_relay(
                "provider-1",
                json!({"region": "eu"}).as_object().unwrap().clone(),
            )
            .unwrap();
        assert_ne!(
            the_dial_url_handed_over_for(&relay_instances, "provider-1"),
            first_dial_url,
            "joining again asks for a fresh credential"
        );
    }

    /// A renewing relay's session ended long before its renewal: the loop's
    /// own check, not a renewal, mints it again — what a machine waking from a
    /// sleep its renewal clock did not count needs.
    #[test]
    fn the_renewal_loop_mints_again_a_renewing_relay_whose_session_ended_before_its_renewal() {
        let async_runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let machine = RelayInstancesOverAStateDirectory::whose_provider_renews_its_credential_every(
            Duration::from_secs(3600),
        );
        let relay_instances = Arc::new(machine.start_a_runtime());
        relay_instances
            .add_relay("provider", provider_settings(&a_relay_url_carrying("a")))
            .unwrap();
        let first_dial_url = the_dial_url_handed_over_for(&relay_instances, "provider-1");
        let mut relays = relay_instances.relays_the_publisher_dials();
        relays.borrow_and_update();
        async_runtime.block_on(async {
            let renewing = tokio::spawn(
                RelayInstancesOfThisRuntime::renew_each_dial_credential_once_due(
                    Arc::downgrade(&relay_instances),
                    Duration::from_millis(200),
                ),
            );
            tokio::task::yield_now().await;
            report_the_publishers_session_with(
                &relay_instances,
                ExposedPortPublisherSessionState::Ended {
                    why: "unauthorized".to_string(),
                },
            );
            tokio::time::timeout(Duration::from_secs(5), relays.changed())
                .await
                .expect("the loop's check hands a fresh credential, with no renewal due")
                .unwrap();
            renewing.abort();
        });
        assert_ne!(
            the_dial_url_handed_over_for(&relay_instances, "provider-1"),
            first_dial_url
        );
    }

    #[test]
    fn a_renewing_relay_whose_session_ended_is_handed_a_fresh_credential() {
        let machine = RelayInstancesOverAStateDirectory::whose_provider_renews_its_credential_every(
            Duration::from_secs(3600),
        );
        let relay_instances = machine.start_a_runtime();
        relay_instances
            .add_relay("provider", provider_settings(&a_relay_url_carrying("a")))
            .unwrap();
        let first_dial_url = the_dial_url_handed_over_for(&relay_instances, "provider-1");
        report_the_publishers_session_with(
            &relay_instances,
            ExposedPortPublisherSessionState::Ended {
                why: "unauthorized".to_string(),
            },
        );
        relay_instances.hand_the_publisher_every_joined_relay();
        assert_ne!(
            the_dial_url_handed_over_for(&relay_instances, "provider-1"),
            first_dial_url,
            "a token the relay refused, or let expire, is minted again"
        );
    }

    #[test]
    fn a_credential_falling_due_is_renewed_and_handed_to_the_publisher() {
        let async_runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let machine = RelayInstancesOverAStateDirectory::whose_provider_renews_its_credential_every(
            Duration::from_millis(200),
        );
        let relay_instances = Arc::new(machine.start_a_runtime());
        relay_instances
            .add_relay("provider", provider_settings(&a_relay_url_carrying("a")))
            .unwrap();
        let first_dial_url = the_dial_url_handed_over_for(&relay_instances, "provider-1");
        let mut relays = relay_instances.relays_the_publisher_dials();
        relays.borrow_and_update();
        async_runtime.block_on(async {
            let renewing = tokio::spawn(
                RelayInstancesOfThisRuntime::renew_each_dial_credential_once_due(
                    Arc::downgrade(&relay_instances),
                    A_RENEWING_RELAYS_CREDENTIAL_IS_CHECKED_AT_LEAST_EVERY,
                ),
            );
            tokio::time::timeout(Duration::from_secs(5), relays.changed())
                .await
                .expect("the credential is renewed once due")
                .unwrap();
            renewing.abort();
        });
        let renewed_dial_url = the_dial_url_handed_over_for(&relay_instances, "provider-1");
        assert_ne!(renewed_dial_url, first_dial_url);
        assert!(renewed_dial_url.starts_with("https://relay.example/?jwt=minted-"));
    }

    #[test]
    fn a_second_instance_of_an_adapter_is_added_from_its_settings_alone_and_listed() {
        let machine = RelayInstancesOverAStateDirectory::new();
        let relay_instances = machine.start_a_runtime();

        let first = relay_instances
            .add_relay("provider", provider_settings(&a_relay_url_carrying("a")))
            .unwrap();
        let second = relay_instances
            .add_relay("provider", provider_settings(&a_relay_url_carrying("b")))
            .unwrap();
        assert_eq!(
            (first.0.as_str(), second.0.as_str()),
            ("provider-1", "provider-2")
        );

        let listing = relay_instances.list_relays();
        assert_eq!(
            listing
                .iter()
                .map(|listed| (listed.relay_id.as_str(), listed.built_in))
                .collect::<Vec<_>>(),
            [
                ("built-in-only-relay", true),
                ("provider-1", false),
                ("provider-2", false)
            ]
        );
        let second_listed = listed(&listing, "provider-2");
        assert_eq!(
            second_listed.connection_state,
            ListedRelayConnectionState::Joining,
            "joined by its adapter, and no session of the publisher's connected yet"
        );
        the_publishers_session_connects_to_every_relay_handed_over(&relay_instances);
        let listing = relay_instances.list_relays();
        let second_listed = listed(&listing, "provider-2");
        assert_eq!(second_listed.adapter, "provider");
        assert_eq!(
            second_listed.settings,
            json!({"region": "us"}).as_object().unwrap().clone()
        );
        assert_eq!(
            second_listed.secrets,
            [(
                "relay_url_carrying_a_token".to_string(),
                ListedRelaySecretPresence::Set
            )]
            .into()
        );
        assert_eq!(
            second_listed.connection_state,
            ListedRelayConnectionState::Joined
        );
        assert_eq!(
            relay_instances
                .relays_the_publisher_dials()
                .borrow()
                .iter()
                .map(|relay| relay.relay_id.0.clone())
                .collect::<Vec<_>>(),
            ["provider-1", "provider-2"]
        );
    }

    #[test]
    fn a_built_in_instance_that_cannot_join_is_listed_saying_why() {
        let machine = RelayInstancesOverAStateDirectory::new();
        let relay_instances = machine.start_a_runtime();
        assert_eq!(
            listed(&relay_instances.list_relays(), "built-in-only-relay").connection_state,
            ListedRelayConnectionState::NotJoined {
                why: Some(
                    "the relay `built-in-only-relay` could not be joined: no relay program on \
                     this machine"
                        .to_string()
                )
            }
        );
    }

    #[test]
    fn relays_json_and_every_listing_hold_no_secret() {
        let machine = RelayInstancesOverAStateDirectory::new();
        let relay_instances = machine.start_a_runtime();
        relay_instances
            .add_relay(
                "provider",
                provider_settings(&a_relay_url_carrying(URL_BORNE_TOKEN)),
            )
            .unwrap();

        let record = std::fs::read_to_string(machine.record_path()).unwrap();
        assert!(!record.contains(URL_BORNE_TOKEN), "{record}");
        assert!(
            !record.contains("relay.example"),
            "the whole URL is the secret: {record}"
        );
        let record: Vec<RecordedRelayInstance> = serde_json::from_str(&record).unwrap();
        assert_eq!(
            record
                .iter()
                .map(|recorded| recorded.relay_id.as_str())
                .collect::<Vec<_>>(),
            ["built-in-only-relay", "provider-1"]
        );

        let listing = serde_json::to_string(&relay_instances.list_relays()).unwrap();
        assert!(!listing.contains("relay.example"), "{listing}");
    }

    #[test]
    fn a_secret_survives_a_restart_from_the_store_and_reaches_the_adapter_again() {
        let machine = RelayInstancesOverAStateDirectory::new();
        machine
            .start_a_runtime()
            .add_relay(
                "provider",
                provider_settings(&a_relay_url_carrying(URL_BORNE_TOKEN)),
            )
            .unwrap();

        let restarted = machine.start_a_runtime();
        assert_eq!(
            machine.last_join_seen(),
            (
                "provider-1".to_string(),
                a_relay_url_carrying(URL_BORNE_TOKEN),
                Some("us".to_string())
            )
        );
        assert_eq!(
            listed(&restarted.list_relays(), "provider-1").secrets["relay_url_carrying_a_token"],
            ListedRelaySecretPresence::Set
        );
    }

    #[test]
    fn configure_relay_replaces_what_it_names_rotating_a_secret_by_overwriting_it() {
        let machine = RelayInstancesOverAStateDirectory::new();
        let relay_instances = machine.start_a_runtime();
        relay_instances
            .add_relay(
                "provider",
                provider_settings(&a_relay_url_carrying("before")),
            )
            .unwrap();

        relay_instances
            .configure_relay(
                "provider-1",
                json!({"relay_url_carrying_a_token": a_relay_url_carrying("after"), "region": null})
                    .as_object()
                    .unwrap()
                    .clone(),
            )
            .unwrap();

        assert_eq!(
            machine.last_join_seen(),
            (
                "provider-1".to_string(),
                a_relay_url_carrying("after"),
                None
            )
        );
        let written = machine.every_file_written();
        assert!(written.contains("after"), "the rotated secret is stored");
        assert!(
            !written.contains("before"),
            "the old secret is overwritten: {written}"
        );
        assert_eq!(
            listed(&relay_instances.list_relays(), "provider-1").settings,
            Map::new()
        );
    }

    #[test]
    fn settings_the_adapter_refuses_add_nothing_and_configure_keeps_what_it_had() {
        let machine = RelayInstancesOverAStateDirectory::new();
        let relay_instances = machine.start_a_runtime();
        let refusal = relay_instances
            .add_relay(
                "provider",
                json!({"region": "us"}).as_object().unwrap().clone(),
            )
            .unwrap_err();
        assert!(
            refusal.to_string().contains("relay_url_carrying_a_token"),
            "{refusal}"
        );
        assert_eq!(relay_instances.list_relays().len(), 1);

        relay_instances
            .add_relay("provider", provider_settings(&a_relay_url_carrying("kept")))
            .unwrap();
        let refusal = relay_instances
            .configure_relay(
                "provider-1",
                json!({"bandwidth": 9}).as_object().unwrap().clone(),
            )
            .unwrap_err();
        assert!(refusal.to_string().contains("bandwidth"), "{refusal}");
        assert_eq!(
            machine.last_join_seen().1,
            a_relay_url_carrying("kept"),
            "it joined again with what it had"
        );
        the_publishers_session_connects_to_every_relay_handed_over(&relay_instances);
        assert_eq!(
            listed(&relay_instances.list_relays(), "provider-1").connection_state,
            ListedRelayConnectionState::Joined
        );
        let a_secret_as_a_number = relay_instances
            .configure_relay(
                "provider-1",
                json!({"relay_url_carrying_a_token": 7})
                    .as_object()
                    .unwrap()
                    .clone(),
            )
            .unwrap_err();
        assert!(a_secret_as_a_number.to_string().contains("is a string"));
    }

    #[test]
    fn a_built_in_instance_is_never_removed_and_an_added_one_goes_with_its_secrets() {
        let machine = RelayInstancesOverAStateDirectory::new();
        let relay_instances = machine.start_a_runtime();
        let refusal = relay_instances
            .remove_relay("built-in-only-relay")
            .unwrap_err();
        assert_eq!(
            refusal.to_string(),
            "the relay `built-in-only-relay` is built in and is never removed"
        );

        relay_instances
            .add_relay(
                "provider",
                provider_settings(&a_relay_url_carrying(URL_BORNE_TOKEN)),
            )
            .unwrap();
        relay_instances.remove_relay("provider-1").unwrap();
        assert_eq!(relay_instances.list_relays().len(), 1);
        assert!(
            relay_instances
                .relays_the_publisher_dials()
                .borrow()
                .is_empty()
        );
        assert!(!machine.every_file_written().contains(URL_BORNE_TOKEN));

        let refusal = relay_instances.remove_relay("provider-1").unwrap_err();
        assert!(
            refusal.to_string().contains("`built-in-only-relay`"),
            "an unknown relay is refused naming the relays: {refusal}"
        );
    }

    #[test]
    fn an_adapter_that_adds_no_instance_and_an_unknown_one_are_refused_by_name() {
        let machine = RelayInstancesOverAStateDirectory::new();
        let relay_instances = machine.start_a_runtime();
        assert_eq!(
            relay_instances
                .add_relay("built-in-only", Map::new())
                .unwrap_err()
                .to_string(),
            "no `built-in-only` relay is added: it is built in"
        );
        assert_eq!(
            relay_instances
                .add_relay("red5", Map::new())
                .unwrap_err()
                .to_string(),
            "no relay adapter is named `red5`; the adapters are `built-in-only`, `provider`"
        );
    }

    /// Everything the log carried during the second of two runs of `act` on
    /// this thread: a callsite first reached on a thread with no subscriber
    /// caches `never`, so the first run registers each, the interest cache is
    /// rebuilt against this thread's subscriber, and the second run is read.
    fn the_log_written_while(act: impl Fn()) -> String {
        #[derive(Clone, Default)]
        struct CapturedLog(Arc<StdMutex<Vec<u8>>>);
        impl std::io::Write for CapturedLog {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let captured = CapturedLog::default();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_writer({
                let captured = captured.clone();
                move || captured.clone()
            })
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            act();
            tracing::callsite::rebuild_interest_cache();
            captured.0.lock().unwrap().clear();
            act();
        });
        String::from_utf8(captured.0.lock().unwrap().clone()).unwrap()
    }

    #[test]
    fn a_url_carrying_a_token_reaches_no_log_line() {
        let log = the_log_written_while(|| {
            let machine = RelayInstancesOverAStateDirectory::new();
            let relay_instances = machine.start_a_runtime();
            relay_instances
                .add_relay(
                    "provider",
                    provider_settings(&a_relay_url_carrying(URL_BORNE_TOKEN)),
                )
                .unwrap();
            relay_instances
                .configure_relay(
                    "provider-1",
                    json!({"relay_url_carrying_a_token": a_relay_url_carrying(URL_BORNE_TOKEN), "bandwidth": 1})
                        .as_object()
                        .unwrap()
                        .clone(),
                )
                .unwrap_err();
            relay_instances.list_relays();
            drop(relay_instances);
            machine.start_a_runtime();
        });
        assert!(log.contains("provider-1"), "the log is captured: {log}");
        assert!(!log.contains(URL_BORNE_TOKEN), "{log}");
        assert!(!log.contains("relay.example"), "{log}");
    }

    /// Makes `relays.json` a directory, which no write can replace.
    fn make_the_record_unwritable(machine: &RelayInstancesOverAStateDirectory) {
        std::fs::remove_file(machine.record_path()).unwrap();
        std::fs::create_dir(machine.record_path()).unwrap();
    }

    #[test]
    fn a_change_that_cannot_be_recorded_leaves_every_relay_and_secret_as_it_was() {
        let machine = RelayInstancesOverAStateDirectory::new();
        let relay_instances = machine.start_a_runtime();
        relay_instances
            .add_relay("provider", provider_settings(&a_relay_url_carrying("held")))
            .unwrap();
        make_the_record_unwritable(&machine);

        let refusal = relay_instances
            .add_relay(
                "provider",
                provider_settings(&a_relay_url_carrying("added")),
            )
            .unwrap_err();
        assert!(
            matches!(refusal, RelayInstanceControlRefusal::NotRecorded { .. }),
            "{refusal}"
        );
        assert_eq!(relay_instances.list_relays().len(), 2);
        assert!(
            !machine.every_file_written().contains("added"),
            "no secret is left behind"
        );

        let refusal = relay_instances
            .configure_relay(
                "provider-1",
                json!({"relay_url_carrying_a_token": a_relay_url_carrying("rotated"), "region": "eu"})
                    .as_object()
                    .unwrap()
                    .clone(),
            )
            .unwrap_err();
        assert!(
            matches!(refusal, RelayInstanceControlRefusal::NotRecorded { .. }),
            "{refusal}"
        );
        let written = machine.every_file_written();
        assert!(
            written.contains("held") && !written.contains("rotated"),
            "{written}"
        );
        assert_eq!(
            machine.last_join_seen(),
            (
                "provider-1".to_string(),
                a_relay_url_carrying("held"),
                Some("us".to_string())
            ),
            "it joined again with what it had"
        );
        the_publishers_session_connects_to_every_relay_handed_over(&relay_instances);
        let listing = relay_instances.list_relays();
        let held = listed(&listing, "provider-1");
        assert_eq!(
            held.settings,
            json!({"region": "us"}).as_object().unwrap().clone()
        );
        assert_eq!(held.connection_state, ListedRelayConnectionState::Joined);

        assert!(matches!(
            relay_instances.remove_relay("provider-1").unwrap_err(),
            RelayInstanceControlRefusal::NotRecorded { .. }
        ));
        assert_eq!(relay_instances.list_relays().len(), 2);
        assert_eq!(
            relay_instances.relays_the_publisher_dials().borrow().len(),
            1
        );
    }

    /// A store that answers reads and refuses every write.
    struct SecretStoreRefusingEveryWrite;

    impl RelaySecretStore for SecretStoreRefusingEveryWrite {
        fn read(&self, _: &str, _: &str) -> Result<Option<SecretString>, String> {
            Ok(None)
        }

        fn write(&self, _: &str, _: &str, _: &SecretString) -> Result<(), String> {
            Err("the store is locked".to_string())
        }

        fn delete(&self, _: &str, _: &str) -> Result<(), String> {
            Ok(())
        }
    }

    #[test]
    fn a_relay_whose_secret_cannot_be_stored_is_not_added() {
        let machine = RelayInstancesOverAStateDirectory::new();
        let relay_instances = RelayInstancesOfThisRuntime::join_every_relay_instance(
            vec![Box::new(ProviderRelayAdapterInstanceMaker {
                joins_seen: Arc::clone(&machine.joins_seen),
                credential_renewed_every: None,
                credentials_minted: Arc::default(),
            })],
            RelayInstancesRecordFile::at(machine.record_path()),
            Box::new(SecretStoreRefusingEveryWrite),
            MachineName::from_a_hostname("desk").unwrap(),
        );
        let refusal = relay_instances
            .add_relay("provider", provider_settings(&a_relay_url_carrying("lost")))
            .unwrap_err();
        assert_eq!(
            refusal.to_string(),
            "the secret `relay_url_carrying_a_token` of the relay `provider-1` was not stored: \
             the store is locked"
        );
        assert!(relay_instances.list_relays().is_empty());
        assert!(
            relay_instances
                .relays_the_publisher_dials()
                .borrow()
                .is_empty()
        );
        assert!(!machine.record_path().exists());
    }

    #[test]
    fn while_relays_json_cannot_be_read_no_change_touches_a_recorded_relays_id_or_secret() {
        let machine = RelayInstancesOverAStateDirectory::new();
        machine
            .start_a_runtime()
            .add_relay(
                "provider",
                provider_settings(&a_relay_url_carrying("recorded")),
            )
            .unwrap();
        std::fs::write(machine.record_path(), "{not json").unwrap();
        let relay_instances = machine.start_a_runtime();
        let joins_before = machine.joins_seen.lock().unwrap().len();

        for refusal in [
            relay_instances
                .add_relay(
                    "provider",
                    provider_settings(&a_relay_url_carrying("added")),
                )
                .unwrap_err(),
            relay_instances
                .configure_relay(
                    "built-in-only-relay",
                    json!({"label": "x"}).as_object().unwrap().clone(),
                )
                .unwrap_err(),
        ] {
            assert!(
                matches!(refusal, RelayInstanceControlRefusal::NotRecorded { .. }),
                "{refusal}"
            );
        }
        assert_eq!(
            machine.joins_seen.lock().unwrap().len(),
            joins_before,
            "nothing joined"
        );
        let written = machine.every_file_written();
        assert!(
            written.contains("recorded") && !written.contains("added"),
            "the recorded relay's secret is untouched: {written}"
        );
    }
}
