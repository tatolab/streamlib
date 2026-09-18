// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Every port on another runtime this runtime's graph links from, and how far
//! each has got.
//!
//! `connect` never waits on the mesh, so this is where the waiting happens: a
//! link lands here the moment it is applied and is resolved afterwards, as the
//! source runtime appears, says which ports it offers, and leaves again. Each
//! link's cell is the one `graph` reads, and it is written here — never under
//! the graph lock, which a compile holds.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};
use parking_lot::Mutex;
use zenoh::Wait;

use crate::core::graph::{LinkUniqueId, MeshPortAddress, RemoteLinkResolution};
use crate::core::runtime::mesh::mesh_link_ingress::MeshLinkIngress;
use crate::core::runtime::mesh::output_ports_offered_on_the_mesh::ask_a_runtime_what_output_ports_it_offers;
use crate::core::runtime::mesh::runtime_mesh_key::RuntimeMeshKeySpace;
use crate::core::runtime::mesh::runtime_mesh_peer_table::RuntimeMeshPeerTable;
use crate::iceoryx2::Iceoryx2Node;
use streamlib_ipc_types::MAX_INBOUND_LINKS_PER_DESTINATION;

/// How often every waiting link is looked at again.
///
/// The pass also runs the moment a runtime's token comes or goes, so this is
/// the floor under a token nobody delivered rather than the cadence resolution
/// normally runs at. Engine-chosen; nothing authorable.
const HOW_OFTEN_EVERY_WAITING_LINK_IS_LOOKED_AT_AGAIN: Duration = Duration::from_secs(2);

/// One link waiting on, or carried from, a port on another runtime.
struct ALinkFromAnotherRuntime {
    address: MeshPortAddress,
    /// The cell `graph` reads. Held here so a resolution lands without the
    /// graph lock.
    how_far_it_has_got: Arc<Mutex<RemoteLinkResolution>>,
    /// The notify service the destination waits on, named by the wiring op —
    /// `None` for a destination that drains no listener. The name rather than
    /// a notifier: an iceoryx2 notifier is `!Send`, so it is minted on the
    /// thread that hands it to the ingress.
    notify_service_name: Option<String>,
    /// Whether the ingress has already been told about this destination, so a
    /// re-pass never adds it twice.
    the_ingress_knows_about_it: bool,
}

/// What the table is waiting on and what it is carrying.
#[derive(Default)]
struct WhatThisRuntimeIsCarryingFromOtherRuntimes {
    links: BTreeMap<LinkUniqueId, ALinkFromAnotherRuntime>,
    carrying: BTreeMap<MeshPortAddress, MeshLinkIngress>,
}

/// Every port on another runtime this runtime links from.
pub struct MeshLinkIngressTable {
    iceoryx2_node: Iceoryx2Node,
    carried: Arc<Mutex<WhatThisRuntimeIsCarryingFromOtherRuntimes>>,
    /// Set once this runtime is on a mesh and the resolving thread is up.
    resolving: Mutex<Option<ResolvingEveryWaitingLink>>,
}

/// The thread that resolves waiting links, and what wakes it.
struct ResolvingEveryWaitingLink {
    wake_the_resolver: Option<Sender<()>>,
    announcement_subscriber: Option<zenoh::pubsub::Subscriber<()>>,
    resolving_thread: Option<std::thread::JoinHandle<()>>,
}

impl MeshLinkIngressTable {
    /// A table for a runtime whose iceoryx2 node is `iceoryx2_node`.
    pub(crate) fn of_this_runtime(iceoryx2_node: &Iceoryx2Node) -> Arc<Self> {
        Arc::new(Self {
            iceoryx2_node: iceoryx2_node.clone(),
            carried: Arc::new(Mutex::new(
                WhatThisRuntimeIsCarryingFromOtherRuntimes::default(),
            )),
            resolving: Mutex::new(None),
        })
    }

    /// Note a link `connect` has just applied, waiting on `address`.
    pub(crate) fn note_a_link_waiting_on(
        &self,
        address: MeshPortAddress,
        link_id: LinkUniqueId,
        how_far_it_has_got: Arc<Mutex<RemoteLinkResolution>>,
    ) {
        self.carried.lock().links.insert(
            link_id,
            ALinkFromAnotherRuntime {
                address,
                how_far_it_has_got,
                notify_service_name: None,
                the_ingress_knows_about_it: false,
            },
        );
        self.ask_the_resolver_to_look_again();
    }

