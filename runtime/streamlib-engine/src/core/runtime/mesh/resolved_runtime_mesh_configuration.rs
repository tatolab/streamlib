// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The five mesh values a runtime was told, read into what Zenoh takes.
//!
//! Everything a caller can state is refused here, at construction, rather than
//! at open — a malformed endpoint is a wiring error, and a runtime that fails
//! to open its session runs local-only instead of reporting it.

use std::ffi::OsString;

use crate::core::error::{Error, Result};
use crate::core::runtime::RuntimeMeshConfiguration;
use crate::core::runtime::mesh::runtime_mesh_endpoint::{
    DEFAULT_MESH_LISTEN_ENDPOINT, MESH_LISTEN_ENDPOINTS_ENVIRONMENT_VARIABLE,
    MESH_PEER_ENDPOINTS_ENVIRONMENT_VARIABLE, RuntimeMeshEndpoint, read_one_mesh_endpoint,
    resolve_mesh_endpoints,
};
use crate::core::runtime::mesh::runtime_mesh_name::RuntimeMeshName;
use crate::core::runtime::stated_configuration_value::what_an_environment_door_says;

/// The environment variable that turns multicast discovery off (`0`) or on
/// (`1`) when the constructor did not say.
pub(crate) const MESH_MULTICAST_DISCOVERY_ENVIRONMENT_VARIABLE: &str =
    "STREAMLIB_MESH_MULTICAST_DISCOVERY";

/// The interface multicast scouting runs on, `auto` unless this names one.
///
/// Engine-internal: it exists so the mesh's own two-process fixture can pin
/// scouting to `127.0.0.1` rather than letting a test join whatever network
/// the machine is on. It is deliberately on no CLI flag and no Python keyword.
pub(crate) const MESH_MULTICAST_INTERFACE_ENVIRONMENT_VARIABLE: &str =
    "STREAMLIB_MESH_MULTICAST_INTERFACE";

/// What the five values mean once every door has been read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedRuntimeMeshConfiguration {
    /// The mesh everything this runtime announces lives under.
    pub mesh_name: RuntimeMeshName,
    peer_endpoints: Vec<RuntimeMeshEndpoint>,
    listen_endpoints: Vec<RuntimeMeshEndpoint>,
    multicast_discovery: bool,
    multicast_interface: Option<String>,
}

impl ResolvedRuntimeMeshConfiguration {
    /// Read the constructor's values, then the environment's, then the
    /// engine's own defaults.
    pub fn resolve(configuration: RuntimeMeshConfiguration) -> Result<Self> {
        // A stated list replaces the default whether or not it names anything:
        // `mesh_listen_endpoints=[]` asks for a runtime that dials peers and is
        // dialled by none, which is a different request from asking for the
        // default. Only an absent list takes the default — an absent
        // environment variable included, since an empty one reads as unset.
        let a_listener_list_was_stated = configuration.mesh_listen_endpoints.is_some();
        let listen_endpoints = resolve_mesh_endpoints(
            configuration.mesh_listen_endpoints,
            std::env::var_os(MESH_LISTEN_ENDPOINTS_ENVIRONMENT_VARIABLE),
            MESH_LISTEN_ENDPOINTS_ENVIRONMENT_VARIABLE,
        )?;
        let listen_endpoints = if listen_endpoints.is_empty() && !a_listener_list_was_stated {
            vec![read_one_mesh_endpoint(
                DEFAULT_MESH_LISTEN_ENDPOINT,
                "the engine's own default listener",
            )?]
        } else {
            listen_endpoints
        };

        Ok(Self {
            mesh_name: RuntimeMeshName::from_configuration_environment_or_default(
                configuration.mesh_name,
            )?,
            peer_endpoints: resolve_mesh_endpoints(
                configuration.mesh_peer_endpoints,
                std::env::var_os(MESH_PEER_ENDPOINTS_ENVIRONMENT_VARIABLE),
                MESH_PEER_ENDPOINTS_ENVIRONMENT_VARIABLE,
            )?,
            listen_endpoints,
            multicast_discovery: resolve_multicast_discovery(
                configuration.mesh_multicast_discovery,
                std::env::var_os(MESH_MULTICAST_DISCOVERY_ENVIRONMENT_VARIABLE),
            )?,
            multicast_interface: what_an_environment_door_says(
                std::env::var_os(MESH_MULTICAST_INTERFACE_ENVIRONMENT_VARIABLE),
                MESH_MULTICAST_INTERFACE_ENVIRONMENT_VARIABLE,
            )?,
        })
    }

