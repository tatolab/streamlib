// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The machine's local relay: `moq-relay` from the runtime unit, run as two
//! children of `tatolabd`, one per [`LocalRelayListener`]. Each instance owns
//! a directory holding its child's configuration, certificate, admission
//! socket and pidfile; `tatolabd` writes the certificate and answers the
//! admission, and the child runs no code of ours.

mod local_relay_admission;
mod local_relay_certificate;
mod local_relay_child_configuration;
mod local_relay_child_supervision;
mod local_relay_instance_settings;
mod local_relay_listener;

use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use streamlib_relay_adapter_sdk::secrecy::SecretString;
use streamlib_relay_adapter_sdk::url::Url;
use streamlib_relay_adapter_sdk::{
    BroadcastPath, JoinedRelay, MachineName, OutputPortExposureLevel, RelayAdapter,
    RelayAdapterKind, RelayAdapterRefusal, RelayConnectionState, RelayConnectionStateReceiver,
    RelayDialCredential, RelayInstanceId, RelaySettingsJsonSchema,
    relay_instance_settings_from_json,
};

pub use local_relay_admission::{
    LocalRelayAdmission, LocalRelayAdmissionAnswer, LocalRelayAdmissionResponder,
};
pub use local_relay_certificate::{
    LOCAL_RELAY_CERTIFICATE_REGENERATION_AGE, LOCAL_RELAY_CERTIFICATE_VALIDITY,
    LocalRelayCertificateFiles, ProvisionedLocalRelayCertificate,
};
pub use local_relay_child_configuration::LocalRelayChildConfiguration;
pub use local_relay_child_supervision::{LocalRelayChildLaunch, LocalRelayChildSupervisor};
pub use local_relay_instance_settings::LocalRelayInstanceSettings;
pub use local_relay_listener::LocalRelayListener;

/// The adapter kind every local relay instance is joined through.
pub const LOCAL_RELAY_ADAPTER_KIND: RelayAdapterKind = RelayAdapterKind("local");

/// The `moq-relay` executable's file name, beside `tatolabd` in a runtime
/// unit's `bin/`.
pub const LOCAL_RELAY_EXECUTABLE_FILE_NAME: &str = "moq-relay";

/// How many random bytes the runtime's publish token carries.
const RUNTIME_RELAY_PUBLISH_TOKEN_BYTES: usize = 32;

/// How often a running instance asks whether its certificate is due.
const CERTIFICATE_AGE_CHECK_INTERVAL: Duration = Duration::from_secs(60 * 60);

/// Only the owner reaches an instance's directory.
const OWNER_ONLY_DIRECTORY_MODE: u32 = 0o700;

/// The token `tatolabd` mints for itself at its start: the one credential a
/// local relay lets publish. Held in memory alone, never written to disk.
pub fn mint_the_runtimes_relay_publish_token() -> SecretString {
    let mut random_bytes = [0u8; RUNTIME_RELAY_PUBLISH_TOKEN_BYTES];
    aws_lc_rs::rand::fill(&mut random_bytes)
        .expect("the system's random source answers a runtime starting");
    SecretString::from(
        random_bytes
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
    )
}

/// What every local relay instance of one `tatolabd` shares.
#[derive(Clone)]
pub struct LocalRelaySharedByBothInstances {
    /// The `moq-relay` executable, canonical.
    pub relay_executable: PathBuf,
    /// The directory each instance keeps its own directory in.
    pub relays_directory: PathBuf,
    /// The machine the certificates name beside loopback.
    pub machine_name: MachineName,
    /// The one token that may publish.
    pub runtime_publish_token: SecretString,
    /// Where the admission sockets are answered.
    pub async_runtime: tokio::runtime::Handle,
}

/// One local relay instance: one listener, one `moq-relay` child.
pub struct LocalRelayAdapter {
    listener: LocalRelayListener,
    relay_id: RelayInstanceId,
    shared: LocalRelaySharedByBothInstances,
    connection_state: tokio::sync::watch::Sender<RelayConnectionState>,
    joined: Option<JoinedLocalRelayChild>,
}

/// What a joined instance runs, ended in this field order: the child, then
/// its admission, then its certificate's rotation.
struct JoinedLocalRelayChild {
    supervisor: LocalRelayChildSupervisor,
    _admission_responder: LocalRelayAdmissionResponder,
    certificate_rotation: LocalRelayCertificateRotation,
}

impl LocalRelayAdapter {
    /// The instance for `listener`, not yet joined.
    pub fn for_the_listener(
        listener: LocalRelayListener,
        shared: LocalRelaySharedByBothInstances,
    ) -> Self {
        let (connection_state, _) = tokio::sync::watch::channel(RelayConnectionState::NotJoined);
        Self {
            listener,
            relay_id: listener.relay_id(),
            shared,
            connection_state,
            joined: None,
        }
    }

