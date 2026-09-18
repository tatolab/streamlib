// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The endpoints a runtime listens on and dials on the mesh.
//!
//! A malformed one is a wiring error the caller made, so it is refused at
//! construction the way a wrong `device_id` is, rather than degrading a
//! runtime to local-only at open.

use std::ffi::OsString;
use std::str::FromStr;

use zenoh::config::EndPoint;

use crate::core::error::{Error, Result};

/// The environment variable naming the endpoints this runtime dials, comma
/// separated.
pub(crate) const MESH_PEER_ENDPOINTS_ENVIRONMENT_VARIABLE: &str = "STREAMLIB_MESH_PEER_ENDPOINTS";

/// The environment variable naming the endpoints this runtime listens on,
/// comma separated.
pub(crate) const MESH_LISTEN_ENDPOINTS_ENVIRONMENT_VARIABLE: &str =
    "STREAMLIB_MESH_LISTEN_ENDPOINTS";

/// What separates endpoints inside one environment variable.
const ENDPOINT_LIST_SEPARATOR: char = ',';

/// The one listener a runtime takes unless it is told to listen elsewhere:
/// QUIC over UDP on an ephemeral port, every address this host has.
///
/// One listener, not a TCP one beside it: with `transport_multilink` off two
/// peers keep exactly one link, and offering two transports lets them pick at
/// random.
pub(crate) const DEFAULT_MESH_LISTEN_ENDPOINT: &str = "udp/[::]:0?rel=1";

/// The transports this build carries. TLS `quic/` needs a provisioned key and
/// certificate, so it is not compiled in and an endpoint naming it is refused.
const TRANSPORTS_THIS_BUILD_CARRIES: [&str; 2] = [UDP_TRANSPORT, TCP_TRANSPORT];

const UDP_TRANSPORT: &str = "udp";
const TCP_TRANSPORT: &str = "tcp";

/// The endpoint metadata key Zenoh reads a link's reliability from
/// (`zenoh-link-udp`'s `is_reliable`, over `Metadata::RELIABILITY`).
const RELIABILITY_METADATA_KEY: &str = "rel";

/// The reliability value that selects QUIC over UDP. Zenoh parses this field
/// as the numeric discriminant of its own `Reliability` enum, whose reliable
/// arm is `1`; the enum itself is not re-exported by the `zenoh` facade.
const RELIABLE_METADATA_VALUE: &str = "1";

/// One endpoint this build can actually open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeMeshEndpoint(EndPoint);

