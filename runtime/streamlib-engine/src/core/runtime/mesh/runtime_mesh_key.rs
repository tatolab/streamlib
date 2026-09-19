// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Where a runtime's announcement sits in the mesh's key space, and how to
//! read who is announced there.
//!
//! A liveliness token carries no payload and a sample carries no zid, so the
//! token key is the whole of what a runtime that has already died still says
//! about itself: its name, its host and its pid. Everything else is answered
//! by the description queryable beside it, which needs a living process.
//!
//! Reading the tokens lives here rather than beside either of its two callers
//! — the duplicate-name check and the observation `streamlib nodes` makes —
//! because knowing which keys are announcements and knowing how to turn one
//! back into an identity is one piece of knowledge, and a second copy of it
//! would be a second place to change when the key layout moves.

use std::time::Duration;

use zenoh::Wait;

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

/// The chunk under a runtime's own name where it answers which output ports it
/// offers. Verbatim like every `@` chunk, so `@runtime/**` — the announcement
/// subscription — never reaches it.
const OFFERED_OUTPUT_PORTS_CHUNK: &str = "@offered-ports";

/// The chunk under a runtime's own name where the runtimes reading its ports
/// hold their tokens: `@readers/<display name>/<port>/<reader's runtime name>`.
const READERS_CHUNK: &str = "@readers";

/// The chunk under a runtime's own name where it holds one token per port it is
/// currently sending: `@egress/<display name>/<port>`.
const EGRESS_CHUNK: &str = "@egress";

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