    /// Record the notify service one link's destination waits on, named by the
    /// wiring op once the destination's side of the channel is open.
    ///
    /// `None` for a destination that drains no listener — a `manual` processor
    /// polls its own ports and is woken by nobody.
    pub(crate) fn note_how_a_links_destination_is_woken(
        &self,
        link_id: &LinkUniqueId,
        notify_service_name: Option<String>,
    ) {
        {
            let mut carried = self.carried.lock();
            let Some(link) = carried.links.get_mut(link_id) else {
                return;
            };
            link.notify_service_name = notify_service_name;
            link.the_ingress_knows_about_it = false;
        }
        self.ask_the_resolver_to_look_again();
    }

    /// Forget a link that has been disconnected, and stop carrying its address
    /// when it was the last link reading it.
    pub(crate) fn forget_a_link(&self, link_id: &LinkUniqueId) {
        let mut carried = self.carried.lock();
        let Some(forgotten) = carried.links.remove(link_id) else {
            return;
        };
        let still_read = carried
            .links
            .values()
            .any(|link| link.address == forgotten.address);
        if !still_read {
            // Dropping the ingress undeclares this runtime's reader token,
            // which is what makes the source stop sending the port.
            carried.carrying.remove(&forgotten.address);
        }
    }

    /// Start resolving waiting links, now that this runtime is on a mesh.
    pub(crate) fn start_resolving_every_waiting_link(
        self: &Arc<Self>,
        session: &zenoh::Session,
        key_space: &RuntimeMeshKeySpace,
        this_runtimes_name: &str,
        peers: &Arc<RuntimeMeshPeerTable>,
    ) {
        let (wake_the_resolver, when_to_look_again) = crossbeam_channel::unbounded();

        // A runtime appearing or leaving is the event every waiting link is
        // waiting on, so the pass runs the moment one does rather than on the
        // next tick.
        let wake_on_a_token = wake_the_resolver.clone();
        let announcement_subscriber = match session
            .liveliness()
            .declare_subscriber(key_space.every_announcement_key())
            .history(false)
            .callback(move |_| {
                let _ = wake_on_a_token.send(());
            })
            .wait()
        {
            Ok(subscriber) => subscriber,
            Err(declare_failure) => {
                tracing::warn!(
                    "this runtime cannot watch the mesh for the runtimes its links wait on, so a \
                     waiting link wires on the next pass rather than at once: {declare_failure}"
                );
                return;
            }
        };

        // The thread holds the shared state rather than the table itself: an
        // `Arc` back to the table would keep it alive for as long as the thread
        // ran, and the thread only ends when the table drops — a cycle that
        // would leave this runtime's iceoryx2 node behind at exit.
        let resolving = ResolvingLinksNeeds {
            session: session.clone(),
            key_space: key_space.clone(),
            this_runtimes_name: this_runtimes_name.to_string(),
            peers: Arc::clone(peers),
            carried: Arc::clone(&self.carried),
            iceoryx2_node: self.iceoryx2_node.clone(),
        };
        match std::thread::Builder::new()
            .name("streamlib-mesh-ingress-table".to_string())
            .spawn(move || {
                resolve_every_waiting_link_until_told_to_stop(resolving, when_to_look_again)
            }) {
            Ok(resolving_thread) => {
                *self.resolving.lock() = Some(ResolvingEveryWaitingLink {
                    wake_the_resolver: Some(wake_the_resolver),
                    announcement_subscriber: Some(announcement_subscriber),
                    resolving_thread: Some(resolving_thread),
                });
            }
            Err(cannot_spawn) => tracing::warn!(
                "this runtime cannot resolve its links from other runtimes for want of a thread, \
                 so each stays waiting: {cannot_spawn}"
            ),
        }
    }

    /// Stop resolving and stop carrying everything — a runtime leaving the
    /// mesh reads nothing from it.
    pub(crate) fn stop(&self) {
        if let Some(mut resolving) = self.resolving.lock().take() {
            drop(resolving.announcement_subscriber.take());
            drop(resolving.wake_the_resolver.take());
            if let Some(resolving_thread) = resolving.resolving_thread.take() {
                if resolving_thread.join().is_err() {
                    tracing::warn!("the mesh ingress-resolving thread panicked");
                }
            }
        }
        let mut carried = self.carried.lock();
        carried.carrying.clear();
        for link in carried.links.values_mut() {
            link.the_ingress_knows_about_it = false;
            *link.how_far_it_has_got.lock() = RemoteLinkResolution::AwaitingRemote {
                reason: "this runtime has left the mesh".to_string(),
            };
        }
    }

    fn ask_the_resolver_to_look_again(&self) {
        if let Some(resolving) = self.resolving.lock().as_ref() {
            if let Some(wake_the_resolver) = resolving.wake_the_resolver.as_ref() {
                let _ = wake_the_resolver.send(());
            }
        }
    }
}