    /// This configuration as Zenoh takes it — built from Zenoh's defaults and
    /// these values, never from a Zenoh config file or a `ZENOH_*` variable.
    pub fn as_a_zenoh_configuration(&self) -> zenoh::Result<zenoh::Config> {
        let mut configuration = zenoh::Config::default();
        configuration.insert_json5("mode", "\"peer\"")?;
        configuration.insert_json5("listen/endpoints", &as_a_json5_list(&self.listen_endpoints))?;
        configuration.insert_json5("connect/endpoints", &as_a_json5_list(&self.peer_endpoints))?;
        configuration.insert_json5(
            "scouting/multicast/enabled",
            if self.multicast_discovery {
                "true"
            } else {
                "false"
            },
        )?;
        if let Some(interface) = &self.multicast_interface {
            configuration.insert_json5(
                "scouting/multicast/interface",
                &as_a_json5_string(interface),
            )?;
        }
        Ok(configuration)
    }
}

/// Whether multicast discovery runs: what the constructor said, else what the
/// environment said, else on — except under the engine's own test build, where
/// a runtime a test constructs must never join the machine's real mesh.
fn resolve_multicast_discovery(
    configured: Option<bool>,
    from_the_environment: Option<OsString>,
) -> Result<bool> {
    if let Some(configured) = configured {
        return Ok(configured);
    }
    if let Some(stated) = what_an_environment_door_says(
        from_the_environment,
        MESH_MULTICAST_DISCOVERY_ENVIRONMENT_VARIABLE,
    )? {
        return match stated.as_str() {
            "1" => Ok(true),
            "0" => Ok(false),
            _ => Err(Error::Configuration(format!(
                "{MESH_MULTICAST_DISCOVERY_ENVIRONMENT_VARIABLE} is {stated:?}, which is neither \
                 \"1\" nor \"0\""
            ))),
        };
    }
    Ok(!cfg!(test))
}

fn as_a_json5_list(endpoints: &[RuntimeMeshEndpoint]) -> String {
    let quoted: Vec<String> = endpoints
        .iter()
        .map(|endpoint| as_a_json5_string(endpoint.as_str()))
        .collect();
    format!("[{}]", quoted.join(","))
}

