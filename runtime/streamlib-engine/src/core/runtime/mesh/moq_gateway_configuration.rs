// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Where the MoQ gateway publishes, under which prefix, and with which keys.
//!
//! Two doors. Another process hands a directory over in
//! `STREAMLIB_MOQ_GATEWAY_HANDOFF_DIR` and keeps writing into it: `relay.json`
//! names the relay and the namespace prefix, and `<runtime name>.json` can
//! override both, pause serving, and carry the per-track content keys. Once a
//! handoff directory is configured a track is sealed or not published at all —
//! never plaintext. Without one, `STREAMLIB_MESH_MOQ_RELAY_URL` and
//! `STREAMLIB_MESH_MOQ_NAMESPACE_PREFIX` configure a plaintext gateway.
//!
//! The relay URL carries the relay's token in its path, so it never reaches a
//! log line or the graph: only its host does.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use parking_lot::RwLock;
use serde::Deserialize;

use crate::core::runtime::mesh::moq_secure_object_envelope::MoqGatewayContentKey;

/// The directory another process hands the gateway its relay and keys in.
pub(crate) const MOQ_GATEWAY_HANDOFF_DIRECTORY_ENVIRONMENT_VARIABLE: &str =
    "STREAMLIB_MOQ_GATEWAY_HANDOFF_DIR";
/// The relay URL a gateway publishes to when no handoff directory is set.
pub(crate) const MOQ_RELAY_URL_ENVIRONMENT_VARIABLE: &str = "STREAMLIB_MESH_MOQ_RELAY_URL";
/// The namespace prefix a gateway publishes under when no handoff directory
/// is set.
pub(crate) const MOQ_NAMESPACE_PREFIX_ENVIRONMENT_VARIABLE: &str =
    "STREAMLIB_MESH_MOQ_NAMESPACE_PREFIX";
/// Dev-only: accept any relay certificate, for a local self-signed relay.
pub(crate) const MOQ_DANGER_ACCEPT_ANY_CERTIFICATE_ENVIRONMENT_VARIABLE: &str =
    "STREAMLIB_MESH_MOQ_DANGER_ACCEPT_ANY_CERTIFICATE";
/// Serve ports whose bags name a surface — raw pixels — to the relay.
pub(crate) const MOQ_SERVE_SURFACE_PORTS_ENVIRONMENT_VARIABLE: &str =
    "STREAMLIB_MESH_MOQ_SERVE_SURFACE_PORTS";
/// Which data path a cross-runtime link rides: `zenoh` (default) or `moq`.
pub(crate) const MESH_TRANSPORT_ENVIRONMENT_VARIABLE: &str = "STREAMLIB_MESH_TRANSPORT";

/// How often the runtime's handoff file is read again.
pub(crate) const HOW_OFTEN_THE_MOQ_GATEWAY_HANDOFF_IS_READ_AGAIN: Duration = Duration::from_secs(2);

/// Which data path a cross-runtime link rides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MeshDataTransport {
    Zenoh,
    Moq,
}

impl MeshDataTransport {
    /// The transport `STREAMLIB_MESH_TRANSPORT` names, Zenoh when unset.
    pub(crate) fn from_the_environment() -> Self {
        match std::env::var(MESH_TRANSPORT_ENVIRONMENT_VARIABLE)
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "moq" => Self::Moq,
            "" | "zenoh" => Self::Zenoh,
            other => {
                tracing::warn!(
                    "{MESH_TRANSPORT_ENVIRONMENT_VARIABLE}={other} names no transport; remote \
                     links ride zenoh"
                );
                Self::Zenoh
            }
        }
    }
}

/// `relay.json` in a handoff directory.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MoqGatewayRelayHandoffFile {
    pub(crate) relay_publish_url: String,
    pub(crate) namespace_prefix: String,
}

/// One content key as a handoff file spells it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MoqGatewayContentKeyInAHandoffFile {
    epoch: u32,
    key_base64_url: String,
}

/// `<runtime name>.json` in a handoff directory, as it is written.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MoqGatewayRuntimeHandoffFileAsWritten {
    #[serde(default)]
    relay_publish_url: Option<String>,
    #[serde(default)]
    namespace_prefix: Option<String>,
    #[serde(default)]
    paused: bool,
    #[serde(default)]
    content_keys: BTreeMap<String, Vec<MoqGatewayContentKeyInAHandoffFile>>,
}