/// One runtime reading one output port of another, as its token names it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ReaderOfAnOutputPort {
    /// The runtime that owns the port being read.
    pub source_runtime_name: String,
    /// The display name of the processor that owns it.
    pub processor_display_name: String,
    /// The port's own name.
    pub port_name: String,
    /// The runtime doing the reading.
    pub reading_runtime_name: String,
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

    /// The one key a runtime announces itself at — its liveliness token and the
    /// queryable that describes it both live here.
    ///
    /// One key rather than two subtrees: liveliness and queries are separate
    /// declaration kinds, so a subscriber over the tokens is never delivered a
    /// query and a `get` never reaches a token. Splitting them would buy
    /// nothing and give #2284's duplicate check two places to look.
    pub fn announcement_key_for(&self, announced: &AnnouncedRuntimeIdentity) -> String {
        format!(
            "{}/{}/{}/{}",
            self.runtime_announcement_root(),
            announced.runtime_name,
            announced.host_identity.as_one_key_chunk(),
            announced.process_id
        )
    }

    /// The key a runtime answers on when a peer asks which output ports it
    /// offers.
    ///
    /// Under the runtime's name rather than its whole announced identity: a
    /// peer knows the name it is pulling from and nothing else about the
    /// process behind it. Two live runtimes holding one name is refused as an
    /// address collision before a link is ever carried from either.
    pub fn offered_output_ports_key_of(&self, runtime_name: &str) -> String {
        format!(
            "{}/{runtime_name}/{OFFERED_OUTPUT_PORTS_CHUNK}",
            self.runtime_announcement_root()
        )
    }

    /// The token a runtime declares to say it is reading one port of
    /// `source_runtime_name`.
    ///
    /// The source runtime watches these: the first reader of a port creates its
    /// egress, and the last reader leaving removes it, so a runtime does no
    /// network work for a port nobody pulls.
    pub fn reader_token_key(
        &self,
        source_runtime_name: &str,
        processor_display_name: &str,
        port_name: &str,
        reading_runtime_name: &str,
    ) -> String {
        format!(
            "{}/{source_runtime_name}/{READERS_CHUNK}/{processor_display_name}/{port_name}/\
             {reading_runtime_name}",
            self.runtime_announcement_root()
        )
    }

    /// The key a source runtime subscribes to in order to see every reader of
    /// every one of its ports.
    pub fn every_reader_token_of(&self, source_runtime_name: &str) -> String {
        format!(
            "{}/{source_runtime_name}/{READERS_CHUNK}/**",
            self.runtime_announcement_root()
        )
    }

    /// Which port a reader token names, or `None` when the key is not one this
    /// engine wrote.
    pub fn read_a_reader_token_key(&self, key: &str) -> Option<ReaderOfAnOutputPort> {
        let readers_root = format!("{}/", self.runtime_announcement_root());
        let rest = key.strip_prefix(&readers_root)?;
        let mut chunks = rest.split('/');
        let source_runtime_name = chunks.next()?.to_string();
        if chunks.next()? != READERS_CHUNK {
            return None;
        }
        let processor_display_name = chunks.next()?.to_string();
        let port_name = chunks.next()?.to_string();
        let reading_runtime_name = chunks.next()?.to_string();
        if chunks.next().is_some() {
            return None;
        }
        Some(ReaderOfAnOutputPort {
            source_runtime_name,
            processor_display_name,
            port_name,
            reading_runtime_name,
        })
    }

    /// The token a source runtime declares while it is sending one port.
    ///
    /// A reader watches this: the token going while the runtime stays says the
    /// port stopped being sent, which returns the link to waiting.
    pub fn egress_token_key(
        &self,
        source_runtime_name: &str,
        processor_display_name: &str,
        port_name: &str,
    ) -> String {
        format!(
            "{}/{source_runtime_name}/{EGRESS_CHUNK}/{processor_display_name}/{port_name}",
            self.runtime_announcement_root()
        )
    }

    /// The key one port's bags ride on.
    ///
    /// Outside the `@runtime` subtree, because this is the port's own address:
    /// `streamlib/<mesh name>/<runtime name>/<display name>/<port>`.
    pub fn data_key(
        &self,
        source_runtime_name: &str,
        processor_display_name: &str,
        port_name: &str,
    ) -> String {
        format!(
            "{MESH_KEY_ROOT_CHUNK}/{}/{source_runtime_name}/{processor_display_name}/{port_name}",
            self.mesh_name
        )
    }

    /// The key a liveliness subscriber names to see every runtime on this
    /// mesh. `@runtime` is spelled out because a leading-`@` chunk is verbatim
    /// and no wildcard would reach it.
    pub fn every_announcement_key(&self) -> String {
        format!("{}/**", self.runtime_announcement_root())
    }

    /// The key a duplicate-name check names to see every runtime holding
    /// `runtime_name` — whatever host it is on and whatever its pid.
    ///
    /// A runtime name is one legal key chunk, refused at construction
    /// otherwise, so it carries no wildcard of its own.
    pub fn every_announcement_key_under(&self, runtime_name: &str) -> String {
        format!("{}/{runtime_name}/**", self.runtime_announcement_root())
    }

    /// Every runtime whose liveliness token is live on this mesh.
    pub(super) fn every_runtime_announced_on_this_mesh(
        &self,
        session: &zenoh::Session,
        how_long_peers_have_to_answer: Duration,
    ) -> zenoh::Result<Vec<AnnouncedRuntimeIdentity>> {
        self.read_every_announcement_matching(
            session,
            self.every_announcement_key(),
            how_long_peers_have_to_answer,
        )
    }

    /// Every runtime whose liveliness token is live under `runtime_name` —
    /// whatever host it is on and whatever its pid.
    pub(super) fn every_runtime_announced_under_the_name(
        &self,
        session: &zenoh::Session,
        runtime_name: &str,
        how_long_peers_have_to_answer: Duration,
    ) -> zenoh::Result<Vec<AnnouncedRuntimeIdentity>> {
        self.read_every_announcement_matching(
            session,
            self.every_announcement_key_under(runtime_name),
            how_long_peers_have_to_answer,
        )
    }

    /// Ask the mesh who is announced under `announcement_key`.
    ///
    /// The bound is the caller's to state rather than Zenoh's to default: a
    /// liveliness `get` whose peer never sends its final reply — a partition,
    /// a process killed behind a half-open link — otherwise waits out
    /// `queries_default_timeout`, ten seconds nobody here would have chosen.
    ///
    /// A token this engine did not write is read past rather than guessed at,
    /// the way the discovery subscriber reads past one.
    fn read_every_announcement_matching(
        &self,
        session: &zenoh::Session,
        announcement_key: String,
        how_long_peers_have_to_answer: Duration,
    ) -> zenoh::Result<Vec<AnnouncedRuntimeIdentity>> {
        let replies = session
            .liveliness()
            .get(announcement_key)
            .timeout(how_long_peers_have_to_answer)
            .wait()?;
        Ok(replies
            .into_iter()
            .filter_map(|reply| {
                self.read_an_announcement_key(reply.result().ok()?.key_expr().as_str())
            })
            .collect())
    }

    /// Who an announcement key names, or `None` when the key is not one this
    /// engine wrote.
    pub fn read_an_announcement_key(&self, key: &str) -> Option<AnnouncedRuntimeIdentity> {
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

    /// An announcement key carries the whole identity, so a peer reads name,
    /// host and pid off a runtime that is already gone.
    #[test]
    fn an_announcement_key_reads_back_as_the_identity_that_wrote_it() {
        let key_space = a_key_space("lab");
        for identity in [
            an_identity("rig-desk-a1b2", 4321),
            AnnouncedRuntimeIdentity {
                host_identity: HostIdentity::Unidentified,
                ..an_identity("カメラ 2", 1)
            },
        ] {
            let key = key_space.announcement_key_for(&identity);
            assert_eq!(
                key_space.read_an_announcement_key(&key),
                Some(identity.clone()),
                "{key} must read back as the identity that wrote it"
            );
        }
    }

    /// Every key this engine writes is a key expression Zenoh accepts, and the
    /// subscription reaches the announcements.
    #[test]
    fn every_key_is_a_zenoh_key_expression_and_the_subscription_reaches_the_announcements() {
        let key_space = a_key_space("lab");
        let announcement_key = key_space.announcement_key_for(&an_identity("rig-desk-a1b2", 4321));
        let subscription = key_space.every_announcement_key();

        let announcement_key = keyexpr::new(announcement_key.as_str())
            .expect("the announcement key is a key expression");
        let subscription =
            keyexpr::new(subscription.as_str()).expect("the subscription is a key expression");

        assert!(
            subscription.includes(announcement_key),
            "{subscription} must reach {announcement_key}"
        );
    }

    /// The duplicate check's key reaches every holder of one name and nobody
    /// else's, whatever host or pid the holder announces.
    #[test]
    fn the_key_for_one_name_reaches_every_holder_of_it_and_no_other_name() {
        let key_space = a_key_space("lab");
        let under_one_name = keyexpr::new(
            key_space
                .every_announcement_key_under("rig-desk-a1b2")
                .as_str(),
        )
        .expect("a key expression")
        .to_owned();

        for holder in [
            an_identity("rig-desk-a1b2", 4321),
            an_identity("rig-desk-a1b2", 9999),
            AnnouncedRuntimeIdentity {
                host_identity: HostIdentity::Unidentified,
                ..an_identity("rig-desk-a1b2", 1)
            },
        ] {
            let held = key_space.announcement_key_for(&holder);
            assert!(
                under_one_name.includes(keyexpr::new(held.as_str()).expect("a key expression")),
                "{under_one_name} must reach {held}"
            );
        }

        let another_name = key_space.announcement_key_for(&an_identity("rig-desk-c3d4", 4321));
        assert!(
            !under_one_name
                .includes(keyexpr::new(another_name.as_str()).expect("a key expression")),
            "{under_one_name} must not reach {another_name}"
        );
    }

    /// The verbatim `@runtime` chunk is what keeps port addresses out of the
    /// announcement subtree: a `**` subscription over the mesh never matches
    /// it, which is why the subscriber spells the chunk out.
    #[test]
    fn a_wildcard_over_the_mesh_never_reaches_an_announcement() {
        let key_space = a_key_space("lab");
        let announcement_key = key_space.announcement_key_for(&an_identity("rig-desk-a1b2", 4321));
        let everything_under_the_mesh = keyexpr::new("streamlib/lab/**").expect("a key expression");

        assert!(
            !everything_under_the_mesh
                .includes(keyexpr::new(announcement_key.as_str()).expect("a key expression")),
            "a wildcard must not reach {announcement_key}"
        );
    }

    /// Two meshes never read each other's tokens, which is the whole of the
    /// isolation the mesh name buys.
    #[test]
    fn one_meshs_key_space_reads_nothing_of_anothers() {
        let identity = an_identity("rig-desk-a1b2", 4321);
        let key_in_the_other_mesh = a_key_space("other").announcement_key_for(&identity);

        assert_eq!(
            a_key_space("lab").read_an_announcement_key(&key_in_the_other_mesh),
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
                key_space.read_an_announcement_key(foreign),
                None,
                "{foreign:?} must read as no identity"
            );
        }
    }

    /// A reader token carries the whole of what the source runtime needs to
    /// decide which port to send and to whom.
    #[test]
    fn a_reader_token_key_reads_back_as_the_port_and_the_reader() {
        let key_space = a_key_space("lab");
        let key = key_space.reader_token_key(
            "bench-cam-a1b2",
            "Camera Source 2",
            "video",
            "desk-viewer-c3d4",
        );
        assert_eq!(
            key_space.read_a_reader_token_key(&key),
            Some(ReaderOfAnOutputPort {
                source_runtime_name: "bench-cam-a1b2".to_string(),
                processor_display_name: "Camera Source 2".to_string(),
                port_name: "video".to_string(),
                reading_runtime_name: "desk-viewer-c3d4".to_string(),
            })
        );
    }

    /// A source runtime's reader subscription reaches every reader of every one
    /// of its ports, and nobody else's.
    #[test]
    fn the_reader_subscription_reaches_every_reader_of_this_runtimes_ports_only() {
        let key_space = a_key_space("lab");
        let ours = keyexpr::new(key_space.every_reader_token_of("bench-cam-a1b2").as_str())
            .expect("a key expression")
            .to_owned();

        for (display, port, reader) in [
            ("CameraSource", "video", "desk-one"),
            ("CameraSource", "video", "desk-two"),
            ("MicrophoneSource", "audio", "desk-one"),
        ] {
            let held = key_space.reader_token_key("bench-cam-a1b2", display, port, reader);
            assert!(
                ours.includes(keyexpr::new(held.as_str()).expect("a key expression")),
                "{ours} must reach {held}"
            );
        }

        let anothers =
            key_space.reader_token_key("bench-cam-c3d4", "CameraSource", "video", "desk-one");
        assert!(
            !ours.includes(keyexpr::new(anothers.as_str()).expect("a key expression")),
            "{ours} must not reach {anothers}"
        );
    }

    /// The announcement subscription never reaches the reader, egress or
    /// offered-port keys, and the reader reader never reaches an announcement:
    /// each hangs under a chunk beginning `@`, which no wildcard matches.
    #[test]
    fn the_at_chunks_keep_every_subtree_out_of_every_others_subscription() {
        let key_space = a_key_space("lab");
        let announcements = keyexpr::new(key_space.every_announcement_key().as_str())
            .expect("a key expression")
            .to_owned();

        for out_of_reach in [
            key_space.reader_token_key("bench", "CameraSource", "video", "desk"),
            key_space.egress_token_key("bench", "CameraSource", "video"),
            key_space.offered_output_ports_key_of("bench"),
        ] {
            assert!(
                !announcements
                    .includes(keyexpr::new(out_of_reach.as_str()).expect("a key expression")),
                "{announcements} must not reach {out_of_reach}"
            );
            assert_eq!(
                key_space.read_an_announcement_key(&out_of_reach),
                None,
                "{out_of_reach} must not read as an announcement"
            );
        }

        assert_eq!(
            key_space
                .read_a_reader_token_key(&key_space.announcement_key_for(&an_identity("bench", 7))),
            None,
            "an announcement must not read as a reader token"
        );
    }

    /// A port's own bags ride outside the `@runtime` subtree, at the address
    /// the port is named by — so the data key is the address and nothing else.
    #[test]
    fn a_ports_bags_ride_at_the_address_the_port_is_named_by() {
        let key_space = a_key_space("lab");
        let data_key = key_space.data_key("bench-cam-a1b2", "Camera Source 2", "video");
        assert_eq!(
            data_key,
            "streamlib/lab/bench-cam-a1b2/Camera Source 2/video"
        );
        keyexpr::new(data_key.as_str()).expect("the data key is a key expression");

        assert_eq!(
            a_key_space("other").data_key("bench-cam-a1b2", "Camera Source 2", "video"),
            "streamlib/other/bench-cam-a1b2/Camera Source 2/video",
            "two meshes never share a port's data key"
        );
    }

    /// A key this engine did not write reads as no reader rather than as one
    /// with half its parts invented.
    #[test]
    fn a_key_this_engine_did_not_write_reads_as_no_reader() {
        let key_space = a_key_space("lab");
        for foreign in [
            "streamlib/lab/@runtime/bench/@readers",
            "streamlib/lab/@runtime/bench/@readers/CameraSource",
            "streamlib/lab/@runtime/bench/@readers/CameraSource/video",
            "streamlib/lab/@runtime/bench/@readers/CameraSource/video/desk/extra",
            "streamlib/lab/@runtime/bench/@egress/CameraSource/video",
            "ros2/lab/@runtime/bench/@readers/CameraSource/video/desk",
        ] {
            assert_eq!(
                key_space.read_a_reader_token_key(foreign),
                None,
                "{foreign:?} must read as no reader"
            );
        }
    }
}
