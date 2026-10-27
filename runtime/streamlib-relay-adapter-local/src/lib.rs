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
    BroadcastPath, BuiltInRelayInstance, JoinedRelay, MachineName, OutputPortExposureLevel,
    RelayAdapter, RelayAdapterCatalogEntry, RelayAdapterInstanceMaker, RelayAdapterKind,
    RelayAdapterRefusal, RelayAdapterSettingField, RelayConnectionState,
    RelayConnectionStateReceiver, RelayDialCredential, RelayInstanceId,
    relay_adapter_setting_fields_of, relay_instance_settings_from_json,
};

pub use local_relay_admission::{
    LocalRelayAdmission, LocalRelayAdmissionAnswer, LocalRelayAdmissionResponder,
};
pub use local_relay_certificate::{
    LOCAL_RELAY_CERTIFICATE_REGENERATION_AGE, LOCAL_RELAY_CERTIFICATE_VALIDITY,
    LocalRelayCertificateAndKeyFile, LocalRelayCertificateRefusal,
    ProvisionedLocalRelayCertificate,
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
pub fn mint_the_runtimes_relay_publish_token()
-> Result<SecretString, RuntimeRelayPublishTokenRefusal> {
    let mut random_bytes = [0u8; RUNTIME_RELAY_PUBLISH_TOKEN_BYTES];
    aws_lc_rs::rand::fill(&mut random_bytes)
        .map_err(|_| RuntimeRelayPublishTokenRefusal::NoRandomSource)?;
    Ok(SecretString::from(lowercase_hex(&random_bytes)))
}

/// Why the runtime's publish token could not be minted.
#[derive(Debug, thiserror::Error)]
pub enum RuntimeRelayPublishTokenRefusal {
    #[error("the system's random source did not answer, so no publish token can be minted")]
    NoRandomSource,
}

/// `bytes` as lowercase hex.
pub(crate) fn lowercase_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut hex = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// The value behind `mutex`, whichever thread panicked holding it last.
pub(crate) fn lock_ignoring_poison<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
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

/// Each local relay listener with the settings it is built in with.
pub type SettingsOfEachLocalRelayListener = Vec<(
    LocalRelayListener,
    serde_json::Map<String, serde_json::Value>,
)>;

/// Each listener with no settings: its default port.
pub fn default_settings_of_each_local_relay_listener() -> SettingsOfEachLocalRelayListener {
    LocalRelayListener::BOTH
        .into_iter()
        .map(|listener| (listener, serde_json::Map::new()))
        .collect()
}

/// Makes the local relay's two instances, one per [`LocalRelayListener`], and
/// never a third.
pub struct LocalRelayAdapterInstanceMaker {
    shared_or_why_no_local_relay_runs: Result<LocalRelaySharedByBothInstances, String>,
    settings_of_each_listener: SettingsOfEachLocalRelayListener,
}

impl LocalRelayAdapterInstanceMaker {
    /// The maker of instances sharing `shared`, each listener built in with
    /// its settings in `settings_of_each_listener`.
    pub fn sharing(
        shared: LocalRelaySharedByBothInstances,
        settings_of_each_listener: SettingsOfEachLocalRelayListener,
    ) -> Self {
        Self {
            shared_or_why_no_local_relay_runs: Ok(shared),
            settings_of_each_listener,
        }
    }

    /// The maker on a machine where no local relay runs, for `why`: both
    /// listeners are still listed, and neither joins.
    pub fn running_no_local_relay(why: String) -> Self {
        Self {
            shared_or_why_no_local_relay_runs: Err(why),
            settings_of_each_listener: default_settings_of_each_local_relay_listener(),
        }
    }
}

impl RelayAdapterInstanceMaker for LocalRelayAdapterInstanceMaker {
    fn catalog_entry(&self) -> RelayAdapterCatalogEntry {
        RelayAdapterCatalogEntry::of_the_adapter::<LocalRelayAdapter>()
    }

    fn built_in_instances(&self) -> Vec<BuiltInRelayInstance> {
        self.settings_of_each_listener
            .iter()
            .map(|(listener, settings)| BuiltInRelayInstance {
                relay_id: listener.relay_id(),
                settings: settings.clone(),
            })
            .collect()
    }

    fn why_no_instance_is_added(&self) -> Option<String> {
        Some(format!(
            "the local relay is built in: its listeners `{}` and `{}` are on every machine, and \
             where a listener binds is never a setting",
            LocalRelayListener::Loopback.relay_id(),
            LocalRelayListener::EveryInterface.relay_id()
        ))
    }

    fn make_the_instance(
        &self,
        relay_id: &RelayInstanceId,
    ) -> Result<Box<dyn RelayAdapter>, RelayAdapterRefusal> {
        let listener = LocalRelayListener::BOTH
            .into_iter()
            .find(|listener| &listener.relay_id() == relay_id)
            .ok_or_else(|| RelayAdapterRefusal::JoinFailed {
                relay_id: relay_id.clone(),
                why: "the local relay has no listener by that id".to_string(),
            })?;
        let shared = self
            .shared_or_why_no_local_relay_runs
            .as_ref()
            .map_err(|why| RelayAdapterRefusal::JoinFailed {
                relay_id: relay_id.clone(),
                why: format!("no local relay runs: {why}"),
            })?;
        Ok(Box::new(LocalRelayAdapter::for_the_listener(
            listener,
            shared.clone(),
        )))
    }
}

/// One local relay instance: one listener, one `moq-relay` child.
pub struct LocalRelayAdapter {
    listener: LocalRelayListener,
    relay_id: RelayInstanceId,
    shared: LocalRelaySharedByBothInstances,
    connection_state: tokio::sync::watch::Sender<RelayConnectionState>,
    joined: Option<JoinedLocalRelayChild>,
}

/// What a joined instance runs, ended as it drops in this field order: the
/// child, then its admission, then its certificate's rotation.
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

    fn provision_the_instance_and_start_its_supervised_child(
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

        let certificate_and_key_file =
            LocalRelayCertificateAndKeyFile::in_the_directory(&instance_directory);
        let certificate_rotation = LocalRelayCertificateRotation::start(
            self.relay_id.clone(),
            certificate_and_key_file.clone(),
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
            certificate_and_key_path: certificate_and_key_file.certificate_and_key_path,
            admission_socket_path,
        };
        let rendered_configuration = configuration
            .rendered_as_toml()
            .map_err(|failure| self.join_refused(failure))?;
        std::fs::write(&configuration_path, rendered_configuration).map_err(|failure| {
            self.join_refused(format!(
                "its configuration {} could not be written: {failure}",
                configuration_path.display()
            ))
        })?;

        self.connection_state
            .send_replace(RelayConnectionState::Joining);
        let supervisor = LocalRelayChildSupervisor::start(
            LocalRelayChildLaunch {
                relay_id: self.relay_id.clone(),
                relay_executable: self.shared.relay_executable.clone(),
                configuration_path,
                pidfile_path: instance_directory.join("moq-relay.pid"),
                address_accepting_once_up: self.listener.address_dialed_on_this_machine(port),
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
        .map_err(|failure| self.join_refused(failure))?;
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
    fn kind() -> RelayAdapterKind {
        LOCAL_RELAY_ADAPTER_KIND
    }

    fn doc() -> &'static str {
        "The machine's own relay, built in: `local-loopback` serves every exposed port to this \
         machine alone, `local-every-interface` serves public ports to any machine that reaches \
         it. Where each listens is never a setting."
    }

    fn display_name() -> &'static str {
        "Local relay"
    }

    fn display_icon_svg() -> &'static str {
        include_str!("local_relay_adapter_display_icon.svg")
    }

    fn relay_id(&self) -> &RelayInstanceId {
        &self.relay_id
    }

    fn setting_fields() -> Vec<RelayAdapterSettingField> {
        relay_adapter_setting_fields_of::<LocalRelayInstanceSettings>()
    }

    fn join(&mut self, settings: serde_json::Value) -> Result<JoinedRelay, RelayAdapterRefusal> {
        let settings: LocalRelayInstanceSettings =
            relay_instance_settings_from_json(&self.relay_id, settings)?;
        self.leave();
        match self.provision_the_instance_and_start_its_supervised_child(&settings) {
            Ok((joined, joined_relay)) => {
                tracing::info!(
                    relay = %self.relay_id,
                    "the local relay listens at {}, serving {} ports",
                    joined_relay.dial_url,
                    self.listener
                        .levels_served()
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(" and ")
                );
                self.joined = Some(joined);
                Ok(joined_relay)
            }
            Err(refusal) => {
                self.connection_state
                    .send_replace(RelayConnectionState::NotJoined);
                Err(refusal)
            }
        }
    }

    fn leave(&mut self) {
        drop(self.joined.take());
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
        Some(RelayDialCredential::token_in_the_dial_url_query(
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
        certificate_and_key_file: LocalRelayCertificateAndKeyFile,
        subject_alt_names: Vec<String>,
    ) -> Result<Self, LocalRelayCertificateRotationRefusal> {
        let served = Arc::new(Mutex::new(
            certificate_and_key_file.write_a_fresh_certificate_issued_now(&subject_alt_names)?,
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
                            &certificate_and_key_file,
                            &subject_alt_names,
                            &served,
                        );
                    }
                }
            })
            .map_err(LocalRelayCertificateRotationRefusal::ThreadNotStarted)?;
        Ok(Self {
            served,
            stop_rotating: Some(stop_rotating),
            rotation_thread: Some(rotation_thread),
        })
    }

    fn served_sha256_fingerprint_hex(&self) -> String {
        lock_ignoring_poison(&self.served)
            .sha256_fingerprint_hex
            .clone()
    }
}