/// `<runtime name>.json`, with every key decoded.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct MoqGatewayRuntimeHandoffFile {
    pub(crate) relay_publish_url: Option<String>,
    pub(crate) namespace_prefix: Option<String>,
    pub(crate) paused: bool,
    /// Keyed `<namespace>/<track name>`.
    pub(crate) content_keys_by_track: BTreeMap<String, Vec<MoqGatewayContentKey>>,
}

/// Read one runtime handoff file's JSON.
pub(crate) fn read_a_moq_gateway_runtime_handoff(
    json: &str,
) -> Result<MoqGatewayRuntimeHandoffFile, String> {
    let as_written: MoqGatewayRuntimeHandoffFileAsWritten = serde_json::from_str(json)
        .map_err(|not_json| format!("it is not a runtime handoff document: {not_json}"))?;
    let mut content_keys_by_track = BTreeMap::new();
    for (track, keys) in as_written.content_keys {
        let mut decoded = Vec::with_capacity(keys.len());
        for key in keys {
            let key_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(key.key_base64_url.trim_end_matches('='))
                .map_err(|not_base64| {
                    format!(
                        "the epoch {} key for {track} is not base64url: {not_base64}",
                        key.epoch
                    )
                })?;
            let key_bytes: [u8; 16] = key_bytes.try_into().map_err(|wrong: Vec<u8>| {
                format!(
                    "the epoch {} key for {track} is {} bytes, and AES-128 takes 16",
                    key.epoch,
                    wrong.len()
                )
            })?;
            decoded.push(MoqGatewayContentKey {
                epoch: key.epoch,
                key_bytes,
            });
        }
        content_keys_by_track.insert(track, decoded);
    }
    Ok(MoqGatewayRuntimeHandoffFile {
        relay_publish_url: as_written.relay_publish_url,
        namespace_prefix: as_written.namespace_prefix,
        paused: as_written.paused,
        content_keys_by_track,
    })
}

/// Read one relay handoff file's JSON.
pub(crate) fn read_a_moq_gateway_relay_handoff(
    json: &str,
) -> Result<MoqGatewayRelayHandoffFile, String> {
    serde_json::from_str(json)
        .map_err(|not_json| format!("it is not a relay handoff document: {not_json}"))
}

/// Where the gateway stands right now, as every door read it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct WhatTheMoqGatewayIsToldRightNow {
    pub(crate) relay_publish_url: Option<String>,
    pub(crate) namespace_prefix: Option<String>,
    pub(crate) paused: bool,
    /// Whether a handoff directory is configured, which forbids plaintext.
    pub(crate) objects_must_be_sealed: bool,
    pub(crate) content_keys_by_track: BTreeMap<String, Vec<MoqGatewayContentKey>>,
}

impl WhatTheMoqGatewayIsToldRightNow {
    /// The key a track is sealed with now — its highest epoch — or `None`.
    pub(crate) fn the_key_to_seal_a_track_with(
        &self,
        namespace_and_track: &str,
    ) -> Option<&MoqGatewayContentKey> {
        self.content_keys_by_track
            .get(namespace_and_track)?
            .iter()
            .max_by_key(|content_key| content_key.epoch)
    }

    /// Every key configured for a track, for opening what it carries.
    pub(crate) fn every_key_of_a_track(
        &self,
        namespace_and_track: &str,
    ) -> Vec<MoqGatewayContentKey> {
        self.content_keys_by_track
            .get(namespace_and_track)
            .cloned()
            .unwrap_or_default()
    }

    /// Whether the gateway has somewhere to publish and is not paused.
    #[cfg(test)]
    pub(crate) fn it_can_serve(&self) -> bool {
        !self.paused && self.relay_publish_url.is_some() && self.namespace_prefix.is_some()
    }
}

/// The gateway's doors, read once at start and the handoff file re-read on a
/// cadence.
pub(crate) struct MoqGatewayDoors {
    handoff_directory: Option<PathBuf>,
    this_runtimes_name: String,
    what_it_is_told: RwLock<WhatTheMoqGatewayIsToldRightNow>,
    /// Dev-only certificate bypass, read once.
    pub(crate) accept_any_relay_certificate: bool,
    /// Whether ports that carry raw pixels may be served to the relay.
    pub(crate) serve_surface_ports: bool,
}

