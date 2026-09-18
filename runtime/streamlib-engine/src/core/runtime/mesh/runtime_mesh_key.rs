// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Where a runtime's announcement sits in the mesh's key space.
//!
//! A liveliness token carries no payload and a sample carries no zid, so the
//! token key is the whole of what a runtime that has already died still says
//! about itself: its name, its host and its pid. Everything else is answered
//! by the description queryable beside it, which needs a living process.

use crate::core::runtime::RuntimeName;
use crate::core::runtime::mesh::HostIdentity;
use crate::core::runtime::mesh::runtime_mesh_name::RuntimeMeshName;

/// The chunk every streamlib key begins with, so unrelated Zenoh traffic
/// sharing a network — ROS 2's `rmw_zenoh`, say — never collides with ours.
const MESH_KEY_ROOT_CHUNK: &str = "streamlib";

/// The chunk a runtime's liveliness tokens hang under. Verbatim, because it
/// begins with `@`: no `**` subscription over a mesh's port addresses reaches
/// it, and a display name may not begin with `@`, so no address collides.
const RUNTIME_ANNOUNCEMENT_CHUNK: &str = "@runtime";

/// The same, for the description queryable — its own subtree, so a liveliness
/// subscription over the announcement subtree sees tokens and nothing else.
const RUNTIME_DESCRIPTION_CHUNK: &str = "@runtime-description";

/// What a runtime's own announcement is named by. Every field is on the token
/// key, so a peer reads all three off a token whose runtime is already gone.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct AnnouncedRuntimeIdentity {
    /// The name the runtime is addressed by on the mesh.
    pub runtime_name: String,
    /// The host the runtime is running on, as far as a peer can tell.
    pub host_identity: HostIdentity,
    /// The runtime's process id on that host.
    pub process_id: u32,
}

impl AnnouncedRuntimeIdentity {
    /// What this runtime announces about itself.
    pub fn of_this_runtime(runtime_name: &RuntimeName) -> Self {
        Self {
            runtime_name: runtime_name.as_str().to_string(),
            host_identity: HostIdentity::of_this_host(),
            process_id: std::process::id(),
        }
    }
}

/// One mesh's key space.
#[derive(Debug, Clone)]
pub struct RuntimeMeshKeySpace {
    mesh_name: RuntimeMeshName,
}

impl RuntimeMeshKeySpace {
    /// The key space everything under `mesh_name` lives in.
    pub fn of(mesh_name: RuntimeMeshName) -> Self {
        Self { mesh_name }
    }

    /// The liveliness token key for `announced`.
    pub fn liveliness_token_key_for(&self, announced: &AnnouncedRuntimeIdentity) -> String {
        format!(
            "{}/{}/{}/{}",
            self.runtime_announcement_root(),
            announced.runtime_name,
            announced.host_identity.as_one_key_chunk(),
            announced.process_id
        )
    }

    /// The key a liveliness subscriber names to see every runtime on this
    /// mesh. `@runtime` is spelled out because a leading-`@` chunk is verbatim
    /// and no wildcard would reach it.
    pub fn every_liveliness_token_key(&self) -> String {
        format!("{}/**", self.runtime_announcement_root())
    }

    /// The description queryable key for `announced`.
    pub fn description_key_for(&self, announced: &AnnouncedRuntimeIdentity) -> String {
        format!(
            "{}/{}/{}/{}/{}/{}",
            MESH_KEY_ROOT_CHUNK,
            self.mesh_name,
            RUNTIME_DESCRIPTION_CHUNK,
            announced.runtime_name,
            announced.host_identity.as_one_key_chunk(),
            announced.process_id
        )
    }

    /// Who a liveliness token key names, or `None` when the key is not one
    /// this engine wrote.
    pub fn read_a_liveliness_token_key(&self, key: &str) -> Option<AnnouncedRuntimeIdentity> {
        let announcement_root = self.runtime_announcement_root();
        let rest = key.strip_prefix(&announcement_root)?.strip_prefix('/')?;
        let mut chunks = rest.split('/');
        let runtime_name = chunks.next()?.to_string();
        let host_identity = HostIdentity::from_one_key_chunk(chunks.next()?)?;
        let process_id = chunks.next()?.parse().ok()?;
        if chunks.next().is_some() || runtime_name.is_empty() {
            return None;
        }
        Some(AnnouncedRuntimeIdentity {
            runtime_name,
            host_identity,
            process_id,
        })
    }