/// Why an instance's certificate rotation could not start.
#[derive(Debug, thiserror::Error)]
enum LocalRelayCertificateRotationRefusal {
    #[error(transparent)]
    NotProvisioned(#[from] LocalRelayCertificateRefusal),
    #[error("its certificate rotation could not start: {0}")]
    ThreadNotStarted(std::io::Error),
}

fn replace_the_certificate_once_due(
    relay_id: &RelayInstanceId,
    certificate_and_key_file: &LocalRelayCertificateAndKeyFile,
    subject_alt_names: &[String],
    served: &Mutex<ProvisionedLocalRelayCertificate>,
) {
    let mut served = lock_ignoring_poison(served);
    if !served.is_due_for_regeneration_now() {
        return;
    }
    match certificate_and_key_file.write_a_fresh_certificate_issued_now(subject_alt_names) {
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
        let first = mint_the_runtimes_relay_publish_token().unwrap();
        let second = mint_the_runtimes_relay_publish_token().unwrap();
        assert_eq!(
            first.expose_secret().len(),
            RUNTIME_RELAY_PUBLISH_TOKEN_BYTES * 2
        );
        assert_ne!(first.expose_secret(), second.expose_secret());
    }

    #[test]
    fn bytes_render_as_lowercase_hex_two_digits_each() {
        assert_eq!(lowercase_hex(&[0x00, 0x0f, 0xab, 0xff]), "000fabff");
    }

    #[test]
    fn both_listeners_are_built_in_and_no_third_instance_is_added() {
        let maker = LocalRelayAdapterInstanceMaker::running_no_local_relay("none".to_string());
        assert_eq!(
            maker
                .built_in_instances()
                .into_iter()
                .map(|built_in| built_in.relay_id.0)
                .collect::<Vec<_>>(),
            ["local-loopback", "local-every-interface"]
        );
        let why_not = maker.why_no_instance_is_added().unwrap();
        assert!(why_not.contains("built in"), "{why_not}");
        let refusal = maker
            .make_the_instance(&RelayInstanceId("local-loopback".to_string()))
            .err()
            .unwrap();
        assert!(
            refusal.to_string().contains("no local relay runs: none"),
            "{refusal}"
        );
    }

    #[test]
    fn the_catalog_lists_the_local_adapter_with_its_port_and_no_secret() {
        let entry =
            LocalRelayAdapterInstanceMaker::running_no_local_relay(String::new()).catalog_entry();
        assert_eq!(entry.kind, "local");
        assert_eq!(
            entry
                .settings
                .iter()
                .map(|setting| setting.name.as_str())
                .collect::<Vec<_>>(),
            ["port"]
        );
        assert!(entry.secret_setting_names().is_empty());
    }

    #[test]
    fn the_relay_is_found_beside_the_executable() {
        assert_eq!(
            local_relay_executable_beside(Path::new("/opt/tatolab/bin/tatolabd")),
            Some(PathBuf::from("/opt/tatolab/bin/moq-relay"))
        );
    }

    #[test]
    fn the_catalog_entry_names_the_local_relay_for_people_and_its_icon_is_an_svg_document() {
        let catalog_entry = RelayAdapterCatalogEntry::of_the_adapter::<LocalRelayAdapter>();
        assert_eq!(catalog_entry.display_name, "Local relay");
        let display_icon_svg = LocalRelayAdapter::display_icon_svg();
        assert!(display_icon_svg.contains("<svg "));
        assert!(display_icon_svg.trim_end().ends_with("</svg>"));
    }
}
