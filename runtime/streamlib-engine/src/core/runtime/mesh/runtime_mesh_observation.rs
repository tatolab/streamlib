// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! What a process sees on a runtime mesh it never joins.
//!
//! `streamlib nodes` reads the mesh beside the on-disk registry, and a runtime
//! is the wrong thing to build for that: constructing one would take a name, an
//! iceoryx2 node and a surface socket, and would put a second holder of that
//! name on the mesh for as long as the listing took.
//!
//! So this opens a session that **declares nothing** — no liveliness token, no
//! description queryable — asks the mesh who is on it, asks each of them what
//! it is, and closes. Nothing it does is visible to a runtime as a peer, and
//! the duplicate-name check has nothing new to trip over. It listens on nothing
//! either: an observer is dialled by nobody, and taking the runtime's default
//! listener would make `streamlib nodes` fight a runtime in the same shell for
//! a `STREAMLIB_MESH_LISTEN_ENDPOINTS` port.
//!
//! Everything else is the runtime's: the same configuration resolution, the
//! same defaults, the same key space, the same description query. This is a
//! second reader of the mesh, never a second mesh.

use std::collections::BTreeSet;

use zenoh::Wait;

use crate::core::error::{Error, Result};
use crate::core::json_schema::RuntimeMeshPeerOutput;
use crate::core::runtime::RuntimeMeshConfiguration;
use crate::core::runtime::mesh::resolved_runtime_mesh_configuration::ResolvedRuntimeMeshConfiguration;
use crate::core::runtime::mesh::runtime_mesh_description::{
    ask_every_peer_what_it_is, render_a_peer,
};
use crate::core::runtime::mesh::runtime_mesh_key::{AnnouncedRuntimeIdentity, RuntimeMeshKeySpace};
use crate::core::runtime::mesh::zenoh_work_off_any_tokio_runtime::off_any_current_thread_tokio_runtime;

/// One look at one mesh, taken from outside it.
#[derive(Debug, Clone)]
pub struct RuntimeMeshObservation {
    /// The mesh that was looked at, resolved — so a caller that named none can
    /// still say which one it read.
    pub mesh_name: String,
    /// Every runtime announced on it, sorted by name. The same rendering
    /// `graph` gives a peer, including the four keys an unanswered peer leaves
    /// absent.
    pub peers: Vec<RuntimeMeshPeerOutput>,
}

/// Look at the mesh `configuration` names without joining it.
///
/// `runtime_name` and `mesh_listen_endpoints` are not read: an observer is
/// addressed by nobody. The mesh name, the dialled peers and multicast
/// discovery resolve exactly as a runtime's do, environment included.
///
/// Errs when the caller's configuration is refused, and when the session will
/// not open at all — an observer that cannot look has nothing to report, where
/// a runtime that cannot join still runs.
pub fn observe_a_runtime_mesh(
    configuration: RuntimeMeshConfiguration,
) -> Result<RuntimeMeshObservation> {
    let resolved = ResolvedRuntimeMeshConfiguration::resolve(RuntimeMeshConfiguration {
        runtime_name: None,
        mesh_listen_endpoints: Some(Vec::new()),
        ..configuration
    })?;
    let key_space = RuntimeMeshKeySpace::of(resolved.mesh_name.clone());
    let mesh_name = resolved.mesh_name.to_string();

    let peers = off_any_current_thread_tokio_runtime("observe", || {
        read_every_runtime_announced_on(&resolved, &key_space)
    })
    .map_err(|cannot_spawn| {
        Error::Runtime(format!(
            "the {mesh_name} mesh could not be read for want of a thread: {cannot_spawn}"
        ))
    })??;

    Ok(RuntimeMeshObservation { mesh_name, peers })
}

/// Open, ask, close. Runs on a thread that is nobody's tokio runtime.
fn read_every_runtime_announced_on(
    resolved: &ResolvedRuntimeMeshConfiguration,
    key_space: &RuntimeMeshKeySpace,
) -> Result<Vec<RuntimeMeshPeerOutput>> {
    let configuration = resolved
        .as_a_zenoh_configuration_for_one_question()
        .map_err(|configuration_failure| {
            Error::Runtime(format!(
                "the {} mesh could not be configured for reading: {configuration_failure}",
                resolved.mesh_name
            ))
        })?;
    let session = zenoh::open(configuration).wait().map_err(|open_failure| {
        Error::Runtime(format!(
            "the {} mesh could not be reached: {open_failure}",
            resolved.mesh_name
        ))
    })?;

    let peers = ask_every_peer_what_it_is(
        &session,
        key_space,
        every_runtime_announced_on(&session, key_space),
    )
    .iter()
    .map(|(announced, described)| render_a_peer(&announced.runtime_name, described.as_ref()))
    .collect();

    if let Err(close_failure) = session.close().wait() {
        tracing::debug!("the session that read the mesh did not close cleanly: {close_failure}");
    }
    Ok(peers)
}

/// Every runtime whose liveliness token is live on this mesh, in the order a
/// peer table renders them.
///
/// A set rather than a list: two runtimes may hold one name, so ordering by
/// name alone would leave two rows whose order changed between runs. A token
/// this engine did not write is read past rather than guessed at, the way the
/// discovery subscriber reads past one.
fn every_runtime_announced_on(
    session: &zenoh::Session,
    key_space: &RuntimeMeshKeySpace,
) -> BTreeSet<AnnouncedRuntimeIdentity> {
    // An unreadable mesh is an empty one here rather than an error: the caller
    // has a registry table to print either way, and a mesh with nobody on it
    // and a mesh that would not answer both read as no peers.
    let Ok(replies) = session
        .liveliness()
        .get(key_space.every_announcement_key())
        .wait()
        .inspect_err(|query_failure| {
            tracing::warn!("could not ask the mesh who is on it: {query_failure}");
        })
    else {
        return BTreeSet::new();
    };

    replies
        .into_iter()
        .filter_map(|reply| {
            key_space.read_an_announcement_key(reply.result().ok()?.key_expr().as_str())
        })
        .collect()
}