/// One quoting rule for every value this builds into JSON5, so an interface
/// name carrying a quote cannot make a document an endpoint list would have
/// escaped.
fn as_a_json5_string(value: &str) -> String {
    format!("{value:?}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolved(
        configuration: RuntimeMeshConfiguration,
    ) -> Result<ResolvedRuntimeMeshConfiguration> {
        ResolvedRuntimeMeshConfiguration::resolve(configuration)
    }

    /// The defaults every runtime takes when it is told nothing: the `default`
    /// mesh, one QUIC-over-UDP listener, and no dialled peer.
    #[test]
    fn a_runtime_told_nothing_listens_on_quic_over_udp_and_joins_the_default_mesh() {
        let resolved = resolved(RuntimeMeshConfiguration::default()).expect("the defaults resolve");
        assert_eq!(resolved.mesh_name.as_str(), "default");
        assert_eq!(
            resolved
                .listen_endpoints
                .iter()
                .map(RuntimeMeshEndpoint::as_str)
                .collect::<Vec<_>>(),
            [DEFAULT_MESH_LISTEN_ENDPOINT]
        );
        assert!(resolved.peer_endpoints.is_empty());
    }

    /// Stated listen endpoints replace the default rather than joining it —
    /// two peers keep one link, so a second listener is a second way to be
    /// reached, never an addition the engine makes behind the caller.
    #[test]
    fn stated_listen_endpoints_replace_the_default_listener() {
        let resolved = resolved(RuntimeMeshConfiguration {
            mesh_listen_endpoints: Some(vec!["tcp/127.0.0.1:7447".to_string()]),
            ..Default::default()
        })
        .expect("a legal endpoint resolves");
        assert_eq!(
            resolved
                .listen_endpoints
                .iter()
                .map(RuntimeMeshEndpoint::as_str)
                .collect::<Vec<_>>(),
            ["tcp/127.0.0.1:7447"]
        );
    }

    /// A stated empty listener list is honoured rather than quietly replaced by
    /// the default, which is what `resolve_mesh_endpoints` already promises for
    /// every other stated list.
    #[test]
    fn a_stated_empty_listener_list_leaves_the_runtime_listening_on_nothing() {
        let resolved = resolved(RuntimeMeshConfiguration {
            mesh_listen_endpoints: Some(Vec::new()),
            ..Default::default()
        })
        .expect("an empty listener list is legal");

        assert!(resolved.listen_endpoints.is_empty());
        assert_eq!(
            resolved
                .as_a_zenoh_configuration()
                .expect("zenoh takes every value")
                .get_json("listen/endpoints")
                .expect("listen endpoints"),
            "[]"
        );
    }

    /// A transport this build lacks is refused at construction, from whichever
    /// list names it.
    #[test]
    fn an_endpoint_this_build_cannot_open_is_refused_at_construction() {
        for configuration in [
            RuntimeMeshConfiguration {
                mesh_peer_endpoints: Some(vec!["quic/127.0.0.1:7447".to_string()]),
                ..Default::default()
            },
            RuntimeMeshConfiguration {
                mesh_listen_endpoints: Some(vec!["quic/127.0.0.1:7447".to_string()]),
                ..Default::default()
            },
        ] {
            let refusal = resolved(configuration)
                .err()
                .expect("a quic/ endpoint must be refused")
                .to_string();
            assert!(refusal.contains("quic"), "{refusal}");
        }
    }

    /// The engine's own test build never joins the machine's real mesh, so a
    /// test that constructs a runtime cannot reach the owner's desk.
    #[test]
    fn the_engines_test_build_leaves_multicast_discovery_off_by_default() {
        assert!(
            !resolved(RuntimeMeshConfiguration::default())
                .expect("the defaults resolve")
                .multicast_discovery
        );
    }

    /// The constructor beats the environment, which beats the default.
    #[test]
    fn multicast_discovery_takes_the_constructors_answer_then_the_environments() {
        assert!(
            resolve_multicast_discovery(Some(true), Some(OsString::from("0")))
                .expect("a stated value wins")
        );
        assert!(
            resolve_multicast_discovery(None, Some(OsString::from("1"))).expect("the environment")
        );
        assert!(
            !resolve_multicast_discovery(None, Some(OsString::from("0"))).expect("the environment")
        );
        assert!(
            !resolve_multicast_discovery(None, Some(OsString::new()))
                .expect("an empty variable is no variable")
        );
    }

    /// A discovery value that is neither `1` nor `0` is refused by name rather
    /// than read as one of them.
    #[test]
    fn a_discovery_value_that_is_neither_one_nor_zero_is_refused_by_name() {
        let refusal = resolve_multicast_discovery(None, Some(OsString::from("yes")))
            .err()
            .expect("\"yes\" must be refused")
            .to_string();
        assert!(
            refusal.contains(MESH_MULTICAST_DISCOVERY_ENVIRONMENT_VARIABLE)
                && refusal.contains("yes"),
            "{refusal}"
        );
    }

    /// The Zenoh configuration this builds is the one the session opens with:
    /// peer mode, exactly the endpoints resolved above, and scouting as asked.
    #[test]
    fn the_zenoh_configuration_carries_peer_mode_and_exactly_these_endpoints() {
        let resolved = resolved(RuntimeMeshConfiguration {
            mesh_peer_endpoints: Some(vec!["tcp/127.0.0.1:7447".to_string()]),
            mesh_multicast_discovery: Some(false),
            ..Default::default()
        })
        .expect("a legal configuration resolves");

        let configuration = resolved
            .as_a_zenoh_configuration()
            .expect("zenoh takes every value");
        assert_eq!(configuration.get_json("mode").expect("mode"), "\"peer\"");
        assert_eq!(
            configuration
                .get_json("connect/endpoints")
                .expect("connect endpoints"),
            "[\"tcp/127.0.0.1:7447\"]"
        );
        assert_eq!(
            configuration
                .get_json("listen/endpoints")
                .expect("listen endpoints"),
            format!("[\"{DEFAULT_MESH_LISTEN_ENDPOINT}\"]")
        );
        assert_eq!(
            configuration
                .get_json("scouting/multicast/enabled")
                .expect("scouting"),
            "false"
        );
    }
}