impl RuntimeMeshEndpoint {
    /// The endpoint as Zenoh's configuration takes it.
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl std::fmt::Display for RuntimeMeshEndpoint {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// The endpoints a caller stated, or the ones the environment names, or none.
///
/// An empty environment value reads as unset, and an empty list the
/// constructor states is a list of no endpoints rather than an absent one —
/// stating one is asking for it.
pub(crate) fn resolve_mesh_endpoints(
    configured_endpoints: Option<Vec<String>>,
    endpoints_from_the_environment: Option<OsString>,
    environment_variable_name: &str,
) -> Result<Vec<RuntimeMeshEndpoint>> {
    if let Some(configured) = configured_endpoints {
        return configured
            .iter()
            .map(|stated| read_one_mesh_endpoint(stated, "the endpoints it was constructed with"))
            .collect();
    }
    let Some(from_the_environment) =
        endpoints_from_the_environment.filter(|value| !value.is_empty())
    else {
        return Ok(Vec::new());
    };
    let Some(from_the_environment) = from_the_environment.to_str() else {
        return Err(Error::Configuration(format!(
            "{environment_variable_name} is not UTF-8"
        )));
    };
    from_the_environment
        .split(ENDPOINT_LIST_SEPARATOR)
        .map(str::trim)
        .filter(|stated| !stated.is_empty())
        .map(|stated| read_one_mesh_endpoint(stated, environment_variable_name))
        .collect()
}

/// One endpoint, refused by name unless this build can open it.
pub(crate) fn read_one_mesh_endpoint(
    stated: &str,
    where_it_came_from: &str,
) -> Result<RuntimeMeshEndpoint> {
    let endpoint = EndPoint::from_str(stated).map_err(|unreadable| {
        refuse_a_stated_endpoint(
            stated,
            where_it_came_from,
            &format!("Zenoh cannot read it as a locator ({unreadable})"),
        )
    })?;

    let transport = endpoint.protocol().as_str().to_string();
    if !TRANSPORTS_THIS_BUILD_CARRIES.contains(&transport.as_str()) {
        return Err(refuse_a_stated_endpoint(
            stated,
            where_it_came_from,
            &format!("this build carries no {transport:?} transport"),
        ));
    }
    if transport == UDP_TRANSPORT
        && endpoint.metadata().get(RELIABILITY_METADATA_KEY) != Some(RELIABLE_METADATA_VALUE)
    {
        return Err(refuse_a_stated_endpoint(
            stated,
            where_it_came_from,
            &format!(
                "a UDP endpoint carries the mesh over QUIC and must say so with \
                 ?{RELIABILITY_METADATA_KEY}={RELIABLE_METADATA_VALUE}; plain best-effort UDP \
                 would carry declarations, liveliness tokens and queries with no retransmission"
            ),
        ));
    }
    Ok(RuntimeMeshEndpoint(endpoint))
}

fn refuse_a_stated_endpoint(stated: &str, where_it_came_from: &str, what_is_wrong: &str) -> Error {
    Error::Configuration(format!(
        "{where_it_came_from} names the mesh endpoint {stated:?}, which this runtime cannot \
         open: {what_is_wrong}. A mesh endpoint is udp/<host>:<port>?{RELIABILITY_METADATA_KEY}=\
         {RELIABLE_METADATA_VALUE} or tcp/<host>:<port>"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(stated: &str) -> Result<RuntimeMeshEndpoint> {
        read_one_mesh_endpoint(stated, "the test")
    }

    /// The engine's own default listener is an endpoint the engine accepts —
    /// the one case where a mistake would make every runtime local-only.
    #[test]
    fn the_default_listener_passes_the_engines_own_endpoint_check() {
        assert_eq!(
            read(DEFAULT_MESH_LISTEN_ENDPOINT)
                .expect("the default listener must be readable")
                .as_str(),
            DEFAULT_MESH_LISTEN_ENDPOINT
        );
    }

    /// Both transports this build carries are accepted, with or without a
    /// bound address.
    #[test]
    fn both_compiled_transports_are_accepted() {
        for legal in [
            "udp/127.0.0.1:7447?rel=1",
            "udp/[::1]:0?rel=1",
            "tcp/127.0.0.1:7447",
            "tcp/[::]:0",
        ] {
            assert!(read(legal).is_ok(), "{legal:?} must be readable");
        }
    }

    /// A transport this build does not carry is refused naming it, rather than
    /// failing later as an unexplained open failure.
    #[test]
    fn a_transport_this_build_lacks_is_refused_naming_it() {
        for absent in [
            "quic/127.0.0.1:7447",
            "tls/127.0.0.1:7447",
            "ws/127.0.0.1:7447",
        ] {
            let refusal = read(absent)
                .err()
                .unwrap_or_else(|| panic!("{absent:?} must be refused"))
                .to_string();
            let transport = absent.split('/').next().expect("a transport");
            assert!(
                refusal.contains(transport) && refusal.contains(absent),
                "the refusal of {absent:?} must name the transport: {refusal}"
            );
        }
    }

    /// A plain best-effort UDP endpoint is refused naming the reliability
    /// metadata that would make it QUIC.
    #[test]
    fn a_plain_best_effort_udp_endpoint_is_refused_naming_the_reliability_it_lacks() {
        for best_effort in ["udp/127.0.0.1:7447", "udp/127.0.0.1:7447?rel=0"] {
            let refusal = read(best_effort)
                .err()
                .unwrap_or_else(|| panic!("{best_effort:?} must be refused"))
                .to_string();
            assert!(
                refusal.contains("rel=1"),
                "the refusal of {best_effort:?} must name ?rel=1: {refusal}"
            );
        }
    }

    /// A string Zenoh cannot read at all is refused by name too, rather than
    /// reaching the session as something plausible.
    #[test]
    fn a_locator_zenoh_cannot_read_is_refused_by_name() {
        for malformed in ["", "127.0.0.1:7447", "not an endpoint"] {
            assert!(read(malformed).is_err(), "{malformed:?} must be refused");
        }
    }

    /// The environment's list is comma separated, tolerates spacing, and an
    /// empty variable reads as no endpoints at all.
    #[test]
    fn the_environment_takes_a_comma_separated_list_and_an_empty_value_is_no_list() {
        let resolved = resolve_mesh_endpoints(
            None,
            Some(OsString::from(
                "tcp/127.0.0.1:7447, udp/127.0.0.1:7448?rel=1",
            )),
            MESH_PEER_ENDPOINTS_ENVIRONMENT_VARIABLE,
        )
        .expect("both endpoints are readable");
        assert_eq!(
            resolved
                .iter()
                .map(RuntimeMeshEndpoint::as_str)
                .collect::<Vec<_>>(),
            ["tcp/127.0.0.1:7447", "udp/127.0.0.1:7448?rel=1"]
        );

        assert!(
            resolve_mesh_endpoints(
                None,
                Some(OsString::new()),
                MESH_PEER_ENDPOINTS_ENVIRONMENT_VARIABLE
            )
            .expect("an empty variable is no variable")
            .is_empty()
        );
    }

    /// A stated list beats the environment's, and a stated empty list is a
    /// list of no endpoints rather than a fall-through.
    #[test]
    fn a_stated_list_beats_the_environment_and_a_stated_empty_list_is_still_a_list() {
        let resolved = resolve_mesh_endpoints(
            Some(vec!["tcp/127.0.0.1:7447".to_string()]),
            Some(OsString::from("tcp/127.0.0.1:9999")),
            MESH_PEER_ENDPOINTS_ENVIRONMENT_VARIABLE,
        )
        .expect("the stated endpoint is readable");
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].as_str(), "tcp/127.0.0.1:7447");

        assert!(
            resolve_mesh_endpoints(
                Some(Vec::new()),
                Some(OsString::from("tcp/127.0.0.1:9999")),
                MESH_PEER_ENDPOINTS_ENVIRONMENT_VARIABLE
            )
            .expect("an empty stated list is legal")
            .is_empty()
        );
    }
}
