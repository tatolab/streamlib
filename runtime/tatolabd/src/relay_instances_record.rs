// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `<state>/relays.json`: every relay instance the runtime joins at its start,
//! `[{relay_id, adapter, settings}]`, holding no secret.

#[cfg(test)]
use std::path::Path;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use streamlib_runtime_client_contract::owner_only_file_written_whole::write_an_owner_only_file_whole;

/// One relay instance as `relays.json` records it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RecordedRelayInstance {
    pub(crate) relay_id: String,
    pub(crate) adapter: String,
    /// Its settings, every secret left out.
    pub(crate) settings: Map<String, Value>,
}

/// The file the relay instances are recorded in.
pub(crate) struct RelayInstancesRecordFile {
    path: PathBuf,
}

impl RelayInstancesRecordFile {
    /// The record at `path`.
    pub(crate) fn at(path: PathBuf) -> Self {
        Self { path }
    }

    #[cfg(test)]
    fn path(&self) -> &Path {
        &self.path
    }

    /// Every recorded instance; none when the file is absent, refused naming
    /// its path when it cannot be read.
    pub(crate) fn read(&self) -> Result<Vec<RecordedRelayInstance>, String> {
        match std::fs::read(&self.path) {
            Ok(record_bytes) => serde_json::from_slice(&record_bytes).map_err(|unreadable| {
                format!("{} cannot be read: {unreadable}", self.path.display())
            }),
            Err(not_read) if not_read.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(not_read) => Err(format!(
                "{} cannot be read: {not_read}",
                self.path.display()
            )),
        }
    }

    /// Record exactly `instances`, whole or not at all, owner-only.
    pub(crate) fn write<'recorded>(
        &self,
        instances: impl IntoIterator<Item = &'recorded RecordedRelayInstance>,
    ) -> Result<(), String> {
        let instances: Vec<&RecordedRelayInstance> = instances.into_iter().collect();
        let mut record_json = serde_json::to_vec_pretty(&instances)
            .map_err(|cannot_serialize| cannot_serialize.to_string())?;
        record_json.push(b'\n');
        write_an_owner_only_file_whole(&self.path, &record_json).map_err(|not_written| {
            format!("{} was not written: {not_written}", self.path.display())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_absent_record_holds_no_instance_and_a_written_one_reads_back() {
        let directory = tempfile::tempdir().unwrap();
        let record_file = RelayInstancesRecordFile::at(directory.path().join("relays.json"));
        assert_eq!(record_file.read().unwrap(), []);
        let instances = vec![RecordedRelayInstance {
            relay_id: "local-loopback".to_string(),
            adapter: "local".to_string(),
            settings: serde_json::json!({"port": 4443})
                .as_object()
                .unwrap()
                .clone(),
        }];
        record_file.write(&instances).unwrap();
        assert_eq!(record_file.read().unwrap(), instances);
        assert!(
            std::fs::read_to_string(record_file.path())
                .unwrap()
                .trim_start()
                .starts_with('['),
            "the record is a list of instances"
        );
    }

    #[test]
    fn an_unreadable_record_is_refused_naming_its_path() {
        let directory = tempfile::tempdir().unwrap();
        let record_file = RelayInstancesRecordFile::at(directory.path().join("relays.json"));
        std::fs::write(record_file.path(), "{not json").unwrap();
        let refusal = record_file.read().unwrap_err();
        assert!(refusal.contains("relays.json"), "{refusal}");
    }
}