impl MoqGatewayDoors {
    /// Read every door for `this_runtimes_name`, or `None` when none is set.
    pub(crate) fn read_for_the_runtime_named(this_runtimes_name: &str) -> Option<Arc<Self>> {
        let handoff_directory =
            std::env::var_os(MOQ_GATEWAY_HANDOFF_DIRECTORY_ENVIRONMENT_VARIABLE)
                .filter(|value| !value.is_empty())
                .map(PathBuf::from);
        let environment_relay_url = non_empty_environment_value(MOQ_RELAY_URL_ENVIRONMENT_VARIABLE);
        let environment_prefix =
            non_empty_environment_value(MOQ_NAMESPACE_PREFIX_ENVIRONMENT_VARIABLE);
        if handoff_directory.is_none() && environment_relay_url.is_none() {
            return None;
        }
        let accept_any_relay_certificate =
            environment_flag(MOQ_DANGER_ACCEPT_ANY_CERTIFICATE_ENVIRONMENT_VARIABLE);
        if accept_any_relay_certificate {
            tracing::warn!(
                "DANGER: {MOQ_DANGER_ACCEPT_ANY_CERTIFICATE_ENVIRONMENT_VARIABLE} is set, so the \
                 MoQ gateway accepts ANY relay certificate. Dev-only, for a local self-signed \
                 relay; never set it anywhere a relay could be impersonated."
            );
        }
        let doors = Arc::new(Self {
            handoff_directory,
            this_runtimes_name: this_runtimes_name.to_string(),
            what_it_is_told: RwLock::new(WhatTheMoqGatewayIsToldRightNow {
                relay_publish_url: environment_relay_url,
                namespace_prefix: environment_prefix,
                ..Default::default()
            }),
            accept_any_relay_certificate,
            serve_surface_ports: environment_flag(MOQ_SERVE_SURFACE_PORTS_ENVIRONMENT_VARIABLE),
        });
        doors.read_the_handoff_again();
        Some(doors)
    }

    /// What the gateway is told right now.
    pub(crate) fn what_it_is_told(&self) -> WhatTheMoqGatewayIsToldRightNow {
        self.what_it_is_told.read().clone()
    }

    /// Run `read` against what the gateway is told, without cloning it.
    pub(crate) fn with_what_it_is_told<T>(
        &self,
        read: impl FnOnce(&WhatTheMoqGatewayIsToldRightNow) -> T,
    ) -> T {
        read(&self.what_it_is_told.read())
    }

    /// Read the handoff directory again, keeping what was last read of any
    /// file that is absent or does not parse.
    pub(crate) fn read_the_handoff_again(&self) {
        let Some(handoff_directory) = &self.handoff_directory else {
            return;
        };
        let relay = read_a_handoff_file(
            &handoff_directory.join("relay.json"),
            read_a_moq_gateway_relay_handoff,
        );
        let runtime = read_a_handoff_file(
            &handoff_directory.join(format!("{}.json", self.this_runtimes_name)),
            read_a_moq_gateway_runtime_handoff,
        );
        let mut told = self.what_it_is_told.write();
        told.objects_must_be_sealed = true;
        if let Some(relay) = &relay {
            told.relay_publish_url = Some(relay.relay_publish_url.clone());
            told.namespace_prefix = Some(relay.namespace_prefix.clone());
        }
        if let Some(runtime) = runtime {
            if let Some(relay_publish_url) = runtime.relay_publish_url {
                told.relay_publish_url = Some(relay_publish_url);
            }
            if let Some(namespace_prefix) = runtime.namespace_prefix {
                told.namespace_prefix = Some(namespace_prefix);
            }
            told.paused = runtime.paused;
            told.content_keys_by_track = runtime.content_keys_by_track;
        }
    }
}

/// One handoff file read and parsed, or `None` — absent is ordinary, a parse
/// failure is said at debug because the writer may be mid-write.
fn read_a_handoff_file<T>(path: &Path, parse: impl FnOnce(&str) -> Result<T, String>) -> Option<T> {
    let json = std::fs::read_to_string(path).ok()?;
    match parse(&json) {
        Ok(parsed) => Some(parsed),
        Err(why_not) => {
            tracing::debug!(
                "the MoQ gateway handoff {} was not read: {why_not}",
                path.display()
            );
            None
        }
    }
}

