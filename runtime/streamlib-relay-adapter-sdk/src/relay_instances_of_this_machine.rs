// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The calls the app and the CLI drive: the relay-adapter catalog, and adding,
//! configuring, removing and listing relay instances. `tatolabd` answers them;
//! its local API serves them.

use serde::Serialize;
use serde_json::{Map, Value};
use streamlib_runtime_client_contract::local_api_wire_contract::ListedRelayInstance;

use crate::relay_secret_setting::secret_setting_names_in_the_schema;
use crate::{RelayAdapter, RelayAdapterRefusal, RelayInstanceId};

/// One adapter as the relay-adapter catalog lists it: enough for a form that
/// knows no adapter.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RelayAdapterCatalogEntry {
    /// The adapter's kind, as `add_relay` takes it.
    pub kind: String,
    /// What the adapter joins.
    pub doc: String,
    /// The JSON Schema of an instance's settings, each secret `writeOnly`.
    pub settings_schema: Value,
}

impl RelayAdapterCatalogEntry {
    /// The entry of the adapter `A`.
    pub fn of_the_adapter<A: RelayAdapter>() -> Self {
        Self {
            kind: A::kind().to_string(),
            doc: A::doc().to_string(),
            settings_schema: A::settings_schema(),
        }
    }

    /// The settings of this adapter that are secrets.
    pub fn secret_setting_names(&self) -> Vec<String> {
        secret_setting_names_in_the_schema(&self.settings_schema)
    }
}

/// A relay instance present on every machine with no one adding it.
#[derive(Debug, Clone, PartialEq)]
pub struct BuiltInRelayInstance {
    /// Its id.
    pub relay_id: RelayInstanceId,
    /// The settings it is joined with until someone configures it.
    pub settings: Map<String, Value>,
}

/// Makes the instances of one adapter. One per adapter crate, handed to
/// `tatolabd` as it starts.
pub trait RelayAdapterInstanceMaker: Send + Sync {
    /// The adapter's catalog entry.
    fn catalog_entry(&self) -> RelayAdapterCatalogEntry;

    /// The instances of this adapter every machine has, never removed.
    fn built_in_instances(&self) -> Vec<BuiltInRelayInstance> {
        Vec::new()
    }

    /// Why `add_relay` makes no instance of this adapter, when it makes none.
    fn why_no_instance_is_added(&self) -> Option<String> {
        None
    }

    /// The instance `relay_id`, not yet joined.
    fn make_the_instance(
        &self,
        relay_id: &RelayInstanceId,
    ) -> Result<Box<dyn RelayAdapter>, RelayAdapterRefusal>;
}

/// Why a call on the machine's relay instances was refused.
#[derive(Debug, thiserror::Error)]
pub enum RelayInstanceControlRefusal {
    #[error("no relay adapter is named `{adapter}`; the adapters are {known}")]
    UnknownAdapter { adapter: String, known: String },
    #[error("no `{adapter}` relay is added: {why}")]
    AdapterAddsNoInstance { adapter: String, why: String },
    #[error("no relay is named `{relay_id}`; the relays are {known}")]
    UnknownRelay { relay_id: String, known: String },
    #[error("the relay `{relay_id}` is built in and is never removed")]
    BuiltInRelayIsNeverRemoved { relay_id: String },
    #[error(transparent)]
    RefusedByItsAdapter(#[from] RelayAdapterRefusal),
    #[error("the relay `{relay_id}` was not recorded, so nothing changed: {why}")]
    NotRecorded { relay_id: String, why: String },
    #[error("the secret `{setting}` of the relay `{relay_id}` was not stored: {why}")]
    SecretNotStored {
        relay_id: String,
        setting: String,
        why: String,
    },
}

/// The machine's relay instances, as the app and the CLI drive them.
pub trait RelayInstancesOfThisMachine: Send + Sync {
    /// Every adapter compiled into the runtime.
    fn relay_adapter_catalog(&self) -> Vec<RelayAdapterCatalogEntry>;

    /// Add and join an instance of `adapter` with `settings`, its secrets
    /// among them; nothing is recorded unless it joins.
    fn add_relay(
        &self,
        adapter: &str,
        settings: Map<String, Value>,
    ) -> Result<RelayInstanceId, RelayInstanceControlRefusal>;

    /// Replace each of `settings` on the instance `relay_id` — a secret
    /// replaced in the store, a `null` returning a setting to its default —
    /// and join it again with them, or keep what it had.
    fn configure_relay(
        &self,
        relay_id: &str,
        settings: Map<String, Value>,
    ) -> Result<(), RelayInstanceControlRefusal>;

    /// Leave the instance `relay_id` and forget it and its secrets.
    fn remove_relay(&self, relay_id: &str) -> Result<(), RelayInstanceControlRefusal>;

    /// Every instance, each secret listed only as set or unset.
    fn list_relays(&self) -> Vec<ListedRelayInstance>;
}