    fn runtime_announcement_root(&self) -> String {
        format!(
            "{MESH_KEY_ROOT_CHUNK}/{}/{RUNTIME_ANNOUNCEMENT_CHUNK}",
            self.mesh_name
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zenoh_keyexpr::keyexpr;

    fn a_key_space(mesh_name: &str) -> RuntimeMeshKeySpace {
        RuntimeMeshKeySpace::of(
            RuntimeMeshName::from_configuration_environment_or_default(Some(mesh_name.to_string()))
                .expect("a legal mesh name"),
        )
    }

    fn an_identity(runtime_name: &str, process_id: u32) -> AnnouncedRuntimeIdentity {
        AnnouncedRuntimeIdentity {
            runtime_name: runtime_name.to_string(),
            host_identity: HostIdentity::ThisKernelBootAndPidNamespace {
                kernel_boot_id: "2f1c8a30-6b4e-4d5a-9a11-2c7f0d5e8b93".to_string(),
                pid_namespace_inode: 4_026_531_836,
            },
            process_id,
        }
    }

    /// A token key carries the whole identity, so a peer reads name, host and
    /// pid off a runtime that is already gone.
    #[test]
    fn a_token_key_reads_back_as_the_identity_that_wrote_it() {
        let key_space = a_key_space("lab");
        for identity in [
            an_identity("rig-desk-a1b2", 4321),
            AnnouncedRuntimeIdentity {
                host_identity: HostIdentity::Unidentified,
                ..an_identity("カメラ 2", 1)
            },
        ] {
            let key = key_space.liveliness_token_key_for(&identity);
            assert_eq!(
                key_space.read_a_liveliness_token_key(&key),
                Some(identity.clone()),
                "{key} must read back as the identity that wrote it"
            );
        }
    }

    /// Every key this engine writes is a key expression Zenoh accepts, and the
    /// subscription reaches the tokens.
    #[test]
    fn every_key_is_a_zenoh_key_expression_and_the_subscription_reaches_the_tokens() {
        let key_space = a_key_space("lab");
        let identity = an_identity("rig-desk-a1b2", 4321);

        let token_key = key_space.liveliness_token_key_for(&identity);
        let description_key = key_space.description_key_for(&identity);
        let subscription = key_space.every_liveliness_token_key();

        let token_key =
            keyexpr::new(token_key.as_str()).expect("the token key is a key expression");
        keyexpr::new(description_key.as_str()).expect("the description key is a key expression");
        let subscription =
            keyexpr::new(subscription.as_str()).expect("the subscription is a key expression");

        assert!(
            subscription.includes(token_key),
            "{subscription} must reach {token_key}"
        );
    }

    /// The verbatim `@runtime` chunk is what keeps port addresses out of the
    /// announcement subtree: a `**` subscription over the mesh never matches
    /// it, which is why the subscriber spells the chunk out.
    #[test]
    fn a_wildcard_over_the_mesh_never_reaches_an_announcement() {
        let key_space = a_key_space("lab");
        let token_key = key_space.liveliness_token_key_for(&an_identity("rig-desk-a1b2", 4321));
        let everything_under_the_mesh = keyexpr::new("streamlib/lab/**").expect("a key expression");

        assert!(
            !everything_under_the_mesh
                .includes(keyexpr::new(token_key.as_str()).expect("a key expression")),
            "a wildcard must not reach {token_key}"
        );
    }

    /// Announcements and descriptions are separate subtrees, so the liveliness
    /// subscription sees tokens alone.
    #[test]
    fn the_description_subtree_is_outside_the_announcement_subscription() {
        let key_space = a_key_space("lab");
        let identity = an_identity("rig-desk-a1b2", 4321);
        let subscription = keyexpr::new(key_space.every_liveliness_token_key().as_str())
            .expect("a key expression")
            .to_owned();
        let description_key = key_space.description_key_for(&identity);

        assert!(
            !subscription
                .includes(keyexpr::new(description_key.as_str()).expect("a key expression")),
            "{subscription} must not reach {description_key}"
        );
    }

    /// Two meshes never read each other's tokens, which is the whole of the
    /// isolation the mesh name buys.
    #[test]
    fn one_meshs_key_space_reads_nothing_of_anothers() {
        let identity = an_identity("rig-desk-a1b2", 4321);
        let key_in_the_other_mesh = a_key_space("other").liveliness_token_key_for(&identity);

        assert_eq!(
            a_key_space("lab").read_a_liveliness_token_key(&key_in_the_other_mesh),
            None
        );
    }

    /// A key this engine did not write reads as nothing rather than as a peer
    /// with half its fields invented.
    #[test]
    fn a_key_this_engine_did_not_write_reads_as_no_identity() {
        let key_space = a_key_space("lab");
        for foreign in [
            "streamlib/lab/@runtime",
            "streamlib/lab/@runtime/desk",
            "streamlib/lab/@runtime/desk/kernel.boot.7",
            "streamlib/lab/@runtime/desk/kernel.boot.7/notapid",
            "streamlib/lab/@runtime/desk/kernel.boot.7/1/extra",
            "streamlib/lab/@runtime//kernel.boot.7/1",
            "ros2/lab/@runtime/desk/kernel.boot.7/1",
        ] {
            assert_eq!(
                key_space.read_a_liveliness_token_key(foreign),
                None,
                "{foreign:?} must read as no identity"
            );
        }
    }
}