impl Drop for MeshLinkIngressTable {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Everything one resolving pass reads.
struct ResolvingLinksNeeds {
    session: zenoh::Session,
    key_space: RuntimeMeshKeySpace,
    this_runtimes_name: String,
    peers: Arc<RuntimeMeshPeerTable>,
    carried: Arc<Mutex<WhatThisRuntimeIsCarryingFromOtherRuntimes>>,
    iceoryx2_node: Iceoryx2Node,
}

/// The resolving thread's body.
fn resolve_every_waiting_link_until_told_to_stop(
    resolving: ResolvingLinksNeeds,
    when_to_look_again: Receiver<()>,
) {
    loop {
        run_one_resolution_pass(&resolving);
        match when_to_look_again.recv_timeout(HOW_OFTEN_EVERY_WAITING_LINK_IS_LOOKED_AT_AGAIN) {
            Ok(()) => {
                // Take every other wake-up queued behind this one: one pass
                // answers them all.
                while when_to_look_again.try_recv().is_ok() {}
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// Look at every address this runtime links from and move each one on.
fn run_one_resolution_pass(resolving: &ResolvingLinksNeeds) {
    let every_address: Vec<MeshPortAddress> = {
        let carried = resolving.carried.lock();
        let mut addresses: Vec<MeshPortAddress> = carried
            .links
            .values()
            .map(|link| link.address.clone())
            .collect();
        addresses.dedup();
        addresses
    };
    for address in every_address {
        resolve_one_address(resolving, &address);
    }
}

/// Move one address on: start carrying it, stop carrying it, or say what it is
/// still waiting on.
fn resolve_one_address(resolving: &ResolvingLinksNeeds, address: &MeshPortAddress) {
    // Nothing here holds the graph lock, and nothing here is held while the
    // mesh is queried: the query below waits out a timeout, and holding the
    // table across it would block every `connect` and `disconnect`.
    let already_carrying = resolving.carried.lock().carrying.contains_key(address);
    if already_carrying {
        keep_carrying_or_stop(resolving, address);
        return;
    }
    match why_this_address_cannot_be_carried_yet(resolving, address) {
        Some(not_yet) => say_how_far_every_link_from(resolving, address, not_yet),
        None => start_carrying(resolving, address),
    }
}

/// Why `address` cannot be carried yet, or `None` when it can.
fn why_this_address_cannot_be_carried_yet(
    resolving: &ResolvingLinksNeeds,
    address: &MeshPortAddress,
) -> Option<RemoteLinkResolution> {
    let holders = resolving
        .peers
        .every_peer_holding_the_name(&address.runtime_name);
    let [(_, described)] = holders.as_slice() else {
        if holders.is_empty() {
            return Some(RemoteLinkResolution::AwaitingRemote {
                reason: format!("the runtime {} is not on the mesh", address.runtime_name),
            });
        }
        // Two live runtimes holding one name is an address collision: a link
        // that picked one could feed the wrong machine, so it carries from
        // neither until one of them leaves.
        let where_they_are: Vec<String> = holders
            .iter()
            .map(|(announced, _)| {
                format!(
                    "host {} as pid {}",
                    announced.host_identity.as_one_key_chunk(),
                    announced.process_id
                )
            })
            .collect();
        return Some(RemoteLinkResolution::Refused {
            reason: format!(
                "{} live runtimes on this mesh are named {}, on {}. A link naming that runtime \
                 is ambiguous and carries from none of them until all but one leaves.",
                holders.len(),
                address.runtime_name,
                where_they_are.join(" and ")
            ),
        });
    };
    let Some(described) = described else {
        return Some(RemoteLinkResolution::AwaitingRemote {
            reason: format!(
                "the runtime {} is on the mesh and has not yet said what it is",
                address.runtime_name
            ),
        });
    };
    let this_engines_version = env!("CARGO_PKG_VERSION");
    if described.engine_version != this_engines_version {
        return Some(RemoteLinkResolution::Refused {
            reason: format!(
                "the runtime {} runs engine {} and this one runs engine {this_engines_version}. \
                 Before 1.0 there is no wire between two engine versions, so nothing is carried \
                 across one.",
                address.runtime_name, described.engine_version
            ),
        });
    }

    let Some(offered) = ask_a_runtime_what_output_ports_it_offers(
        &resolving.session,
        &resolving.key_space,
        &address.runtime_name,
    ) else {
        return Some(RemoteLinkResolution::AwaitingRemote {
            reason: format!(
                "the runtime {} has not said which output ports it offers",
                address.runtime_name
            ),
        });
    };
    if !offered.offers(&address.processor_display_name, &address.port_name) {
        return Some(RemoteLinkResolution::Refused {
            reason: format!(
                "the runtime {} offers no output port {}/{}. It offers: {}.",
                address.runtime_name,
                address.processor_display_name,
                address.port_name,
                offered.listed_for_a_refusal()
            ),
        });
    }
    None
}

/// Start carrying `address`, and tell every link from it.
fn start_carrying(resolving: &ResolvingLinksNeeds, address: &MeshPortAddress) {
    let ingress = match MeshLinkIngress::start(
        &resolving.session,
        &resolving.key_space,
        &resolving.this_runtimes_name,
        address,
        &resolving.iceoryx2_node,
    ) {
        Ok(ingress) => ingress,
        Err(cannot_start) => {
            say_how_far_every_link_from(
                resolving,
                address,
                RemoteLinkResolution::AwaitingRemote {
                    reason: format!("{address} could not be read off the mesh: {cannot_start}"),
                },
            );
            return;
        }
    };
    let mut carried = resolving.carried.lock();
    carried.carrying.insert(address.clone(), ingress);
    tell_the_ingress_about_every_link_from(&resolving.iceoryx2_node, &mut carried, address);
}

/// Keep carrying `address`, or stop when the source stopped sending it or left
/// the mesh.
fn keep_carrying_or_stop(resolving: &ResolvingLinksNeeds, address: &MeshPortAddress) {
    let the_runtime_left = resolving
        .peers
        .every_peer_holding_the_name(&address.runtime_name)
        .is_empty();
    let the_source_stopped_sending = resolving
        .carried
        .lock()
        .carrying
        .get(address)
        .is_some_and(|ingress| ingress.the_source_stopped_sending());

    if the_runtime_left || the_source_stopped_sending {
        let reason = if the_runtime_left {
            format!("the runtime {} left the mesh", address.runtime_name)
        } else {
            format!(
                "the runtime {} stopped sending {address}",
                address.runtime_name
            )
        };
        {
            let mut carried = resolving.carried.lock();
            carried.carrying.remove(address);
            for link in carried
                .links
                .values_mut()
                .filter(|link| &link.address == address)
            {
                link.the_ingress_knows_about_it = false;
            }
        }
        say_how_far_every_link_from(
            resolving,
            address,
            RemoteLinkResolution::AwaitingRemote { reason },
        );
        return;
    }

    // A destination wired after the ingress opened still has to be told about.
    let mut carried = resolving.carried.lock();
    tell_the_ingress_about_every_link_from(&resolving.iceoryx2_node, &mut carried, address);
}

/// Hand the ingress every destination of `address` it has not been told about,
/// and mark those links carried.
fn tell_the_ingress_about_every_link_from(
    iceoryx2_node: &Iceoryx2Node,
    carried: &mut WhatThisRuntimeIsCarryingFromOtherRuntimes,
    address: &MeshPortAddress,
) {
    let Some(ingress) = carried.carrying.get(address) else {
        return;
    };
    for (link_id, link) in carried.links.iter_mut() {
        if &link.address != address || link.the_ingress_knows_about_it {
            continue;
        }
        // Minted here rather than carried: an iceoryx2 notifier is `!Send`, so
        // it is created on the thread that hands it to the ingress and never
        // moves again.
        let notifier = link.notify_service_name.as_deref().and_then(|notify| {
            iceoryx2_node
                .open_or_create_notify_service(notify, MAX_INBOUND_LINKS_PER_DESTINATION)
                .and_then(|notify_service| notify_service.create_notifier())
                .inspect_err(|cannot_notify| {
                    tracing::warn!(
                        "a destination of {address} will not be woken when a bag arrives, so a \
                         reactive one reads only when something else wakes it: {cannot_notify}"
                    );
                })
                .ok()
        });
        ingress.note_a_local_destination(link_id.as_str(), notifier);
        link.the_ingress_knows_about_it = true;
        *link.how_far_it_has_got.lock() = RemoteLinkResolution::Wired;
    }
}

/// Write `how_far` onto every link from `address`.
fn say_how_far_every_link_from(
    resolving: &ResolvingLinksNeeds,
    address: &MeshPortAddress,
    how_far: RemoteLinkResolution,
) {
    let carried = resolving.carried.lock();
    for link in carried
        .links
        .values()
        .filter(|link| &link.address == address)
    {
        let mut how_far_it_has_got = link.how_far_it_has_got.lock();
        if *how_far_it_has_got != how_far {
            *how_far_it_has_got = how_far.clone();
        }
    }
}