    /// This instance's own directory.
    pub fn instance_directory(&self) -> PathBuf {
        self.shared.relays_directory.join(&self.relay_id.0)
    }

    /// The SHA-256 of the certificate the child serves, lowercase hex, while
    /// joined.
    pub fn served_certificate_sha256_fingerprint_hex(&self) -> Option<String> {
        self.joined
            .as_ref()
            .map(|joined| joined.certificate_rotation.served_sha256_fingerprint_hex())
    }

    /// The running child's process id, while one runs.
    pub fn running_child_process_id(&self) -> Option<u32> {
        self.joined
            .as_ref()
            .and_then(|joined| joined.supervisor.running_child_process_id())
    }

    fn subject_alt_names(&self) -> Vec<String> {
        vec![
            "localhost".to_string(),
            "127.0.0.1".to_string(),
            "::1".to_string(),
            self.shared.machine_name.name().to_string(),
        ]
    }

    fn join_refused(&self, why: impl std::fmt::Display) -> RelayAdapterRefusal {
        RelayAdapterRefusal::JoinFailed {
            relay_id: self.relay_id.clone(),
            why: why.to_string(),
        }
    }

    fn start_the_child(
        &self,
        settings: &LocalRelayInstanceSettings,
    ) -> Result<(JoinedLocalRelayChild, JoinedRelay), RelayAdapterRefusal> {
        let port = settings.port.unwrap_or(self.listener.default_port());
        let instance_directory = self.instance_directory();
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(OWNER_ONLY_DIRECTORY_MODE)
            .create(&instance_directory)
            .map_err(|failure| {
                self.join_refused(format!(
                    "its directory {} could not be created: {failure}",
                    instance_directory.display()
                ))
            })?;

        let certificate_files = LocalRelayCertificateFiles::in_the_directory(&instance_directory);
        let certificate_rotation = LocalRelayCertificateRotation::start(
            self.relay_id.clone(),
            certificate_files.clone(),
            self.subject_alt_names(),
        )
        .map_err(|failure| self.join_refused(failure))?;

        let admission_socket_path = instance_directory.join("admission.sock");
        let admission_responder = LocalRelayAdmissionResponder::serve(
            Arc::new(LocalRelayAdmission::granting_publish_to_the_token(
                self.shared.runtime_publish_token.clone(),
            )),
            &admission_socket_path,
            &self.shared.async_runtime,
        )
        .map_err(|failure| self.join_refused(failure))?;

        let configuration_path = instance_directory.join("relay.toml");
        let configuration = LocalRelayChildConfiguration {
            bind_address: self.listener.bind_address(port),
            certificate_path: certificate_files.certificate_path,
            private_key_path: certificate_files.private_key_path,
            admission_socket_path,
        };
        std::fs::write(&configuration_path, configuration.rendered_as_toml()).map_err(
            |failure| {
                self.join_refused(format!(
                    "its configuration {} could not be written: {failure}",
                    configuration_path.display()
                ))
            },
        )?;

        self.connection_state
            .send_replace(RelayConnectionState::Joining);
        let supervisor = LocalRelayChildSupervisor::start(
            LocalRelayChildLaunch {
                relay_id: self.relay_id.clone(),
                relay_executable: self.shared.relay_executable.clone(),
                configuration_path,
                pidfile_path: instance_directory.join("moq-relay.pid"),
            },
            self.connection_state.clone(),
        )
        .map_err(|failure| {
            self.join_refused(format!("its supervisor could not be started: {failure}"))
        })?;

        let dial_url = Url::parse(&format!(
            "http://{}/",
            self.listener.address_dialed_on_this_machine(port)
        ))
        .expect("a socket address makes an http URL");
        Ok((
            JoinedLocalRelayChild {
                supervisor,
                _admission_responder: admission_responder,
                certificate_rotation,
            },
            JoinedRelay { dial_url },
        ))
    }
}

impl RelayAdapter for LocalRelayAdapter {
    fn kind(&self) -> RelayAdapterKind {
        LOCAL_RELAY_ADAPTER_KIND
    }

    fn relay_id(&self) -> &RelayInstanceId {
        &self.relay_id
    }

    fn settings_schema(&self) -> serde_json::Value {
        LocalRelayInstanceSettings::processor_config_schema_document()
    }

    fn join(&mut self, settings: serde_json::Value) -> Result<JoinedRelay, RelayAdapterRefusal> {
        let settings: LocalRelayInstanceSettings =
            relay_instance_settings_from_json(&self.relay_id, settings)?;
        self.leave();
        let (joined, joined_relay) = self.start_the_child(&settings)?;
        self.joined = Some(joined);
        Ok(joined_relay)
    }

    fn leave(&mut self) {
        if let Some(mut joined) = self.joined.take() {
            joined.supervisor.stop();
        }
        self.connection_state
            .send_replace(RelayConnectionState::NotJoined);
    }

