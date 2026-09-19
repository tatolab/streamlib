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

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};
use parking_lot::Mutex;
use zenoh::Wait;

use crate::core::graph::{LinkUniqueId, MeshPortAddress, RemoteLinkResolution};
use crate::core::runtime::mesh::OutputPortsOfferedOnTheMesh;
use crate::core::runtime::mesh::mesh_link_ingress::MeshLinkIngress;
use crate::core::runtime::mesh::output_ports_offered_on_the_mesh::ask_a_runtime_what_output_ports_it_offers;
use crate::core::runtime::mesh::runtime_mesh_description::RuntimeMeshDescription;
use crate::core::runtime::mesh::runtime_mesh_key::{AnnouncedRuntimeIdentity, RuntimeMeshKeySpace};
use crate::core::runtime::mesh::runtime_mesh_peer_table::RuntimeMeshPeerTable;
use crate::iceoryx2::{Iceoryx2Node, MeshHopDroppedBagCountsByRemoteInboundLink};
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
    /// Whether the wiring op has opened this link's destination side. Until it
    /// has, the link is not carrying whatever the mesh has managed: `wired`
    /// means the ingress *and* the local destination, never one of the two.
    its_destination_is_open: bool,
    /// The notify service the destination waits on, named by the wiring op —
    /// `None` for a destination that drains no listener. The name rather than
    /// a notifier: an iceoryx2 notifier is `!Send`, so it is minted on the
    /// thread that hands it to the ingress.
    notify_service_name: Option<String>,
    /// Where this link's hop loss is counted — the counts on its destination
    /// processor's node, which `graph` renders. `None` until the wiring op
    /// reports the destination open, and for a fixture standing a link up with
    /// no node to count on.
    where_its_hop_loss_is_counted: Option<Arc<MeshHopDroppedBagCountsByRemoteInboundLink>>,
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
    /// Cleared to stop the thread. The wake-up channel cannot say so on its
    /// own: every ingress holds a sender, and an ingress outlives this.
    whether_to_keep_resolving: Arc<AtomicBool>,
    wake_the_resolver: Sender<()>,
    announcement_subscriber: zenoh::pubsub::Subscriber<()>,
    resolving_thread: std::thread::JoinHandle<()>,
}

impl MeshLinkIngressTable {
    /// A table for a runtime whose iceoryx2 node is `iceoryx2_node`.
    pub fn of_this_runtime(iceoryx2_node: &Iceoryx2Node) -> Arc<Self> {
        Arc::new(Self {
            iceoryx2_node: iceoryx2_node.clone(),
            carried: Arc::new(Mutex::new(
                WhatThisRuntimeIsCarryingFromOtherRuntimes::default(),
            )),
            resolving: Mutex::new(None),
        })
    }