fn non_empty_environment_value(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn environment_flag(name: &str) -> bool {
    matches!(
        std::env::var(name)
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str(),
        "1" | "true" | "yes"
    )
}

/// The host a relay URL names, for logs and the graph — never its path, which
/// carries the relay's token.
pub(crate) fn the_relay_host_of(relay_publish_url: &str) -> String {
    match url::Url::parse(relay_publish_url) {
        Ok(parsed) => match (parsed.host_str(), parsed.port()) {
            (Some(host), Some(port)) => format!("{host}:{port}"),
            (Some(host), None) => host.to_string(),
            (None, _) => "an unnamed host".to_string(),
        },
        Err(_) => "an unreadable relay URL".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A_KEY_BASE64_URL: &str = "AAECAwQFBgcICQoLDA0ODw";

    #[test]
    fn a_runtime_handoff_decodes_every_key_and_the_pause() {
        let read = read_a_moq_gateway_runtime_handoff(&format!(
            r#"{{"relayPublishUrl": "https://relay.example/tok", "namespacePrefix": "example/abc123",
                "paused": true,
                "contentKeys": {{"example/abc123/rt/17/Ticker 1/out": [
                    {{"epoch": 2, "keyBase64Url": "{A_KEY_BASE64_URL}"}},
                    {{"epoch": 3, "keyBase64Url": "{A_KEY_BASE64_URL}=="}}
                ]}}}}"#
        ))
        .expect("the handoff reads");
        assert!(read.paused);
        assert_eq!(read.namespace_prefix.as_deref(), Some("example/abc123"));
        let keys = &read.content_keys_by_track["example/abc123/rt/17/Ticker 1/out"];
        assert_eq!(keys.len(), 2);
        assert_eq!(keys[0].key_bytes, std::array::from_fn(|index| index as u8));
        assert_eq!(keys[1].epoch, 3);
    }

    #[test]
    fn a_runtime_handoff_with_nothing_but_braces_reads_as_unpaused_and_keyless() {
        let read = read_a_moq_gateway_runtime_handoff("{}").expect("it reads");
        assert_eq!(read, MoqGatewayRuntimeHandoffFile::default());
    }

    #[test]
    fn a_key_that_is_not_sixteen_bytes_is_refused_naming_its_track() {
        let refusal = read_a_moq_gateway_runtime_handoff(
            r#"{"contentKeys": {"ns/t": [{"epoch": 1, "keyBase64Url": "AAEC"}]}}"#,
        )
        .expect_err("a three-byte key is refused");
        assert!(
            refusal.contains("ns/t") && refusal.contains("16"),
            "{refusal}"
        );
    }

    #[test]
    fn a_relay_handoff_reads_its_url_and_prefix() {
        let read = read_a_moq_gateway_relay_handoff(
            r#"{"relayPublishUrl": "https://relay.example/tok", "namespacePrefix": "example/abc"}"#,
        )
        .expect("it reads");
        assert_eq!(read.namespace_prefix, "example/abc");
    }

    #[test]
    fn a_track_is_sealed_with_its_highest_epoch_key() {
        let told = WhatTheMoqGatewayIsToldRightNow {
            content_keys_by_track: BTreeMap::from([(
                "ns/t".to_string(),
                vec![
                    MoqGatewayContentKey {
                        epoch: 5,
                        key_bytes: [5; 16],
                    },
                    MoqGatewayContentKey {
                        epoch: 9,
                        key_bytes: [9; 16],
                    },
                    MoqGatewayContentKey {
                        epoch: 7,
                        key_bytes: [7; 16],
                    },
                ],
            )]),
            ..Default::default()
        };
        assert_eq!(
            told.the_key_to_seal_a_track_with("ns/t")
                .map(|key| key.epoch),
            Some(9)
        );
        assert!(told.the_key_to_seal_a_track_with("ns/other").is_none());
    }

    #[test]
    fn the_relay_host_never_carries_the_token_in_the_path() {
        assert_eq!(
            the_relay_host_of("https://draft-16.relay.example/eyJ.secret.sig"),
            "draft-16.relay.example"
        );
        assert_eq!(
            the_relay_host_of("https://localhost:4443/anything"),
            "localhost:4443"
        );
    }

    #[test]
    fn a_handoff_directory_is_reread_and_a_missing_runtime_file_keeps_the_relay() {
        let directory = std::env::temp_dir().join(format!("moq-handoff-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            directory.join("relay.json"),
            r#"{"relayPublishUrl": "https://relay.example/tok", "namespacePrefix": "example/p"}"#,
        )
        .unwrap();
        let doors = MoqGatewayDoors {
            handoff_directory: Some(directory.clone()),
            this_runtimes_name: "rt".to_string(),
            what_it_is_told: RwLock::default(),
            accept_any_relay_certificate: false,
            serve_surface_ports: false,
        };
        doors.read_the_handoff_again();
        let told = doors.what_it_is_told();
        assert!(told.objects_must_be_sealed && told.it_can_serve());

        std::fs::write(directory.join("rt.json"), r#"{"paused": true}"#).unwrap();
        doors.read_the_handoff_again();
        assert!(!doors.what_it_is_told().it_can_serve());
        std::fs::remove_dir_all(&directory).unwrap();
    }
}
