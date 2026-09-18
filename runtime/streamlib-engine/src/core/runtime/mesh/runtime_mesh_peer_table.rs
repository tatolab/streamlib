// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The other runtimes this one currently sees on its mesh.
//!
//! Written by the discovery worker as liveliness tokens come and go, read by
//! `graph`. No network call ever happens while the table is held, so a `graph`
//! never waits on a peer.

use std::collections::BTreeMap;

use parking_lot::RwLock;

use crate::core::json_schema::RuntimeMeshPeerOutput;
use crate::core::runtime::mesh::runtime_mesh_description::RuntimeMeshDescription;
use crate::core::runtime::mesh::runtime_mesh_key::AnnouncedRuntimeIdentity;

/// Every peer this runtime currently sees.
///
/// Keyed by the whole announced identity rather than by name: two runtimes
/// that started inside one discovery window may hold one name, and neither
/// should overwrite the other.
#[derive(Debug, Default)]
pub struct RuntimeMeshPeerTable {
    peers: RwLock<BTreeMap<AnnouncedRuntimeIdentity, Option<RuntimeMeshDescription>>>,
}

impl RuntimeMeshPeerTable {
    /// Record a peer whose token arrived. Its description is unknown until it
    /// answers, which is why a peer renders with its name alone until then.
    pub fn record_that_a_peer_appeared(&self, announced: AnnouncedRuntimeIdentity) {
        self.peers.write().entry(announced).or_default();
    }

    /// Record what a peer answered about itself. A peer whose token has
    /// already left is not resurrected by a late answer.
    pub fn record_what_a_peer_answered(
        &self,
        announced: &AnnouncedRuntimeIdentity,
        description: RuntimeMeshDescription,
    ) {
        if let Some(entry) = self.peers.write().get_mut(announced) {
            *entry = Some(description);
        }
    }

    /// Record a peer whose token left.
    pub fn record_that_a_peer_left(&self, announced: &AnnouncedRuntimeIdentity) {
        self.peers.write().remove(announced);
    }

    /// Every peer this runtime currently sees, for the discovery thread to ask
    /// again what they are.
    pub fn every_peer_it_sees(&self) -> Vec<AnnouncedRuntimeIdentity> {
        self.peers.read().keys().cloned().collect()
    }

    /// Every peer, sorted by name — the order `graph` renders them in.
    pub fn render_for_graph(&self) -> Vec<RuntimeMeshPeerOutput> {
        self.peers
            .read()
            .iter()
            .map(|(announced, described)| RuntimeMeshPeerOutput {
                runtime_name: announced.runtime_name.clone(),
                runtime_id: described.as_ref().map(|it| it.runtime_id.clone()),
                host_name: described.as_ref().map(|it| it.host_name.clone()),
                engine_version: described.as_ref().map(|it| it.engine_version.clone()),
                control_plane_urls: described.as_ref().map(|it| it.control_plane_urls.clone()),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::runtime::mesh::HostIdentity;

    fn an_identity(runtime_name: &str, process_id: u32) -> AnnouncedRuntimeIdentity {
        AnnouncedRuntimeIdentity {
            runtime_name: runtime_name.to_string(),
            host_identity: HostIdentity::ThisKernelBootAndPidNamespace {
                kernel_boot_id: "2f1c8a30".to_string(),
                pid_namespace_inode: 4_026_531_836,
            },
            process_id,
        }
    }

    fn a_description(runtime_id: &str) -> RuntimeMeshDescription {
        RuntimeMeshDescription {
            runtime_id: runtime_id.to_string(),
            host_name: "rig".to_string(),
            pid: 4321,
            engine_version: "0.25.0".to_string(),
            control_plane_urls: vec!["http://198.51.100.7:9000".to_string()],
        }
    }

    /// A peer that has not answered yet renders its name alone, and the other
    /// four keys are absent rather than null.
    #[test]
    fn a_peer_that_has_not_answered_renders_its_name_alone() {
        let table = RuntimeMeshPeerTable::default();
        table.record_that_a_peer_appeared(an_identity("lab-two", 7));

        let rendered = serde_json::to_value(table.render_for_graph()).expect("peers serialize");
        assert_eq!(rendered, serde_json::json!([{ "runtime_name": "lab-two" }]));
    }

    /// Once a peer answers, everything it said renders beside its name.
    #[test]
    fn an_answered_peer_renders_everything_it_said() {
        let table = RuntimeMeshPeerTable::default();
        let announced = an_identity("lab-two", 7);
        table.record_that_a_peer_appeared(announced.clone());
        table.record_what_a_peer_answered(&announced, a_description("R7"));

        let rendered = serde_json::to_value(table.render_for_graph()).expect("peers serialize");
        assert_eq!(
            rendered,
            serde_json::json!([{
                "runtime_name": "lab-two",
                "runtime_id": "R7",
                "host_name": "rig",
                "engine_version": "0.25.0",
                "control_plane_urls": ["http://198.51.100.7:9000"],
            }])
        );
    }

    /// Peers render sorted by name, so two runs of one mesh read the same.
    #[test]
    fn peers_render_sorted_by_name() {
        let table = RuntimeMeshPeerTable::default();
        for name in ["zulu", "alpha", "mike"] {
            table.record_that_a_peer_appeared(an_identity(name, 7));
        }
        assert_eq!(
            table
                .render_for_graph()
                .into_iter()
                .map(|peer| peer.runtime_name)
                .collect::<Vec<_>>(),
            ["alpha", "mike", "zulu"]
        );
    }

    /// A peer that leaves is gone from `graph`, and a leave names one peer
    /// rather than every runtime sharing its name.
    #[test]
    fn a_peer_that_leaves_is_gone_and_takes_no_namesake_with_it() {
        let table = RuntimeMeshPeerTable::default();
        let one = an_identity("lab-two", 7);
        let its_namesake = an_identity("lab-two", 8);
        table.record_that_a_peer_appeared(one.clone());
        table.record_that_a_peer_appeared(its_namesake);

        table.record_that_a_peer_left(&one);

        assert_eq!(table.render_for_graph().len(), 1);
    }

    /// Every peer is askable again, so a control plane hosted after the peer
    /// was first seen reaches the next answer.
    #[test]
    fn every_peer_can_be_asked_again_what_it_is() {
        let table = RuntimeMeshPeerTable::default();
        let answered = an_identity("answered", 7);
        let unanswered = an_identity("unanswered", 8);
        table.record_that_a_peer_appeared(answered.clone());
        table.record_that_a_peer_appeared(unanswered.clone());
        table.record_what_a_peer_answered(&answered, a_description("R7"));

        let mut askable = table.every_peer_it_sees();
        askable.sort();
        assert_eq!(askable, [answered, unanswered]);
    }

    /// A description arriving after its peer left does not bring the peer
    /// back, which is what a query racing a leave would otherwise do.
    #[test]
    fn a_late_answer_does_not_resurrect_a_peer_that_left() {
        let table = RuntimeMeshPeerTable::default();
        let announced = an_identity("lab-two", 7);
        table.record_that_a_peer_appeared(announced.clone());
        table.record_that_a_peer_left(&announced);

        table.record_what_a_peer_answered(&announced, a_description("R7"));

        assert!(table.render_for_graph().is_empty());
    }
}