    /// Note a link `connect` has just applied, waiting on `address`.
    pub fn note_a_link_waiting_on(
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
                its_destination_is_open: false,
                notify_service_name: None,
                where_its_hop_loss_is_counted: None,
                the_ingress_knows_about_it: false,
            },
        );
        self.ask_the_resolver_to_look_again();
    }

    /// Record the notify service one link's destination waits on and where its
    /// hop loss is counted, both named by the wiring op once the destination's
    /// side of the channel is open.
    ///
    /// A `None` notify service is a destination that drains no listener — a
    /// `manual` processor polls its own ports and is woken by nobody. The
    /// counts are the destination processor's own, which is what puts this
    /// link's hop loss on that processor's node in `graph`.
    ///
    /// Reachable rather than supported: the cross-runtime-link fixture stands a
    /// runtime's mesh half up with no compiler, so it reports its own
    /// destination the way the wiring op reports a real one.
    #[doc(hidden)]
    pub fn note_how_a_links_destination_is_woken(
        &self,
        link_id: &LinkUniqueId,
        notify_service_name: Option<String>,
        where_its_hop_loss_is_counted: Option<Arc<MeshHopDroppedBagCountsByRemoteInboundLink>>,
    ) {
        {
            let mut carried = self.carried.lock();
            let Some(link) = carried.links.get_mut(link_id) else {
                return;
            };
            link.its_destination_is_open = true;
            link.notify_service_name = notify_service_name;
            link.where_its_hop_loss_is_counted = where_its_hop_loss_is_counted;
            link.the_ingress_knows_about_it = false;
        }
        self.ask_the_resolver_to_look_again();
    }

    /// Whether the wiring op has reported this link's destination open — the
    /// half of `wired` that is not the mesh's.
    #[cfg(test)]
    pub fn a_links_destination_is_open(&self, link_id: &LinkUniqueId) -> bool {
        self.carried
            .lock()
            .links
            .get(link_id)
            .is_some_and(|link| link.its_destination_is_open)
    }

    /// Forget a link that has been disconnected, and stop carrying its address
    /// when it was the last link reading it.
    pub(crate) fn forget_a_link(&self, link_id: &LinkUniqueId) {
        // Taken out under the lock and dropped outside it: an ingress's drop
        // undeclares a token over the network and joins a thread, and this runs
        // under the graph lock too — a disconnect would hold both across it.
        let stopped_reading = {
            let mut carried = self.carried.lock();
            let Some(forgotten) = carried.links.remove(link_id) else {
                return;
            };
            let still_read = carried
                .links
                .values()
                .any(|link| link.address == forgotten.address);
            // Given back only where it was given: a link the ingress was
            // never told about has nothing of the ingress's to return, and the
            // ingress outlives this link because other links read the same
            // port. A destination that left must stop being notified and stop
            // being charged for what the hop loses after it.
            if still_read && forgotten.the_ingress_knows_about_it {
                if let Some(ingress) = carried.carrying.get(&forgotten.address) {
                    ingress.forget_a_local_destination(link_id.as_str());
                }
            }
            // Dropping the ingress undeclares this runtime's reader token,
            // which is what makes the source stop sending the port.
            (!still_read)
                .then(|| carried.carrying.remove(&forgotten.address))
                .flatten()
        };
        drop(stopped_reading);
    }

    /// Start resolving waiting links, now that this runtime is on a mesh.
    pub fn start_resolving_every_waiting_link(
        self: &Arc<Self>,
        session: &zenoh::Session,
        key_space: &RuntimeMeshKeySpace,
        this_runtimes_name: &str,
        peers: &Arc<RuntimeMeshPeerTable>,
    ) {
        // Any resolver already running goes first. Replacing the handle would
        // only drop it: the thread holds a sender of its own, so the channel
        // never disconnects, and its stop flag would stay set for as long as
        // the process lived.
        self.stop_resolving();

        let (wake_the_resolver, when_to_look_again) = crossbeam_channel::unbounded();
        let whether_to_keep_resolving = Arc::new(AtomicBool::new(true));

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
            wake_the_resolver: wake_the_resolver.clone(),
        };
        let whether_this_thread_keeps_resolving = Arc::clone(&whether_to_keep_resolving);
        match std::thread::Builder::new()
            .name("streamlib-mesh-ingress-table".to_string())
            .spawn(move || {
                resolve_every_waiting_link_until_told_to_stop(
                    resolving,
                    when_to_look_again,
                    &whether_this_thread_keeps_resolving,
                )
            }) {
            Ok(resolving_thread) => {
                *self.resolving.lock() = Some(ResolvingEveryWaitingLink {
                    whether_to_keep_resolving,
                    wake_the_resolver,
                    announcement_subscriber,
                    resolving_thread,
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
    pub fn stop(&self) {
        self.stop_resolving();
        let stopped_reading = {
            let mut carried = self.carried.lock();
            for link in carried.links.values_mut() {
                link.the_ingress_knows_about_it = false;
                *link.how_far_it_has_got.lock() = RemoteLinkResolution::AwaitingRemote {
                    reason: "this runtime has left the mesh".to_string(),
                };
            }
            std::mem::take(&mut carried.carrying)
        };
        drop(stopped_reading);
    }

    /// End the resolving thread, if one is running. Idempotent.
    fn stop_resolving(&self) {
        // Taken out of the lock before any of it is torn down: the join waits
        // out whatever the pass is in the middle of, and the wiring op asks
        // this same lock to wake the resolver while it holds the graph lock.
        let resolving = self.resolving.lock().take();
        if let Some(resolving) = resolving {
            drop(resolving.announcement_subscriber);
            resolving
                .whether_to_keep_resolving
                .store(false, Ordering::Release);
            let _ = resolving.wake_the_resolver.send(());
            if resolving.resolving_thread.join().is_err() {
                tracing::warn!("the mesh ingress-resolving thread panicked");
            }
        }
    }

    fn ask_the_resolver_to_look_again(&self) {
        if let Some(resolving) = self.resolving.lock().as_ref() {
            let _ = resolving.wake_the_resolver.send(());
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
    /// Handed to each ingress, so the pass runs the moment the source starts or
    /// stops sending rather than on the next tick.
    wake_the_resolver: Sender<()>,
}

/// The resolving thread's body.
fn resolve_every_waiting_link_until_told_to_stop(
    resolving: ResolvingLinksNeeds,
    when_to_look_again: Receiver<()>,
    whether_to_keep_resolving: &AtomicBool,
) {
    while whether_to_keep_resolving.load(Ordering::Acquire) {
        run_one_resolution_pass(&resolving, whether_to_keep_resolving);
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
///
/// The stop flag is read between addresses as well as between passes: one
/// address whose runtime is present but not answering costs the whole
/// offered-ports timeout, so a pass over several would eat a shutdown budget.
fn run_one_resolution_pass(
    resolving: &ResolvingLinksNeeds,
    whether_to_keep_resolving: &AtomicBool,
) {
    // A set rather than a deduplicated list: the links are keyed by link id, so
    // two reading one address are not adjacent, and each address is resolved
    // once a pass however many links read it.
    let every_address: BTreeSet<MeshPortAddress> = {
        let carried = resolving.carried.lock();
        carried
            .links
            .values()
            .map(|link| link.address.clone())
            .collect()
    };
    for address in every_address {
        if !whether_to_keep_resolving.load(Ordering::Acquire) {
            return;
        }
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
    if let Err(not_yet) = what_the_runtimes_holding_the_name_say(
        address,
        &resolving
            .peers
            .every_peer_holding_the_name(&address.runtime_name()),
        env!("CARGO_PKG_VERSION"),
    ) {
        return Some(not_yet);
    }
    let offered = ask_a_runtime_what_output_ports_it_offers(
        &resolving.session,
        &resolving.key_space,
        &address.runtime_name(),
    );
    what_the_offered_ports_say(address, offered.as_ref()).err()
}

/// What the runtimes announced under this address's name mean for it: nothing
/// to carry from, one to carry from, or an ambiguity that refuses the link.
fn what_the_runtimes_holding_the_name_say(
    address: &MeshPortAddress,
    holders: &[(AnnouncedRuntimeIdentity, Option<RuntimeMeshDescription>)],
    this_engines_version: &str,
) -> std::result::Result<(), RemoteLinkResolution> {
    let [(_, described)] = holders else {
        if holders.is_empty() {
            return Err(RemoteLinkResolution::AwaitingRemote {
                reason: format!("the runtime {} is not on the mesh", address.runtime_name()),
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
        return Err(RemoteLinkResolution::Refused {
            reason: format!(
                "{} live runtimes on this mesh are named {}, on {}. A link naming that runtime \
                 is ambiguous and carries from none of them until all but one leaves.",
                holders.len(),
                address.runtime_name(),
                where_they_are.join(" and ")
            ),
        });
    };
    let Some(described) = described else {
        return Err(RemoteLinkResolution::AwaitingRemote {
            reason: format!(
                "the runtime {} is on the mesh and has not yet said what it is",
                address.runtime_name()
            ),
        });
    };
    if described.engine_version != this_engines_version {
        return Err(RemoteLinkResolution::Refused {
            reason: format!(
                "the runtime {} runs engine {} and this one runs engine {this_engines_version}. \
                 Before 1.0 there is no wire between two engine versions, so nothing is carried \
                 across one.",
                address.runtime_name(),
                described.engine_version
            ),
        });
    }
    Ok(())
}

/// What a runtime's answer about its output ports means for this address.
fn what_the_offered_ports_say(
    address: &MeshPortAddress,
    offered: Option<&OutputPortsOfferedOnTheMesh>,
) -> std::result::Result<(), RemoteLinkResolution> {
    let Some(offered) = offered else {
        return Err(RemoteLinkResolution::AwaitingRemote {
            reason: format!(
                "the runtime {} has not said which output ports it offers",
                address.runtime_name()
            ),
        });
    };
    // Before the missing-port refusal, because the two read differently to the
    // reader: a port that is not there is one to go and add, and a port that is
    // there and unsendable is one to fix. Refused rather than waited on for the
    // same reason a missing port is — nothing the reader can do makes the
    // source's own port sendable, so waiting would be waiting on a condition
    // that cannot change.
    if let Some(why_it_cannot_be_sent) =
        offered.why_it_cannot_send(address.processor_display_name(), address.port_name())
    {
        return Err(RemoteLinkResolution::Refused {
            reason: format!(
                "the runtime {} holds the output port {}/{} and cannot send it: \
                 {why_it_cannot_be_sent}. It offers: {}.",
                address.runtime_name(),
                address.processor_display_name(),
                address.port_name(),
                offered.listed_for_a_refusal()
            ),
        });
    }
    if !offered.offers(&address.processor_display_name(), &address.port_name()) {
        return Err(RemoteLinkResolution::Refused {
            reason: format!(
                "the runtime {} offers no output port {}/{}. It offers: {}.",
                address.runtime_name(),
                address.processor_display_name(),
                address.port_name(),
                offered.listed_for_a_refusal()
            ),
        });
    }
    Ok(())
}

/// Start carrying `address`, and tell every link from it.
fn start_carrying(resolving: &ResolvingLinksNeeds, address: &MeshPortAddress) {
    let ingress = match MeshLinkIngress::start(
        &resolving.session,
        &resolving.key_space,
        &resolving.this_runtimes_name,
        address,
        &resolving.iceoryx2_node,
        resolving.wake_the_resolver.clone(),
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
        .every_peer_holding_the_name(&address.runtime_name())
        .is_empty();
    let the_source_stopped_sending = resolving
        .carried
        .lock()
        .carrying
        .get(address)
        .is_some_and(|ingress| ingress.the_source_stopped_sending());

    if the_runtime_left || the_source_stopped_sending {
        let reason = if the_runtime_left {
            format!("the runtime {} left the mesh", address.runtime_name())
        } else {
            format!(
                "the runtime {} stopped sending {address}",
                address.runtime_name()
            )
        };
        let stopped_reading = {
            let mut carried = resolving.carried.lock();
            for link in carried
                .links
                .values_mut()
                .filter(|link| &link.address == address)
            {
                link.the_ingress_knows_about_it = false;
            }
            carried.carrying.remove(address)
        };
        drop(stopped_reading);
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

/// How far one link from `address` has got, once this runtime's ingress for it
/// is open.
///
/// All three conditions, not two: the ingress being open is the caller's
/// premise, and the other two are read here. A link that reported `wired` on
/// the first two alone would say it was carrying over a port nothing was
/// sending — and then fall back to `awaiting_remote` when an egress it never
/// had went away.
fn how_far_a_link_from_here_has_got(
    address: &MeshPortAddress,
    its_destination_is_open: bool,
    the_source_is_sending: bool,
) -> RemoteLinkResolution {
    match (its_destination_is_open, the_source_is_sending) {
        (false, _) => RemoteLinkResolution::AwaitingRemote {
            reason: format!("{address} is being read and this link is not wired to it yet"),
        },
        (true, false) => RemoteLinkResolution::AwaitingRemote {
            reason: format!(
                "the runtime {} is on the mesh and offers {}/{}, and is not sending it",
                address.runtime_name(),
                address.processor_display_name(),
                address.port_name()
            ),
        },
        (true, true) => RemoteLinkResolution::Wired,
    }
}

/// Hand the ingress every destination of `address` it has not been told about,
/// and say how far each link from it has got.
///
/// A link reads `wired` only once all three hold: this runtime's ingress is
/// open, the wiring op has opened the link's destination, and the source
/// runtime holds an egress token for the port. The third is what keeps "wired
/// with nothing crossing" out of `graph` — a source that never manages to send
/// the port, for any reason, leaves the link saying so rather than claiming to
/// carry.
fn tell_the_ingress_about_every_link_from(
    iceoryx2_node: &Iceoryx2Node,
    carried: &mut WhatThisRuntimeIsCarryingFromOtherRuntimes,
    address: &MeshPortAddress,
) {
    let Some(ingress) = carried.carrying.get(address) else {
        return;
    };
    let the_source_is_sending = ingress.the_source_is_sending();
    for (link_id, link) in carried.links.iter_mut() {
        if &link.address != address {
            continue;
        }
        if link.its_destination_is_open && !link.the_ingress_knows_about_it {
            // Minted here rather than carried: an iceoryx2 notifier is `!Send`,
            // so it is created on the thread that hands it to the ingress and
            // never moves again.
            let notifier = link.notify_service_name.as_deref().and_then(|notify| {
                iceoryx2_node
                    .open_or_create_notify_service(notify, MAX_INBOUND_LINKS_PER_DESTINATION)
                    .and_then(|notify_service| notify_service.create_notifier())
                    .inspect_err(|cannot_notify| {
                        tracing::warn!(
                            "a destination of {address} will not be woken when a bag arrives, so \
                             a reactive one reads only when something else wakes it: \
                             {cannot_notify}"
                        );
                    })
                    .ok()
            });
            // Forgotten and minted again rather than continued: this is
            // every way a link is wired afresh — the source runtime
            // returning, its egress returning, or a reconnect of the same id
            // — and the plan restarts a remote link's loss count at each.
            let where_its_hop_loss_is_counted = link
                .where_its_hop_loss_is_counted
                .as_ref()
                .map(|counts| counts.a_counter_for_a_fresh_wiring_of(link_id.as_str()))
                .unwrap_or_default();
            ingress.note_a_local_destination(
                link_id.as_str(),
                notifier,
                where_its_hop_loss_is_counted,
            );
            link.the_ingress_knows_about_it = true;
        }

        *link.how_far_it_has_got.lock() = how_far_a_link_from_here_has_got(
            address,
            link.its_destination_is_open,
            the_source_is_sending,
        );
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::runtime::mesh::{HostIdentity, OutputPortOfferedOnTheMesh};

    fn an_address() -> MeshPortAddress {
        MeshPortAddress::new("bench-cam-a1b2", "CameraSource", "video").expect("a legal address")
    }

    fn a_runtime_on(
        host: &str,
        process_id: u32,
        engine_version: &str,
    ) -> (AnnouncedRuntimeIdentity, Option<RuntimeMeshDescription>) {
        (
            AnnouncedRuntimeIdentity {
                runtime_name: "bench-cam-a1b2".to_string(),
                host_identity: HostIdentity::ThisKernelBootAndPidNamespace {
                    kernel_boot_id: host.to_string(),
                    pid_namespace_inode: 4_026_531_836,
                },
                process_id,
            },
            Some(RuntimeMeshDescription {
                runtime_id: "R7".to_string(),
                host_name: host.to_string(),
                pid: process_id,
                engine_version: engine_version.to_string(),
                control_plane_urls: vec![],
            }),
        )
    }

    fn a_listing_offering(
        ports: &[(&str, &str)],
    ) -> crate::core::runtime::mesh::OutputPortsOfferedOnTheMesh {
        crate::core::runtime::mesh::OutputPortsOfferedOnTheMesh {
            ports: ports
                .iter()
                .map(|(display, port)| OutputPortOfferedOnTheMesh {
                    processor_display_name: display.to_string(),
                    port_name: port.to_string(),
                })
                .collect(),
            ports_it_holds_and_cannot_send: vec![],
        }
    }

    fn a_listing_holding_and_unable_to_send(
        held: &[(&str, &str, &str)],
    ) -> crate::core::runtime::mesh::OutputPortsOfferedOnTheMesh {
        crate::core::runtime::mesh::OutputPortsOfferedOnTheMesh {
            ports: vec![],
            ports_it_holds_and_cannot_send: held
                .iter()
                .map(|(display, port, why)| {
                    crate::core::runtime::mesh::OutputPortThisRuntimeHoldsAndCannotSend {
                        processor_display_name: display.to_string(),
                        port_name: port.to_string(),
                        why_it_cannot_be_sent: why.to_string(),
                    }
                })
                .collect(),
        }
    }

    fn the_reason(outcome: std::result::Result<(), RemoteLinkResolution>) -> String {
        match outcome.expect_err("this address cannot be carried") {
            RemoteLinkResolution::AwaitingRemote { reason }
            | RemoteLinkResolution::Refused { reason } => reason,
            RemoteLinkResolution::Wired => panic!("a wired link has no reason"),
        }
    }

    /// A runtime nobody has announced leaves the link waiting, naming the
    /// runtime — which is what a reader has to go and start.
    #[test]
    fn a_runtime_that_is_not_on_the_mesh_leaves_the_link_waiting_naming_it() {
        let outcome =
            what_the_runtimes_holding_the_name_say(&an_address(), &[], env!("CARGO_PKG_VERSION"));
        assert!(matches!(
            outcome,
            Err(RemoteLinkResolution::AwaitingRemote { .. })
        ));
        assert!(the_reason(outcome).contains("bench-cam-a1b2"));
    }

    /// A runtime whose token is here and which has not described itself yet is
    /// still waiting rather than refused: the answer is on its way.
    #[test]
    fn a_runtime_that_has_not_described_itself_is_waited_on_rather_than_refused() {
        let (announced, _) = a_runtime_on("desk", 7, env!("CARGO_PKG_VERSION"));
        let outcome = what_the_runtimes_holding_the_name_say(
            &an_address(),
            &[(announced, None)],
            env!("CARGO_PKG_VERSION"),
        );
        assert!(matches!(
            outcome,
            Err(RemoteLinkResolution::AwaitingRemote { .. })
        ));
    }

    /// A different engine version refuses the link naming both versions. Before
    /// 1.0 there is no wire between two of them.
    #[test]
    fn a_different_engine_version_refuses_the_link_naming_both() {
        let outcome = what_the_runtimes_holding_the_name_say(
            &an_address(),
            &[a_runtime_on("desk", 7, "0.1.0-from-another-age")],
            "0.25.2",
        );
        assert!(matches!(outcome, Err(RemoteLinkResolution::Refused { .. })));
        let reason = the_reason(outcome);
        assert!(reason.contains("0.1.0-from-another-age"), "{reason}");
        assert!(reason.contains("0.25.2"), "{reason}");
    }

    /// A name two live runtimes hold refuses the link naming both hosts, and
    /// carries from neither — one of them would be the wrong machine.
    #[test]
    fn a_name_two_live_runtimes_hold_refuses_the_link_naming_both_hosts() {
        let outcome = what_the_runtimes_holding_the_name_say(
            &an_address(),
            &[
                a_runtime_on("desk-boot-id", 7, env!("CARGO_PKG_VERSION")),
                a_runtime_on("rig-boot-id", 9, env!("CARGO_PKG_VERSION")),
            ],
            env!("CARGO_PKG_VERSION"),
        );
        assert!(matches!(outcome, Err(RemoteLinkResolution::Refused { .. })));
        let reason = the_reason(outcome);
        assert!(reason.contains("desk-boot-id"), "{reason}");
        assert!(reason.contains("rig-boot-id"), "{reason}");
    }

    /// Exactly one runtime, on this engine version, is carried from.
    #[test]
    fn one_runtime_on_this_engine_version_is_carried_from() {
        assert!(
            what_the_runtimes_holding_the_name_say(
                &an_address(),
                &[a_runtime_on("desk", 7, env!("CARGO_PKG_VERSION"))],
                env!("CARGO_PKG_VERSION"),
            )
            .is_ok()
        );
    }

    /// A runtime that has not listed its ports is waited on; one that has, and
    /// offers no such port, is refused listing what it does offer.
    #[test]
    fn a_missing_port_is_refused_listing_what_the_runtime_does_offer() {
        assert!(matches!(
            what_the_offered_ports_say(&an_address(), None),
            Err(RemoteLinkResolution::AwaitingRemote { .. })
        ));

        let outcome = what_the_offered_ports_say(
            &an_address(),
            Some(&a_listing_offering(&[
                ("MicrophoneSource", "audio"),
                ("CameraSource", "depth"),
            ])),
        );
        assert!(matches!(outcome, Err(RemoteLinkResolution::Refused { .. })));
        let reason = the_reason(outcome);
        assert!(reason.contains("CameraSource/video"), "{reason}");
        assert!(reason.contains("MicrophoneSource/audio"), "{reason}");
        assert!(reason.contains("CameraSource/depth"), "{reason}");
    }

    /// A port the source holds and cannot send refuses the link with the
    /// source's own reason — never the missing-port words for a port that is
    /// plainly there.
    ///
    /// Mental-revert: drop the `why_it_cannot_send` arm from
    /// `what_the_offered_ports_say` and this goes red on the reason, not on the
    /// state — the missing-port refusal below catches the same address, and
    /// tells the reader to go and add a port its graph already has. The state is
    /// what the offer's own split fixes.
    #[test]
    fn a_port_the_source_holds_and_cannot_send_refuses_the_link_with_its_reason() {
        let outcome = what_the_offered_ports_say(
            &an_address(),
            Some(&a_listing_holding_and_unable_to_send(&[(
                "CameraSource",
                "video",
                "its channel cannot be named: it contains 'V'",
            )])),
        );
        assert!(
            matches!(outcome, Err(RemoteLinkResolution::Refused { .. })),
            "a port that can never be sent is refused, not waited on; it read {outcome:?}"
        );
        let reason = the_reason(outcome);
        assert!(reason.contains("CameraSource/video"), "{reason}");
        assert!(reason.contains("cannot send it"), "{reason}");
        assert!(reason.contains("it contains 'V'"), "{reason}");
        assert!(
            !reason.contains("offers no output port"),
            "a port that is there is not reported as missing: {reason}"
        );
    }

    /// The reason names what *is* on offer beside it, so a reader refused over
    /// one port learns where to point its link without a second query.
    #[test]
    fn a_held_and_unsendable_port_is_refused_listing_what_the_runtime_does_offer() {
        let mut listing = a_listing_offering(&[("MicrophoneSource", "audio")]);
        listing.ports_it_holds_and_cannot_send =
            a_listing_holding_and_unable_to_send(&[("CameraSource", "video", "no channel name")])
                .ports_it_holds_and_cannot_send;

        let reason = the_reason(what_the_offered_ports_say(&an_address(), Some(&listing)));
        assert!(reason.contains("MicrophoneSource/audio"), "{reason}");
    }

    /// A runtime offering the port is carried from.
    #[test]
    fn a_runtime_offering_the_port_is_carried_from() {
        assert!(
            what_the_offered_ports_say(
                &an_address(),
                Some(&a_listing_offering(&[("CameraSource", "video")])),
            )
            .is_ok()
        );
    }

    /// A port that has never started being sent is not one that stopped: the
    /// reader token has only just gone up and the egress is still coming.
    #[test]
    fn a_port_that_never_started_being_sent_is_not_one_that_stopped() {
        let sending = crate::core::runtime::mesh::mesh_link_ingress::SourceSendingState::default();
        assert!(!sending.it_was_sending_and_stopped());
    }

    /// The address every resolution test below reads.
    fn the_address_being_read() -> MeshPortAddress {
        MeshPortAddress::new("bench-cam-a1b2", "CameraSource", "video").expect("a legal address")
    }

    /// An ingress that is open over a port nobody is sending is not a link that
    /// is carrying. Without this the link would claim `wired` while no bag
    /// could cross, and then drop to `awaiting_remote` when an egress it never
    /// had went away.
    #[test]
    fn a_link_whose_source_is_not_sending_the_port_is_not_wired() {
        let how_far = how_far_a_link_from_here_has_got(&the_address_being_read(), true, false);
        let RemoteLinkResolution::AwaitingRemote { reason } = how_far else {
            panic!("a port nobody is sending is not wired; it read {how_far:?}");
        };
        assert!(reason.contains("bench-cam-a1b2"), "{reason}");
        assert!(reason.contains("is not sending it"), "{reason}");
    }

    /// The source sending the port is what turns an open ingress and an open
    /// destination into a link that is carrying.
    #[test]
    fn a_link_is_wired_once_its_destination_is_open_and_its_source_is_sending() {
        assert_eq!(
            how_far_a_link_from_here_has_got(&the_address_being_read(), true, true),
            RemoteLinkResolution::Wired
        );
    }

    /// A destination the wiring op has not opened yet keeps the link waiting
    /// however hard the source is sending — the bags have nowhere to land.
    #[test]
    fn a_link_whose_destination_is_not_open_is_not_wired_however_hard_the_source_sends() {
        for the_source_is_sending in [false, true] {
            let how_far = how_far_a_link_from_here_has_got(
                &the_address_being_read(),
                false,
                the_source_is_sending,
            );
            let RemoteLinkResolution::AwaitingRemote { reason } = how_far else {
                panic!("a link with no open destination is not wired; it read {how_far:?}");
            };
            assert!(reason.contains("not wired to it yet"), "{reason}");
        }
    }
}