    fn levels_served(&self) -> &[OutputPortExposureLevel] {
        self.listener.levels_served()
    }

    fn broadcast_root(&self, machine: &MachineName) -> BroadcastPath {
        BroadcastPath::of_the_machine_under_the_root("", machine)
    }

    fn credential_for_dial(&self) -> Option<RelayDialCredential> {
        Some(RelayDialCredential::TokenInTheDialUrlQuery(
            self.shared.runtime_publish_token.clone(),
        ))
    }

    fn events(&self) -> RelayConnectionStateReceiver {
        self.connection_state.subscribe()
    }
}

impl Drop for LocalRelayAdapter {
    fn drop(&mut self) {
        self.leave();
    }
}

/// One instance's certificate, issued as it joins and replaced once due,
/// checked every [`CERTIFICATE_AGE_CHECK_INTERVAL`] until dropped.
struct LocalRelayCertificateRotation {
    served: Arc<Mutex<ProvisionedLocalRelayCertificate>>,
    stop_rotating: Option<mpsc::Sender<()>>,
    rotation_thread: Option<JoinHandle<()>>,
}

impl LocalRelayCertificateRotation {
    fn start(
        relay_id: RelayInstanceId,
        certificate_files: LocalRelayCertificateFiles,
        subject_alt_names: Vec<String>,
    ) -> Result<Self, String> {
        let served = Arc::new(Mutex::new(
            certificate_files
                .write_a_fresh_certificate_issued_now(&subject_alt_names)
                .map_err(|refusal| refusal.to_string())?,
        ));
        let (stop_rotating, stop_requested) = mpsc::channel::<()>();
        let rotation_thread = std::thread::Builder::new()
            .name(format!("{relay_id}-certificate"))
            .spawn({
                let served = Arc::clone(&served);
                move || {
                    while let Err(mpsc::RecvTimeoutError::Timeout) =
                        stop_requested.recv_timeout(CERTIFICATE_AGE_CHECK_INTERVAL)
                    {
                        replace_the_certificate_once_due(
                            &relay_id,
                            &certificate_files,
                            &subject_alt_names,
                            &served,
                        );
                    }
                }
            })
            .map_err(|failure| format!("its certificate rotation could not start: {failure}"))?;
        Ok(Self {
            served,
            stop_rotating: Some(stop_rotating),
            rotation_thread: Some(rotation_thread),
        })
    }

    fn served_sha256_fingerprint_hex(&self) -> String {
        self.served
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .sha256_fingerprint_hex
            .clone()
    }
}

fn replace_the_certificate_once_due(
    relay_id: &RelayInstanceId,
    certificate_files: &LocalRelayCertificateFiles,
    subject_alt_names: &[String],
    served: &Mutex<ProvisionedLocalRelayCertificate>,
) {
    let mut served = served
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if !served.is_due_for_regeneration_now() {
        return;
    }
    match certificate_files.write_a_fresh_certificate_issued_now(subject_alt_names) {
        Ok(fresh) => {
            tracing::info!(
                relay = %relay_id,
                "the local relay's certificate is replaced; it now serves sha256 {}",
                fresh.sha256_fingerprint_hex
            );
            *served = fresh;
        }
        Err(refusal) => tracing::error!(relay = %relay_id, "{refusal}; it is tried again later"),
    }
}

impl Drop for LocalRelayCertificateRotation {
    fn drop(&mut self) {
        drop(self.stop_rotating.take());
        if let Some(rotation_thread) = self.rotation_thread.take() {
            let _ = rotation_thread.join();
        }
    }
}

/// The `moq-relay` executable beside `canonical_executable`, as a runtime
/// unit lays it out beside `tatolabd`.
pub fn local_relay_executable_beside(canonical_executable: &Path) -> Option<PathBuf> {
    canonical_executable
        .parent()
        .map(|directory| directory.join(LOCAL_RELAY_EXECUTABLE_FILE_NAME))
}

#[cfg(test)]
mod tests {
    use super::*;
    use streamlib_relay_adapter_sdk::secrecy::ExposeSecret;

    #[test]
    fn each_minted_token_is_fresh_and_carries_thirty_two_random_bytes() {
        let first = mint_the_runtimes_relay_publish_token();
        let second = mint_the_runtimes_relay_publish_token();
        assert_eq!(
            first.expose_secret().len(),
            RUNTIME_RELAY_PUBLISH_TOKEN_BYTES * 2
        );
        assert_ne!(first.expose_secret(), second.expose_secret());
    }

    #[test]
    fn the_relay_is_found_beside_the_executable() {
        assert_eq!(
            local_relay_executable_beside(Path::new("/opt/tatolab/bin/tatolabd")),
            Some(PathBuf::from("/opt/tatolab/bin/moq-relay"))
        );
    }
}
