// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! iceoryx2 Node wrapper for StreamLib runtime.

use std::sync::Arc;

use iceoryx2::node::Node;
use iceoryx2::port::listener::Listener;
use iceoryx2::port::notifier::Notifier;
use iceoryx2::prelude::*;
use parking_lot::Mutex;

use iceoryx2::port::publisher::Publisher;
use iceoryx2::port::subscriber::Subscriber;
use iceoryx2::service::builder::publish_subscribe::{
    Builder as PublishSubscribeServiceBuilder, PublishSubscribeOpenError,
    PublishSubscribeOpenOrCreateError,
};

use super::helper_process_loss_count_board::{
    HelperProcessLossCountBoard, HelperProcessLossCountBoardWriter, inbound_link_slot_keys,
    output_port_keys,
};
use super::{
    DataChannelBagSequenceNumberUserHeader, FRAME_HEADER_SIZE, MAX_PUBLISHERS_PER_CHANNEL,
};
use crate::core::error::{Error, Result};
use crate::core::runtime::current_process_uid;
use streamlib_ipc_types::{
    HelperProcessLossCountBoardKey, InboundLinkLossCountBoardSlot,
    OutputPortRefusedBagCountBoardEntry,
};

/// Nodes a channel or notify service admits per subscriber or notifier slot.
///
/// Every endpoint that is a helper opens the service from its own node, and a
/// node that died holds its place until a sweep reclaims it, so the headroom is
/// what keeps a crashed helper from locking a live one out of a channel.
const ICEORYX2_NODES_ADMITTED_PER_PORT_SLOT: usize = 2;

/// Readers a helper's loss-count board admits: its parent's.
const LOSS_COUNT_BOARD_MAX_READERS: usize = 1;

/// Nodes a helper's loss-count board admits: its parent's and its helper's,
/// with the same headroom a channel gives each port slot for a node a crash left
/// behind.
const LOSS_COUNT_BOARD_MAX_NODES: usize = 2 * ICEORYX2_NODES_ADMITTED_PER_PORT_SLOT;

/// Samples a channel subscriber borrows at once: the receive path copies each
/// sample out and drops it before taking the next.
const CHANNEL_SUBSCRIBER_MAX_BORROWED_SAMPLES: usize = 1;

/// Samples a channel publisher loans at once: every write loans, fills and sends
/// one before the next.
const CHANNEL_PUBLISHER_MAX_LOANED_SAMPLES: usize = 1;

/// Samples a channel replays to a subscriber that connects late: none, since a
/// replayed bag is stale by the time it arrives.
const CHANNEL_HISTORY_SIZE: usize = 0;

/// The environment variable a parent hands its helper the iceoryx2 domain root in.
pub const ICEORYX2_DOMAIN_ROOT_ENVIRONMENT_VARIABLE: &str = "STREAMLIB_ICEORYX2_DOMAIN_ROOT";

/// The most bytes a domain root and its prefix may take together.
///
/// iceoryx2 names its Unix sockets `<root>/<prefix><entity file name>`, and a
/// socket path must fit `sun_path` (108 bytes on Linux, 104 on macOS) with room
/// for the longest name iceoryx2 appends. Past this the first listener fails late
/// as an opaque `ResourceCreationFailed`.
pub const ICEORYX2_DOMAIN_ROOT_AND_PREFIX_BUDGET_BYTES: usize =
    if cfg!(target_os = "macos") { 58 } else { 63 };

/// The file prefix every engine-owned iceoryx2 node of this OS user shares.
pub fn engine_owned_iceoryx2_prefix_for_this_user() -> String {
    format!("sl{}_", current_process_uid())
}

/// The iceoryx2 configuration every engine-owned node, and every static iceoryx2
/// call, uses — built from the library defaults, never from iceoryx2's lookup path.
pub fn engine_owned_iceoryx2_config(domain_root: &std::path::Path) -> Result<Config> {
    iceoryx2_config_for_domain(domain_root, &engine_owned_iceoryx2_prefix_for_this_user())
}

/// Build the configuration for the domain named by `domain_root` and `prefix`.
///
/// iceoryx2 keeps file-backed state under the root but names its POSIX shared
/// memory from the prefix alone, so two domains are disjoint only when their
/// prefixes differ too.
pub(crate) fn iceoryx2_config_for_domain(
    domain_root: &std::path::Path,
    prefix: &str,
) -> Result<Config> {
    let root_bytes = domain_root.as_os_str().as_encoded_bytes();
    let root_and_prefix_bytes = root_bytes.len() + prefix.len();
    if root_and_prefix_bytes > ICEORYX2_DOMAIN_ROOT_AND_PREFIX_BUDGET_BYTES {
        return Err(Error::Configuration(format!(
            "the iceoryx2 domain root {} with prefix {prefix} takes {root_and_prefix_bytes} bytes, \
             past the {ICEORYX2_DOMAIN_ROOT_AND_PREFIX_BUDGET_BYTES}-byte budget a Unix socket \
             path leaves them; set XDG_RUNTIME_DIR to a shorter directory",
            domain_root.display()
        )));
    }

    let mut config = Config::default();
    config
        .global
        .set_root_path(&Path::new(root_bytes).map_err(|refusal| {
            Error::Configuration(format!(
                "the iceoryx2 domain root {} is not a path iceoryx2 accepts: {refusal:?}",
                domain_root.display()
            ))
        })?);
    config.global.prefix = FileName::new(prefix.as_bytes()).map_err(|refusal| {
        Error::Configuration(format!(
            "the iceoryx2 domain prefix {prefix} is not a file name iceoryx2 accepts: {refusal:?}"
        ))
    })?;
    Ok(config)
}

/// Reclaim what every dead iceoryx2 node in the engine-owned domain still holds,
/// and say how many went.
///
/// A dead node keeps its place in every service it had opened, so a channel
/// whose helper process crashed counts that helper against the subscriber cap
/// until a sweep takes it out. The engine's own configuration and never the
/// ambient one: the lookup path would sweep another domain, or none.
///
/// The non-blocking form, because this runs from a liveness poll — a node
/// another process is already cleaning up is left to that process and gone by
/// the next sweep, rather than parking the poll on it.
pub fn reclaim_dead_iceoryx2_nodes_in_engine_owned_domain(
    domain_root: &std::path::Path,
) -> Result<u64> {
    let config = engine_owned_iceoryx2_config(domain_root)?;
    Ok(reclaim_dead_iceoryx2_nodes_in(&config))
}

/// The sweep itself, over a configuration already built — what a node holding
/// its own config runs, so a retry sweeps the domain it is actually opening in.
fn reclaim_dead_iceoryx2_nodes_in(config: &Config) -> u64 {
    let reclaimed = Node::<ipc::Service>::try_cleanup_dead_nodes(config);
    if reclaimed.failed_cleanups > 0 {
        tracing::debug!(
            "{} dead iceoryx2 node(s) could not be reclaimed, which is ordinary when another \
             process holds them",
            reclaimed.failed_cleanups,
        );
    }
    reclaimed.cleanups
}

/// Create a raw iceoryx2 node, labelled `node_name`, in the engine-owned domain rooted at `domain_root`.
pub fn create_iceoryx2_node_in_engine_owned_domain(
    domain_root: &std::path::Path,
    node_name: &str,
) -> Result<Node<ipc::Service>> {
    create_iceoryx2_node_in_domain(
        domain_root,
        &engine_owned_iceoryx2_prefix_for_this_user(),
        node_name,
    )
}

/// Create a raw iceoryx2 node, labelled `node_name`, in the domain named by `domain_root` and `prefix`.
pub(crate) fn create_iceoryx2_node_in_domain(
    domain_root: &std::path::Path,
    prefix: &str,
    node_name: &str,
) -> Result<Node<ipc::Service>> {
    let config = iceoryx2_config_for_domain(domain_root, prefix)?;
    let node_name = NodeName::new(node_name).map_err(|refusal| {
        Error::Configuration(format!(
            "'{node_name}' is not an iceoryx2 node name: {refusal:?}"
        ))
    })?;
    NodeBuilder::new()
        .config(&config)
        .name(&node_name)
        .signal_handling_mode(SignalHandlingMode::Disabled)
        .create::<ipc::Service>()
        .map_err(|failure| {
            Error::Runtime(format!(
                "failed to create iceoryx2 node '{}' in the domain rooted at {}: {failure:?}",
                node_name.as_str(),
                domain_root.display()
            ))
        })
}

/// The sizing a channel data service is created with, and that every opener
/// reopens it at — the parameters iceoryx2 verifies on each open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ChannelSizing {
    /// The fixed destination slot count plus the reserved tap slot.
    pub(crate) max_subscribers: usize,
    /// The deepest ring any subscriber on the channel may take.
    pub(crate) channel_service_creation_depth: usize,
}

/// The publisher of a channel data service: `[u8]` frames under the
/// sequence-number user header.
pub type ChannelDataServicePublisher =
    Publisher<ipc::Service, [u8], DataChannelBagSequenceNumberUserHeader>;

/// A subscriber to a channel data service: `[u8]` frames under the
/// sequence-number user header.
pub type ChannelDataServiceSubscriber =
    Subscriber<ipc::Service, [u8], DataChannelBagSequenceNumberUserHeader>;

/// Thread-safe wrapper for iceoryx2 Node.
///
/// The Node is created once per runtime and shared across all processors.
/// Services, Publishers, and Subscribers are created through this Node.
#[derive(Clone)]
pub struct Iceoryx2Node {
    inner: Arc<Mutex<Node<ipc::Service>>>,
}

impl Iceoryx2Node {
    /// Create a node, labelled `node_name`, in the engine-owned domain rooted at `domain_root`.
    pub fn new(domain_root: &std::path::Path, node_name: &str) -> Result<Self> {
        let node = create_iceoryx2_node_in_engine_owned_domain(domain_root, node_name)?;
        Ok(Self::wrapping(node))
    }

    /// Share an already-created raw node.
    pub(crate) fn wrapping(node: Node<ipc::Service>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(node)),
        }
    }

    /// The iceoryx2 configuration this node was created with.
    #[cfg(test)]
    pub(crate) fn config(&self) -> Config {
        self.inner.lock().config().clone()
    }

    /// Open or create an iceoryx2 Event service for fd-multiplexed wakeups.
    ///
    /// Pairs with a destination's data channels for fd-multiplexed wakeups: the
    /// notify service stays destination-keyed (`streamlib/<dest>/notify`) so a
    /// destination waits on ONE `Listener` fd regardless of fan-in, while every
    /// upstream source publishing into one of its channels holds a `Notifier`
    /// here. `max_notifiers` is the fixed inbound-link cap every opener requests,
    /// never the fan-in of the day.
    pub fn open_or_create_notify_service(
        &self,
        service_name: &str,
        max_notifiers: usize,
    ) -> Result<Iceoryx2NotifyService> {
        let node = self.inner.lock();
        let service_name = iceoryx2_service_name_of(service_name)?;

        let service = node
            .service_builder(&service_name)
            .event()
            .max_notifiers(max_notifiers)
            .max_listeners(1)
            .max_nodes(max_notifiers * ICEORYX2_NODES_ADMITTED_PER_PORT_SLOT)
            .open_or_create()
            .map_err(|e| {
                Error::Runtime(format!("Failed to open/create notify service: {:?}", e))
            })?;

        Ok(Iceoryx2NotifyService { inner: service })
    }

    /// Open or create a channel-centric publish-subscribe service for `[u8]`
    /// slices under [`DataChannelBagSequenceNumberUserHeader`].
    ///
    /// Every opener comes through here or [`Self::open_existing_channel_service`],
    /// which share one builder: iceoryx2 refuses an opener presenting any other
    /// user header.
    ///
    /// The service name is the source-port channel
    /// (`{source_processor}/{source_output_port}`). The service carries exactly
    /// [`MAX_PUBLISHERS_PER_CHANNEL`] (1) publisher — the source — and
    /// `max_subscribers` slots: the fixed destination cap plus the reserved tap
    /// slot ([`crate::iceoryx2::RESERVED_TAP_SUBSCRIBER_SLOTS_PER_CHANNEL`]).
    /// Every opener (the engine and every helper) must request the SAME
    /// `max_subscribers` — iceoryx2 verifies it on `open`.
    ///
    /// `channel_service_creation_depth` is the deepest ring any subscriber on
    /// this service may take. A create fixes it for the service's life; a reopen
    /// asks only that the live service is at least that deep, so every opener
    /// passes the creation depth and never a port's own ring depth.
    ///
    /// Safe overflow is on for every channel service: a full subscriber
    /// buffer auto-evicts its oldest sample so the publisher's `send()`
    /// never blocks.
    ///
    /// A shallower live service refuses the depth this asks for. That is a real
    /// refusal while a live holder is genuinely shallower, and a stale one once
    /// every holder has died: iceoryx2 sweeps dead nodes only after a
    /// *successful* open, so a long-lived node keeps failing the same reopen
    /// until some new node is created anywhere on the machine. This therefore
    /// sweeps its own domain once and retries, which is what turns that
    /// permanent failure back into the transient it is.
    pub fn open_or_create_service(
        &self,
        service_name: &str,
        max_subscribers: usize,
        channel_service_creation_depth: usize,
    ) -> Result<Iceoryx2Service> {
        let node = self.inner.lock();
        let service_name = iceoryx2_service_name_of(service_name)?;

        let open_or_create_once = || {
            channel_data_service_builder(&node, &service_name)
                .max_publishers(MAX_PUBLISHERS_PER_CHANNEL)
                .max_subscribers(max_subscribers)
                .max_nodes(max_subscribers * ICEORYX2_NODES_ADMITTED_PER_PORT_SLOT)
                .subscriber_max_buffer_size(channel_service_creation_depth)
                .subscriber_max_borrowed_samples(CHANNEL_SUBSCRIBER_MAX_BORROWED_SAMPLES)
                .history_size(CHANNEL_HISTORY_SIZE)
                .enable_safe_overflow(true)
                .open_or_create()
        };

        let service = match open_or_create_once() {
            Ok(service) => service,
            Err(
                first_failure @ PublishSubscribeOpenOrCreateError::PublishSubscribeOpenError(
                    PublishSubscribeOpenError::DoesNotSupportRequestedMinBufferSize,
                ),
            ) => {
                let reclaimed = reclaim_dead_iceoryx2_nodes_in(node.config());
                tracing::info!(
                    service = service_name.as_str(),
                    channel_service_creation_depth,
                    reclaimed_dead_nodes = reclaimed,
                    "a live channel service is shallower than this open asks for; swept the \
                     engine's domain for dead holders and retrying once"
                );
                open_or_create_once().map_err(|retry_failure| {
                    channel_data_service_open_failure(
                        &service_name,
                        &retry_failure,
                        Some(&first_failure),
                    )
                })?
            }
            Err(failure) => {
                return Err(channel_data_service_open_failure(
                    &service_name,
                    &failure,
                    None,
                ));
            }
        };

        Ok(Iceoryx2Service { inner: service })
    }

    /// The channel data service named `service_name`, or `None` when nothing
    /// holds one open.
    ///
    /// Opens without creating and requests no sizing, so a live service of any
    /// depth answers rather than refusing the open.
    pub fn open_existing_channel_service(
        &self,
        service_name: &str,
    ) -> Result<Option<Iceoryx2Service>> {
        let node = self.inner.lock();
        let service_name = iceoryx2_service_name_of(service_name)?;

        match channel_data_service_builder(&node, &service_name).open() {
            Ok(service) => Ok(Some(Iceoryx2Service { inner: service })),
            // A service whose last holder has let it go is recreated by the
            // next open-or-create, so it is as good as absent.
            Err(
                PublishSubscribeOpenError::DoesNotExist
                | PublishSubscribeOpenError::IsMarkedForDestruction,
            ) => Ok(None),
            Err(failure @ PublishSubscribeOpenError::IncompatibleTypes) => Err(
                channel_data_service_built_by_something_other_than_this_engine(
                    &service_name,
                    &failure,
                ),
            ),
            Err(failure) => Err(Error::Runtime(format!(
                "Failed to open existing service '{}': {failure:?}",
                service_name.as_str()
            ))),
        }
    }
    /// Create the loss-count board a helper spawn writes on: every inbound-link
    /// slot and an entry per declared output port, all zero.
    pub fn create_helper_process_loss_count_board(
        &self,
        service_name: &str,
        output_port_names: Vec<String>,
    ) -> Result<HelperProcessLossCountBoard> {
        let service = {
            let node = self.inner.lock();
            let iceoryx2_service_name = iceoryx2_service_name_of(service_name)?;
            let mut creator = node
                .service_builder(&iceoryx2_service_name)
                .blackboard_creator::<HelperProcessLossCountBoardKey>()
                .max_readers(LOSS_COUNT_BOARD_MAX_READERS)
                .max_nodes(LOSS_COUNT_BOARD_MAX_NODES);
            for key in inbound_link_slot_keys() {
                creator = creator.add_with_default::<InboundLinkLossCountBoardSlot>(key);
            }
            for key in output_port_keys(output_port_names.len()) {
                creator = creator.add_with_default::<OutputPortRefusedBagCountBoardEntry>(key);
            }
            creator.create().map_err(|refusal| {
                Error::Runtime(format!(
                    "could not create the loss-count board '{service_name}': {refusal:?}"
                ))
            })?
        };
        HelperProcessLossCountBoard::reading(service_name.to_string(), output_port_names, service)
    }

    /// Open the loss-count board a helper's parent created and named, as the
    /// one writer it admits.
    pub fn open_helper_process_loss_count_board_writer(
        &self,
        service_name: &str,
        output_port_names: &[String],
    ) -> Result<HelperProcessLossCountBoardWriter> {
        let service = {
            let node = self.inner.lock();
            let iceoryx2_service_name = iceoryx2_service_name_of(service_name)?;
            node.service_builder(&iceoryx2_service_name)
                .blackboard_opener::<HelperProcessLossCountBoardKey>()
                .open()
                .map_err(|refusal| {
                    Error::Runtime(format!(
                        "could not open the loss-count board '{service_name}' its parent \
                         named: {refusal:?}"
                    ))
                })?
        };
        HelperProcessLossCountBoardWriter::writing(
            service_name.to_string(),
            output_port_names,
            service,
        )
    }
}

/// `service_name` as a name iceoryx2 accepts, refused by name when it is not one.
fn iceoryx2_service_name_of(service_name: &str) -> Result<ServiceName> {
    service_name.try_into().map_err(|e| {
        Error::Configuration(format!("Invalid service name '{}': {:?}", service_name, e))
    })
}

/// The channel data service builder: `[u8]` frames under
/// [`DataChannelBagSequenceNumberUserHeader`], the one type pair every opener
/// presents.
fn channel_data_service_builder(
    node: &Node<ipc::Service>,
    service_name: &ServiceName,
) -> PublishSubscribeServiceBuilder<[u8], DataChannelBagSequenceNumberUserHeader, ipc::Service> {
    node.service_builder(service_name)
        .publish_subscribe::<[u8]>()
        .user_header::<DataChannelBagSequenceNumberUserHeader>()
}

/// The refusal for a failed open-or-create of a channel data service, naming the
/// failure the sweep-and-retry started from when there was one.
fn channel_data_service_open_failure(
    service_name: &ServiceName,
    failure: &PublishSubscribeOpenOrCreateError,
    failure_before_the_dead_node_sweep: Option<&PublishSubscribeOpenOrCreateError>,
) -> Error {
    if let PublishSubscribeOpenOrCreateError::PublishSubscribeOpenError(
        PublishSubscribeOpenError::IncompatibleTypes,
    ) = failure
    {
        return channel_data_service_built_by_something_other_than_this_engine(
            service_name,
            failure,
        );
    }
    match failure_before_the_dead_node_sweep {
        None => Error::Runtime(format!("Failed to open/create service: {failure:?}")),
        Some(first_failure) => Error::Runtime(format!(
            "Failed to open/create service: {failure:?} (still {first_failure:?} after sweeping \
             the engine's iceoryx2 domain for dead holders, so a live holder is genuinely \
             shallower than this open asks for)"
        )),
    }
}

/// The refusal for a channel data service whose type pair is not this engine's.
fn channel_data_service_built_by_something_other_than_this_engine(
    service_name: &ServiceName,
    failure: &dyn std::fmt::Debug,
) -> Error {
    Error::Runtime(format!(
        "channel data service '{}' exists with a sample type other than `[u8]` frames under \
         the `DataChannelBagSequenceNumberUserHeader` user header, so it was built by \
         something other than this engine: {failure:?}",
        service_name.as_str()
    ))
}

/// Handle to an iceoryx2 channel data service.
pub struct Iceoryx2Service {
    inner: iceoryx2::service::port_factory::publish_subscribe::PortFactory<
        ipc::Service,
        [u8],
        DataChannelBagSequenceNumberUserHeader,
    >,
}

impl Iceoryx2Service {
    /// The deepest ring a subscriber may take, read off the live service — on a
    /// reopen that is the depth the service was created at, not what the reopen
    /// asked for.
    pub fn channel_service_creation_depth(&self) -> usize {
        self.inner.static_config().subscriber_max_buffer_size()
    }

    /// Whether iceoryx2 holds this service under safe overflow, read off the
    /// live static config — on a reopen that is the config the service was
    /// created with, not what this call asked for.
    #[cfg(test)]
    pub(crate) fn has_safe_overflow(&self) -> bool {
        self.inner.static_config().has_safe_overflow()
    }

    /// Create a channel publisher under [`AllocationStrategy::PowerOfTwo`].
    ///
    /// `expected_payload_bytes` primes the initial per-slot data segment (the
    /// [`FRAME_HEADER_SIZE`] header is added internally) — it is a HINT, never a
    /// cap. The first loan larger than the primed slot grows the shared-memory
    /// segment (rounded to the next power of two) and subscribers remap
    /// transparently, so an oversized payload delivers instead of failing with
    /// `ExceedsMaxLoanSize`. Every channel primes at
    /// [`DEFAULT_EXPECTED_PAYLOAD_BYTES`](crate::iceoryx2::DEFAULT_EXPECTED_PAYLOAD_BYTES).
    pub fn create_publisher(
        &self,
        expected_payload_bytes: usize,
    ) -> Result<ChannelDataServicePublisher> {
        self.inner
            .publisher_builder()
            .initial_max_slice_len(expected_payload_bytes + FRAME_HEADER_SIZE)
            .allocation_strategy(AllocationStrategy::PowerOfTwo)
            .max_loaned_samples(CHANNEL_PUBLISHER_MAX_LOANED_SAMPLES)
            .create()
            .map_err(|e| Error::Runtime(format!("Failed to create publisher: {:?}", e)))
    }

    /// A publisher primed at `primed_slice_bytes` with no growth strategy — the
    /// counterfactual that proves [`Self::create_publisher`]'s growth is what
    /// lets an oversized loan through.
    #[cfg(test)]
    pub(crate) fn create_publisher_that_cannot_grow(
        &self,
        primed_slice_bytes: usize,
    ) -> ChannelDataServicePublisher {
        self.inner
            .publisher_builder()
            .initial_max_slice_len(primed_slice_bytes)
            .create()
            .expect("a publisher with the library's static allocation strategy")
    }

    /// Create a subscriber whose ring holds `input_port_ring_depth` samples —
    /// the depth of the input port it feeds, at most the service's creation depth.
    pub fn create_subscriber(
        &self,
        input_port_ring_depth: usize,
    ) -> Result<ChannelDataServiceSubscriber> {
        self.inner
            .subscriber_builder()
            .buffer_size(input_port_ring_depth)
            .create()
            .map_err(|e| Error::Runtime(format!("Failed to create subscriber: {:?}", e)))
    }

    /// Create the channel's reserved-slot tap subscriber, discriminating the
    /// slot-exhaustion case from every other transport failure.
    ///
    /// A channel data service is opened with
    /// `max_subscribers = MAX_DESTINATIONS_PER_CHANNEL + RESERVED_TAP_SUBSCRIBER_SLOTS_PER_CHANNEL`.
    /// Destination subscribers take their slots as links are wired; the reserved
    /// slot is what a tap consumes here, with a ring `tap_ring_depth` deep.
    /// iceoryx2 fixes `max_subscribers` at create time, so a tap arriving when
    /// every slot is taken trips
    /// [`iceoryx2::port::subscriber::SubscriberCreateError::ExceedsMaxSupportedSubscribers`]
    /// — surfaced as [`ChannelTapSubscribeError::ReservedSlotOccupied`] so the op
    /// can map it to the named [`Error::TapSlotOccupied`], distinct from a generic
    /// subscribe failure.
    pub fn create_tap_subscriber(
        &self,
        tap_ring_depth: usize,
    ) -> std::result::Result<ChannelDataServiceSubscriber, ChannelTapSubscribeError> {
        use iceoryx2::port::subscriber::SubscriberCreateError;
        self.inner
            .subscriber_builder()
            .buffer_size(tap_ring_depth)
            .create()
            .map_err(|e| match e {
                SubscriberCreateError::ExceedsMaxSupportedSubscribers => {
                    ChannelTapSubscribeError::ReservedSlotOccupied
                }
                other => ChannelTapSubscribeError::Transport(format!("{:?}", other)),
            })
    }
}

/// Why creating a channel's reserved-slot tap subscriber failed.
#[derive(Debug)]
pub enum ChannelTapSubscribeError {
    /// The channel's single reserved tap slot is already taken by another tap —
    /// iceoryx2 rejected the create with `ExceedsMaxSupportedSubscribers`.
    ReservedSlotOccupied,
    /// Any other iceoryx2 subscriber-create failure, rendered for the log.
    Transport(String),
}

/// Handle to an iceoryx2 Event service used for fd-multiplexed wakeups.
///
/// This wraps iceoryx2's `MessagingPattern::Event` — `Notifier::notify()` causes any
/// `Listener` on the same service to become readable on its underlying fd.
pub struct Iceoryx2NotifyService {
    inner: iceoryx2::service::port_factory::event::PortFactory<ipc::Service>,
}

impl Iceoryx2NotifyService {
    /// Create a notifier for this service.
    pub fn create_notifier(&self) -> Result<Notifier<ipc::Service>> {
        self.inner
            .notifier_builder()
            .create()
            .map_err(|e| Error::Runtime(format!("Failed to create notifier: {:?}", e)))
    }

    /// Create a listener for this service.
    pub fn create_listener(&self) -> Result<Listener<ipc::Service>> {
        self.inner
            .listener_builder()
            .create()
            .map_err(|e| Error::Runtime(format!("Failed to create listener: {:?}", e)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::machine_global_unique_name::mint_machine_global_unique_name_suffix;
    use crate::iceoryx2::InboundLinkName;

    fn unique_service_name(tag: &str) -> String {
        format!(
            "test/node/{tag}/{}",
            mint_machine_global_unique_name_suffix()
        )
    }

    /// The destination-keyed notify service honors the requested `max_notifiers`
    /// — exactly that many notifiers can be created and one more must fail.
    /// Every source publishing into one of the destination's channels holds one
    /// notifier here, so the cap is the most inbound links a destination holds.
    #[test]
    fn notify_service_honors_requested_max_notifiers() {
        let fanin = 3usize;
        let node = Iceoryx2Node::for_this_test_process();
        let service = node
            .open_or_create_notify_service(&unique_service_name("notify_cap"), fanin)
            .expect("open notify service");

        let mut notifiers = Vec::with_capacity(fanin);
        for i in 0..fanin {
            notifiers.push(
                service
                    .create_notifier()
                    .unwrap_or_else(|e| panic!("notifier {i} (under cap) must succeed: {e:?}")),
            );
        }
        assert!(
            service.create_notifier().is_err(),
            "creating notifier {} must fail — notify service was opened with \
             max_notifiers={fanin}",
            fanin + 1,
        );
    }

    /// A channel created for the full destination cap admits every one of its
    /// subscribers — the destinations and the tap — each opening the service
    /// from its own node, as helpers do, and refuses one more.
    ///
    /// Fail-without-fix: leave `max_nodes` at iceoryx2's default of 20 and the
    /// twentieth subscriber's node cannot open the service.
    #[test]
    fn a_channel_admits_every_destination_and_the_tap_from_their_own_nodes_and_no_more() {
        use crate::iceoryx2::DeliveryProfile;
        use streamlib_ipc_types::{
            MAX_DESTINATIONS_PER_CHANNEL, RESERVED_TAP_SUBSCRIBER_SLOTS_PER_CHANNEL,
        };

        let max_subscribers =
            MAX_DESTINATIONS_PER_CHANNEL + RESERVED_TAP_SUBSCRIBER_SLOTS_PER_CHANNEL;
        let service_name = unique_service_name("fan_out_from_distinct_nodes");
        let open_from_a_node_of_its_own = || {
            let node = Iceoryx2Node::for_this_test_process();
            let service = node
                .open_or_create_service(
                    &service_name,
                    max_subscribers,
                    DeliveryProfile::ORDERED_DEPTH,
                )
                .expect("every opener's node fits the service");
            (node, service)
        };
        let (_source_node, source_service) = open_from_a_node_of_its_own();
        let _publisher = source_service
            .create_publisher(64)
            .expect("the source publisher");

        let mut subscribers_on_their_own_nodes = Vec::with_capacity(max_subscribers);
        for subscriber_index in 0..max_subscribers {
            let (node, service) = open_from_a_node_of_its_own();
            let subscriber = service
                .create_subscriber(DeliveryProfile::NEWEST_DEPTH)
                .unwrap_or_else(|refusal| {
                    panic!("subscriber {subscriber_index} must fit: {refusal:?}")
                });
            subscribers_on_their_own_nodes.push((node, service, subscriber));
        }

        let (_one_node_too_many, service) = open_from_a_node_of_its_own();
        assert!(
            service
                .create_subscriber(DeliveryProfile::NEWEST_DEPTH)
                .is_err(),
            "subscriber {} is one past the destination cap plus the tap",
            max_subscribers + 1
        );
    }

    /// A destination's notify service created for the full inbound-link cap
    /// admits a notifier from every one of those links' nodes, and refuses one
    /// more.
    ///
    /// Fail-without-fix: leave `max_nodes` at iceoryx2's event default of 36 and
    /// the thirty-sixth notifier's node cannot open the service.
    #[test]
    fn a_notify_service_admits_every_inbound_link_from_their_own_nodes_and_no_more() {
        use streamlib_ipc_types::MAX_INBOUND_LINKS_PER_DESTINATION;

        let service_name = unique_service_name("fan_in_from_distinct_nodes");
        let open_from_a_node_of_its_own = || {
            let node = Iceoryx2Node::for_this_test_process();
            let service = node
                .open_or_create_notify_service(&service_name, MAX_INBOUND_LINKS_PER_DESTINATION)
                .expect("every opener's node fits the service");
            (node, service)
        };
        let (_destination_node, destination_service) = open_from_a_node_of_its_own();
        let _listener = destination_service
            .create_listener()
            .expect("the destination's listener");

        let mut notifiers_on_their_own_nodes =
            Vec::with_capacity(MAX_INBOUND_LINKS_PER_DESTINATION);
        for notifier_index in 0..MAX_INBOUND_LINKS_PER_DESTINATION {
            let (node, service) = open_from_a_node_of_its_own();
            let notifier = service.create_notifier().unwrap_or_else(|refusal| {
                panic!("notifier {notifier_index} must fit: {refusal:?}")
            });
            notifiers_on_their_own_nodes.push((node, service, notifier));
        }

        let (_one_node_too_many, service) = open_from_a_node_of_its_own();
        assert!(
            service.create_notifier().is_err(),
            "notifier {} is one past the inbound-link cap",
            MAX_INBOUND_LINKS_PER_DESTINATION + 1
        );
    }

    /// Every channel service states the sample limits the engine's ports stay
    /// inside: one borrowed sample per subscriber, no history, room for twice
    /// its subscribers in nodes — and a publisher that loans one sample at a
    /// time.
    #[test]
    fn a_channel_service_lends_one_sample_at_a_time_and_replays_none() {
        use crate::iceoryx2::DeliveryProfile;

        let max_subscribers = 3;
        let service = Iceoryx2Node::for_this_test_process()
            .open_or_create_service(
                &unique_service_name("sample_limits"),
                max_subscribers,
                DeliveryProfile::ORDERED_DEPTH,
            )
            .expect("open channel data service");

        let static_config = service.inner.static_config();
        assert_eq!(static_config.subscriber_max_borrowed_samples(), 1);
        assert_eq!(static_config.history_size(), 0);
        assert_eq!(static_config.max_nodes(), max_subscribers * 2);

        let publisher = service.create_publisher(64).expect("the source publisher");
        let _first_loan = publisher.loan_slice_uninit(8).expect("one loan");
        assert!(
            publisher.loan_slice_uninit(8).is_err(),
            "a second loan held beside the first must be refused"
        );
    }

    /// A channel data service carries exactly ONE publisher (the source) and
    /// `N + RESERVED_TAP_SUBSCRIBER_SLOTS_PER_CHANNEL` subscribers. This is the
    /// transport inversion (#1419): the old destination-centric service pinned
    /// `max_subscribers = 1` and let N publishers fan in; a channel service pins
    /// `max_publishers = 1` and lets N subscribers fan OUT one zero-copy loan,
    /// reserving one extra slot for a phase-3.5 tap.
    ///
    /// Mentally-revert: raise `max_publishers` back above 1 in
    /// [`Iceoryx2Node::open_or_create_service`] and the second `create_publisher`
    /// stops failing; drop the reserved tap slot and the (N+1)th subscriber (the
    /// tap) stops fitting. Both halves fail here when the contract is broken.
    #[test]
    fn channel_service_single_publisher_n_plus_tap_subscribers() {
        use streamlib_ipc_types::RESERVED_TAP_SUBSCRIBER_SLOTS_PER_CHANNEL;

        let destinations = 3usize;
        let max_subscribers = destinations + RESERVED_TAP_SUBSCRIBER_SLOTS_PER_CHANNEL;
        let node = Iceoryx2Node::for_this_test_process();
        let service = node
            .open_or_create_service(&unique_service_name("chan_caps"), max_subscribers, 4)
            .expect("open channel data service");

        // Exactly one publisher — the source.
        let _publisher = service.create_publisher(64).expect("the source publisher");
        assert!(
            service.create_publisher(64).is_err(),
            "a channel carries exactly one publisher — max_publishers drifted above \
             MAX_PUBLISHERS_PER_CHANNEL (1)",
        );

        // N destination subscribers plus the one reserved tap slot fit; the slot
        // after that does not.
        let mut subscribers = Vec::with_capacity(max_subscribers);
        for i in 0..max_subscribers {
            subscribers.push(
                service.create_subscriber(4).unwrap_or_else(|e| {
                    panic!("subscriber {i} (destination or tap) must fit: {e:?}")
                }),
            );
        }
        assert!(
            service.create_subscriber(4).is_err(),
            "the {}th subscriber must fail — max_subscribers was N({destinations}) + \
             reserved tap({RESERVED_TAP_SUBSCRIBER_SLOTS_PER_CHANNEL})",
            max_subscribers + 1,
        );
    }

    /// A channel data service is created once and reopened by every subscriber
    /// (each destination + a subprocess SDK opening the same name). iceoryx2
    /// rejects reopening with a LARGER buffer than the existing service —
    /// `DoesNotSupportRequestedMinBufferSize`, the exact crash the drone-racer
    /// pilot hit — but accepts reopening with a SMALLER one. Channel sizing relies
    /// on this: create the service at the channel's declared depth and every
    /// shallower reopen fits, regardless of wiring order. If a future iceoryx2
    /// changes this open-validation behavior, this test is the trip-wire.
    #[test]
    fn channel_service_reopen_larger_fails_smaller_succeeds() {
        let node = Iceoryx2Node::for_this_test_process();
        let subs = 2usize;

        // Bug shape: a shallow-depth open creates the service first, then a
        // deeper reopen is rejected.
        let bug_name = unique_service_name("reopen_bug");
        let _shallow = node
            .open_or_create_service(&bug_name, subs, 4)
            .expect("create channel service at depth 4");
        assert!(
            node.open_or_create_service(&bug_name, subs, 64).is_err(),
            "reopening the channel service with a deeper buffer must fail — \
             this is the DoesNotSupportRequestedMinBufferSize crash the \
             channel-depth sizing prevents",
        );

        // Fix shape: create at the deepest depth first, then every shallower
        // reopen succeeds cleanly.
        let fixed_name = unique_service_name("reopen_fixed");
        let _deep = node
            .open_or_create_service(&fixed_name, subs, 64)
            .expect("create channel service at depth 64");
        node.open_or_create_service(&fixed_name, subs, 4)
            .expect("reopening the channel service with a shallower buffer must succeed");
    }

    /// No link blocks a producer: the subscriber buffer auto-evicts its
    /// oldest sample on overflow and the publisher's `send()` returns
    /// promptly. Sends `depth * 3` samples to a depth-N service whose
    /// subscriber is attached but never drains; every publish must
    /// return promptly. Attaching a non-draining subscriber is
    /// load-bearing — iceoryx2's publisher only observes back-pressure
    /// once at least one subscriber is present (without one, samples are
    /// dropped on the floor at send time regardless of the overflow
    /// flag).
    ///
    /// The behavioural half; `channel_sizing_tests` reads the same
    /// contract off the opened service's static config. Neither catches
    /// the `.enable_safe_overflow(true)` line simply going missing —
    /// iceoryx2 defaults to `true` — but a `false` written there
    /// fails both.
    #[test]
    fn overflow_enabled_publisher_does_not_block_on_full_buffer() {
        use std::time::{Duration, Instant};

        let depth: usize = 4;
        let node = Iceoryx2Node::for_this_test_process();
        let service = node
            .open_or_create_service(&unique_service_name("overflow_true"), 2, depth)
            .expect("open service");
        let publisher = service.create_publisher(64).expect("publisher");
        // Subscriber attached but never read — the buffer fills against
        // it. Required for the publisher to observe back-pressure at
        // all (without a subscriber, sends silently no-op).
        let _subscriber = service.create_subscriber(depth).expect("subscriber");

        let start = Instant::now();
        for _ in 0..(depth * 3) {
            let sample = publisher.loan_slice_uninit(8).expect("loan");
            let sample = sample.write_from_slice(&[0u8; 8]);
            sample.send().expect("send must succeed with overflow on");
        }
        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_millis(200),
            "overflow-on publisher must not block — sent {} samples to a depth-{} ring \
             in {:?}, expected sub-200ms",
            depth * 3,
            depth,
            elapsed,
        );
    }

    /// Every opener of a channel data service presents the sequence-number
    /// user header. iceoryx2 refuses an opener that presents none, and a
    /// service built without it is refused to the engine's own opener by a
    /// message naming the header it lacks.
    #[test]
    fn a_channel_data_service_and_an_opener_disagreeing_on_the_user_header_are_refused_by_name() {
        let node = Iceoryx2Node::for_this_test_process();

        let built_by_the_engine = unique_service_name("header/engine-built");
        let _service = node
            .open_or_create_service(&built_by_the_engine, 2, 4)
            .expect("the engine builds the service with its header");
        let opened_without_the_header = node
            .inner
            .lock()
            .service_builder(&ServiceName::new(&built_by_the_engine).unwrap())
            .publish_subscribe::<[u8]>()
            .open();
        assert!(
            matches!(
                opened_without_the_header,
                Err(iceoryx2::service::builder::publish_subscribe::PublishSubscribeOpenError::IncompatibleTypes)
            ),
            "an opener presenting no user header must be refused: {opened_without_the_header:?}"
        );

        let built_without_the_header = unique_service_name("header/built-without");
        let _foreign_service = node
            .inner
            .lock()
            .service_builder(&ServiceName::new(&built_without_the_header).unwrap())
            .publish_subscribe::<[u8]>()
            .create()
            .unwrap();
        let refusal = match node.open_or_create_service(&built_without_the_header, 2, 4) {
            Ok(_) => panic!("a service built without the header must refuse the engine"),
            Err(refusal) => refusal.to_string(),
        };
        assert!(
            refusal.contains("DataChannelBagSequenceNumberUserHeader")
                && refusal.contains(&built_without_the_header),
            "the refusal must name the header and the service: {refusal}"
        );
    }

    /// A reopen of a live service states the depth the service was created at,
    /// read off iceoryx2 rather than echoed from the call, so a later wire can
    /// see how deep the channel it joins really is.
    #[test]
    fn a_reopened_service_states_the_depth_it_was_created_at() {
        let service_name = unique_service_name("creation_depth_stated");
        let _creating_node_service = Iceoryx2Node::for_this_test_process()
            .open_or_create_service(&service_name, 2, 42)
            .expect("create data service");

        let reopened = Iceoryx2Node::for_this_test_process()
            .open_or_create_service(&service_name, 2, 4)
            .expect("a shallower reopen joins the live service");

        assert_eq!(reopened.channel_service_creation_depth(), 42);
    }

    /// Opening an existing channel service finds none where nothing holds one,
    /// and finds one of any depth where something does.
    #[test]
    fn opening_an_existing_channel_service_finds_one_of_any_depth_and_creates_none() {
        let node = Iceoryx2Node::for_this_test_process();
        let service_name = unique_service_name("open_existing");

        assert!(
            node.open_existing_channel_service(&service_name)
                .expect("a missing service is not an error")
                .is_none(),
            "nothing holds the service, so opening finds none"
        );
        assert!(
            node.open_existing_channel_service(&service_name)
                .expect("a missing service is not an error")
                .is_none(),
            "the first open created nothing"
        );

        let _held_open = node
            .open_or_create_service(&service_name, 2, 3)
            .expect("create data service");
        assert_eq!(
            node.open_existing_channel_service(&service_name)
                .expect("a live service answers")
                .map(|service| service.channel_service_creation_depth()),
            Some(3),
        );
    }

    /// End-to-end smoke test for the 200 Hz two-stage pipeline shape that
    /// motivated this engine fix (the MAVLink stress test in PR #836:
    /// UdpSource → Decoder → Encoder → UdpSink at 200 Hz, where each
    /// stage's drain window was tight against scheduler jitter).
    ///
    /// Producer publishes at 200 Hz to service S1; a relay thread drains
    /// S1 and republishes to S2 while periodically pausing to simulate
    /// downstream jitter; a consumer drains S2 and counts. We run the
    /// pipeline twice:
    ///
    /// 1. **Shallow rings (depth 4)** — during a 50 ms relay pause, the
    ///    producer emits ~10 messages at 200 Hz. The S1 ring (depth 4)
    ///    overflows; ~6 messages per pause are overwritten and lost.
    /// 2. **Deep rings (depth 64, matching MavlinkMessage's declared
    ///    `max_queued_messages`)** — the same 10-message accumulation
    ///    fits comfortably; zero loss.
    ///
    /// Reverting the engine's `subscriber_max_buffer_size` wiring to a
    /// hardcoded constant would either make both runs lose the same way
    /// (low constant) or both runs preserve everything (high constant) —
    /// the asymmetric outcome locks the per-service plumbing.
    ///
    /// Runtime ~1.5 s. Two thread pools per run.
    #[test]
    fn sustained_200hz_two_stage_relay_preserves_messages_only_with_deep_rings() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
        use std::time::{Duration, Instant};

        fn run_relay(
            s1_depth: usize,
            s2_depth: usize,
            total: u32,
            hz: u32,
            relay_pause_every: u32,
            relay_pause_ms: u64,
        ) -> (u32, u32) {
            let interval = Duration::from_micros(1_000_000 / hz as u64);
            // iceoryx2 Publishers/Subscribers are `!Send` (they hold Rc
            // internally), so each thread constructs its own ports from
            // the shared Node + service-name string.
            let node = Iceoryx2Node::for_this_test_process();
            let s1_name = Arc::new(unique_service_name("relay_s1"));
            let s2_name = Arc::new(unique_service_name("relay_s2"));

            let sent_counter = Arc::new(AtomicU32::new(0));
            // Startup barrier — iceoryx2 doesn't queue messages for late
            // subscribers, so the producer must wait until the relay
            // subscriber and consumer subscriber are both attached
            // before publishing the first sample. 3 participants:
            // producer, relay, consumer.
            let startup = Arc::new(std::sync::Barrier::new(3));
            // Two-phase shutdown: relay stops first (drain S1 → S2),
            // then consumer stops (drain S2). One shared flag would
            // race: the consumer's "drain and exit" can finish before
            // the relay republishes the last message.
            let relay_stop = Arc::new(AtomicBool::new(false));
            let consumer_stop = Arc::new(AtomicBool::new(false));

            let node_p = node.clone();
            let s1_p = s1_name.clone();
            let sent_clone = sent_counter.clone();
            let startup_p = startup.clone();
            let producer = std::thread::spawn(move || {
                let svc = node_p
                    .open_or_create_service(&s1_p, 2, s1_depth)
                    .expect("producer s1 open");
                let publisher = svc.create_publisher(64).expect("publisher");
                startup_p.wait();
                let start = Instant::now();
                for i in 0..total {
                    let mut payload = vec![0u8; FRAME_HEADER_SIZE + 4];
                    payload[FRAME_HEADER_SIZE..].copy_from_slice(&i.to_le_bytes());
                    let sample = publisher.loan_slice_uninit(payload.len()).expect("loan");
                    let sample = sample.write_from_slice(&payload);
                    sample.send().expect("send");
                    sent_clone.fetch_add(1, Ordering::Relaxed);

                    let target = start + interval * (i + 1);
                    let now = Instant::now();
                    if target > now {
                        std::thread::sleep(target - now);
                    }
                }
            });

            let node_r = node.clone();
            let s1_r = s1_name.clone();
            let s2_r = s2_name.clone();
            let relay_stop_t = relay_stop.clone();
            let startup_r = startup.clone();
            let relay = std::thread::spawn(move || {
                let svc_in = node_r
                    .open_or_create_service(&s1_r, 2, s1_depth)
                    .expect("relay s1 open");
                let svc_out = node_r
                    .open_or_create_service(&s2_r, 2, s2_depth)
                    .expect("relay s2 open");
                let subscriber = svc_in.create_subscriber(s1_depth).expect("relay sub");
                let publisher = svc_out.create_publisher(64).expect("relay pub");
                startup_r.wait();
                let mut count: u32 = 0;
                let relay_one = |subscriber: &ChannelDataServiceSubscriber,
                                 publisher: &ChannelDataServicePublisher|
                 -> bool {
                    match subscriber.receive() {
                        Ok(Some(sample)) => {
                            let bytes = sample.payload().to_vec();
                            let s = publisher
                                .loan_slice_uninit(bytes.len())
                                .expect("relay loan");
                            let s = s.write_from_slice(&bytes);
                            s.send().expect("relay send");
                            true
                        }
                        _ => false,
                    }
                };
                loop {
                    if relay_stop_t.load(Ordering::Relaxed) {
                        // Drain anything still pending so the very last
                        // producer message makes it to s2.
                        while relay_one(&subscriber, &publisher) {}
                        break;
                    }
                    if relay_one(&subscriber, &publisher) {
                        count += 1;
                        if relay_pause_every > 0 && count % relay_pause_every == 0 {
                            std::thread::sleep(Duration::from_millis(relay_pause_ms));
                        }
                    } else {
                        std::thread::sleep(Duration::from_micros(200));
                    }
                }
            });

            let node_c = node.clone();
            let s2_c = s2_name.clone();
            let consumer_stop_t = consumer_stop.clone();
            let startup_c = startup.clone();
            let consumer_handle = std::thread::spawn(move || {
                let svc = node_c
                    .open_or_create_service(&s2_c, 2, s2_depth)
                    .expect("consumer s2 open");
                let subscriber = svc.create_subscriber(s2_depth).expect("consumer sub");
                startup_c.wait();
                let mut received: u32 = 0;
                loop {
                    if consumer_stop_t.load(Ordering::Relaxed) {
                        while let Ok(Some(_)) = subscriber.receive() {
                            received += 1;
                        }
                        break;
                    }
                    match subscriber.receive() {
                        Ok(Some(_)) => received += 1,
                        Ok(None) => std::thread::sleep(Duration::from_micros(200)),
                        Err(_) => break,
                    }
                }
                received
            });

            // Phased shutdown: producer → settle → relay finishes flushing
            // S1 into S2 → settle → consumer drains S2. Using one shared
            // flag races on the very last message.
            producer.join().expect("producer thread");
            std::thread::sleep(Duration::from_millis(500));
            relay_stop.store(true, Ordering::Relaxed);
            relay.join().expect("relay thread");
            std::thread::sleep(Duration::from_millis(100));
            consumer_stop.store(true, Ordering::Relaxed);
            let received = consumer_handle.join().expect("consumer thread");

            (sent_counter.load(Ordering::Relaxed), received)
        }

        let total: u32 = 100; // 0.5 s at 200 Hz
        let hz: u32 = 200;
        let pause_every: u32 = 25;
        let pause_ms: u64 = 50;

        let (sent_shallow, recv_shallow) = run_relay(4, 4, total, hz, pause_every, pause_ms);
        let (sent_deep, recv_deep) = run_relay(64, 64, total, hz, pause_every, pause_ms);

        assert_eq!(sent_shallow, total, "producer should send every message");
        assert_eq!(sent_deep, total, "producer should send every message");

        assert!(
            recv_shallow < sent_shallow,
            "depth-4 rings should lose messages at 200 Hz under {pause_ms} ms relay pauses (every {pause_every} msgs): sent {sent_shallow}, recv {recv_shallow}"
        );
        assert_eq!(
            recv_deep, sent_deep,
            "depth-64 rings (MavlinkMessage's declared depth) should preserve every message at 200 Hz under the same jitter: sent {sent_deep}, recv {recv_deep}"
        );
    }

    /// End-to-end overwrite behavior: a depth-N ring published with N+1
    /// unread samples drops the oldest in favor of the newest. Locks the
    /// fact that the `subscriber_max_buffer_size` we pass through actually
    /// reaches iceoryx2 — if the wiring is dropped (hardcoded to 16 again),
    /// publishing 17+ messages would only start dropping at the much
    /// larger default and this test would fail.
    #[test]
    fn data_service_honors_small_ring_depth_with_overwrite() {
        let depth: usize = 4;
        let send_count: usize = depth + 3; // 3 extra overwrites
        let node = Iceoryx2Node::for_this_test_process();
        let service = node
            .open_or_create_service(&unique_service_name("ring_overwrite"), 2, depth)
            .expect("open data service");

        let max_payload = 64usize;
        let publisher = service
            .create_publisher(max_payload)
            .expect("create publisher");
        let subscriber = service.create_subscriber(depth).expect("create subscriber");

        for i in 0..send_count {
            let mut payload = vec![0u8; FRAME_HEADER_SIZE + 1];
            payload[FRAME_HEADER_SIZE] = i as u8;
            let sample = publisher
                .loan_slice_uninit(payload.len())
                .expect("loan slot");
            let sample = sample.write_from_slice(&payload);
            sample.send().expect("send must succeed even on overwrite");
        }

        // Drain everything currently in the subscriber's queue.
        let mut received: Vec<u8> = Vec::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
        while std::time::Instant::now() < deadline {
            match subscriber.receive() {
                Ok(Some(sample)) => {
                    received.push(sample.payload()[FRAME_HEADER_SIZE]);
                }
                Ok(None) => break,
                Err(e) => panic!("subscriber.receive() failed: {e:?}"),
            }
        }

        assert!(
            received.len() <= depth,
            "ring depth {depth} must cap the subscriber-side queue; received {} samples",
            received.len()
        );
        assert!(
            !received.is_empty(),
            "subscriber should have received at least one of the published samples"
        );
        // The newest-sent payload must be among what the subscriber drained —
        // "latest wins" semantics: even after overwrites the freshest sample
        // is preserved.
        let newest_sent = (send_count - 1) as u8;
        assert!(
            received.contains(&newest_sent),
            "newest-sent sample (payload byte {newest_sent}) should survive ring overwrites, drained: {received:?}"
        );
    }

    /// PowerOfTwo growth (#1421): a publisher primed with a small
    /// `expected_payload_bytes` HINT must still loan — and deliver — a slice far
    /// larger than the hint, because [`Iceoryx2Service::create_publisher`] opens
    /// under [`iceoryx2::prelude::AllocationStrategy::PowerOfTwo`] and grows the
    /// data segment on the first oversized loan; the subscriber remaps
    /// transparently.
    ///
    /// Fail-without-fix: drop the `.allocation_strategy(AllocationStrategy::PowerOfTwo)`
    /// line in `create_publisher` and iceoryx2 falls back to the Static strategy,
    /// so the oversized `loan_slice_uninit` fails with `ExceedsMaxLoanSize` — the
    /// exact crash class this issue deletes — and the `.expect("loan")` panics.
    #[test]
    fn publisher_grows_segment_for_oversized_loan_and_delivers() {
        let node = Iceoryx2Node::for_this_test_process();
        let service = node
            .open_or_create_service(&unique_service_name("powertwo_growth"), 2, 4)
            .expect("open data service");

        // Prime the publisher with a deliberately tiny 64-byte hint.
        let hint_bytes = 64usize;
        let publisher = service
            .create_publisher(hint_bytes)
            .expect("create PowerOfTwo publisher");
        let subscriber = service.create_subscriber(4).expect("subscriber");

        // Loan a 1 MiB slice — ~16000x the primed slot. Under Static this is an
        // ExceedsMaxLoanSize failure; under PowerOfTwo it grows and succeeds.
        let oversized = 1024 * 1024usize;
        let mut payload = vec![0u8; oversized];
        payload[0] = 0xAB;
        payload[oversized - 1] = 0xCD;

        let sample = publisher
            .loan_slice_uninit(oversized)
            .expect("PowerOfTwo publisher must loan a slice far larger than its primed hint");
        let sample = sample.write_from_slice(&payload);
        sample.send().expect("send oversized sample");

        let received = subscriber
            .receive()
            .expect("receive")
            .expect("subscriber must transparently remap the grown segment and deliver");
        assert_eq!(
            received.payload().len(),
            oversized,
            "full payload delivered"
        );
        assert_eq!(received.payload()[0], 0xAB);
        assert_eq!(received.payload()[oversized - 1], 0xCD);
    }

    /// End-to-end #1549 reconnect cycle: connect → disconnect → reconnect a
    /// persistent source+destination pair the way the compiler op wires it, and
    /// prove the per-link reclaim releases both iceoryx2 services so the reconnect
    /// recreates them fresh — reproducing (and defeating) BOTH reported errors:
    ///
    /// - `ExceedsMaxSupportedNotifiers`: the notify service is created with
    ///   `max_notifiers = fan-in (1)`. Without reclaim, the first connect's
    ///   notifier stays live on [`OutputWriterInner`], so the reconnect's
    ///   `create_notifier` is the SECOND on a max-1 service and fails.
    /// - `DoesNotSupportRequestedMinBufferSize`: the reconnect opens the data
    ///   service at a DEEPER ring depth. Without reclaim, the stale shallow
    ///   service is still held by the leaked publisher/subscriber, so the deeper
    ///   reopen is rejected.
    ///
    /// Fail-without-fix: revert `OutputWriterInner::remove_channel_link` /
    /// `InputMailboxesInner::remove_channel_link` to no-ops (the pre-#1549
    /// `close_iceoryx2_service` behaviour) and the reconnect's `create_notifier`
    /// trips `ExceedsMaxSupportedNotifiers` and the deeper `open_or_create_service`
    /// trips `DoesNotSupportRequestedMinBufferSize` — the two `.expect`s panic.
    #[test]
    fn disconnect_reconnect_cycle_reclaims_notifier_and_data_service() {
        use crate::iceoryx2::{
            ChannelEgressConfig, ChannelTrustTier, InputMailboxesInner, OutputWriterInner,
            ReadMode, TRUSTED_CHANNEL_PAYLOAD_CEILING_BYTES,
        };
        use streamlib_ipc_types::RESERVED_TAP_SUBSCRIBER_SLOTS_PER_CHANNEL;

        let node = Iceoryx2Node::for_this_test_process();
        let data_name = unique_service_name("cycle/data");
        let notify_name = unique_service_name("cycle/notify");
        let link_id = "L-cycle-1";
        // fan-in = 1 (one inbound link), fan-out = 1 (+ reserved tap slot).
        let max_notifiers = 1usize;
        let max_subscribers = 1 + RESERVED_TAP_SUBSCRIBER_SLOTS_PER_CHANNEL;

        let out_inner = Arc::new(OutputWriterInner::new());
        let in_inner = Arc::new(InputMailboxesInner::new());

        // One `connect()` wiring pass at ring depth `depth`, exactly as the
        // compiler op stitches a Rust→Rust link.
        let connect = |depth: usize| {
            let data = node
                .open_or_create_service(&data_name, max_subscribers, depth)
                .expect("open channel data service");
            let notify = node
                .open_or_create_notify_service(&notify_name, max_notifiers)
                .expect("open notify service");

            if !out_inner.has_channel_publisher("out") {
                let publisher = data.create_publisher(64).expect("source publisher");
                out_inner.set_channel_publisher(
                    "out",
                    publisher,
                    ChannelEgressConfig {
                        service_name: data_name.clone(),
                        trust_tier: ChannelTrustTier::Trusted,
                        expected_payload_bytes: 64,
                        ceiling_bytes: TRUSTED_CHANNEL_PAYLOAD_CEILING_BYTES,
                    },
                );
            }
            out_inner.add_channel_link(
                "out",
                link_id,
                Some(notify.create_notifier().expect(
                    "create_notifier must fit the notify service's max_notifiers cap — a \
                     leaked notifier from the previous connect would trip \
                     ExceedsMaxSupportedNotifiers here",
                )),
            );

            if !in_inner.has_port("in") {
                in_inner.add_port("in", depth, ReadMode::ReadNextInOrder);
            }
            in_inner.add_channel_subscriber(
                "in",
                link_id,
                &InboundLinkName::from("psource/out"),
                data.create_subscriber(depth).expect("dest subscriber"),
            );
            if !in_inner.has_listener() {
                in_inner.set_listener(notify.create_listener().expect("dest listener"));
            }
        };

        // Simulate `close_iceoryx2_service`'s per-link reclaim on both halves.
        let disconnect = || {
            out_inner.remove_channel_link("out", link_id);
            in_inner.remove_channel_link(link_id);
        };

        // First connect at a shallow depth.
        connect(4);
        assert!(out_inner.has_channel_publisher("out"));
        assert!(in_inner.has_listener());

        // Disconnect: the reclaim must drop the publisher, notifier, subscriber
        // and listener so iceoryx2 releases both services.
        disconnect();
        assert!(
            !out_inner.has_channel_publisher("out"),
            "the channel publisher must be released on disconnect",
        );
        assert!(
            !in_inner.has_listener(),
            "the destination listener must be released on disconnect",
        );

        // Reconnect at a DEEPER ring depth. Both `.expect`s inside `connect`
        // (create_notifier + open_or_create_service) are the #1549 trip-wires.
        connect(64);
        assert!(out_inner.has_channel_publisher("out"));
        assert!(in_inner.has_listener());
    }

    fn names_of_the_live_nodes_in(config: &Config) -> Vec<String> {
        use iceoryx2::node::NodeView;

        let mut names = Vec::new();
        Node::<ipc::Service>::list(config, |node_state| {
            if let NodeState::Alive(view) = node_state {
                if let Some(details) = view.details() {
                    names.push(details.name().as_str().to_string());
                }
            }
            CallbackProgression::Continue
        })
        .expect("the domain's nodes can be listed with its own config");
        names
    }

    #[test]
    fn a_domain_root_past_the_socket_path_budget_is_refused_by_name() {
        let prefix = engine_owned_iceoryx2_prefix_for_this_user();
        let root_at_the_budget = format!(
            "/{}",
            "r".repeat(ICEORYX2_DOMAIN_ROOT_AND_PREFIX_BUDGET_BYTES - prefix.len() - 1)
        );
        let root_one_byte_past_it = format!("{root_at_the_budget}r");

        engine_owned_iceoryx2_config(std::path::Path::new(&root_at_the_budget))
            .expect("a root exactly at the budget is accepted");
        let refusal = match create_iceoryx2_node_in_engine_owned_domain(
            std::path::Path::new(&root_one_byte_past_it),
            "streamlib-test",
        ) {
            Ok(_) => panic!("a root one byte past the budget must be refused before any node"),
            Err(refusal) => refusal.to_string(),
        };

        assert!(refusal.contains(&root_one_byte_past_it), "{refusal}");
        assert!(
            refusal.contains(&format!(
                "{ICEORYX2_DOMAIN_ROOT_AND_PREFIX_BUDGET_BYTES}-byte budget"
            )),
            "{refusal}"
        );
        assert!(
            !std::path::Path::new(&root_one_byte_past_it).exists(),
            "a refused root must never be created"
        );
    }

    /// Set only in the child process the dead-node test re-runs itself in.
    const DEAD_NODE_CHILD_DOMAIN_ROOT_ENVIRONMENT_VARIABLE: &str =
        "STREAMLIB_TEST_DEAD_NODE_CHILD_ICEORYX2_DOMAIN_ROOT";

    /// A node is dead only once the process that opened it is gone, so the node
    /// is opened in a child test process that is then killed where it stands —
    /// the same self-re-run shape the working-directory test uses.
    #[test]
    fn a_node_whose_process_was_killed_is_reclaimed_by_the_sweep() {
        if let Some(domain_root) =
            std::env::var_os(DEAD_NODE_CHILD_DOMAIN_ROOT_ENVIRONMENT_VARIABLE)
        {
            let _node = Iceoryx2Node::new(
                std::path::Path::new(&domain_root),
                "streamlib-test/killed-where-it-stood",
            )
            .expect("a node opens in the engine-owned domain");
            // SAFETY: this process signalling itself, which is what leaves the
            // node registered with no process behind it.
            unsafe { libc::kill(std::process::id() as libc::pid_t, libc::SIGKILL) };
            unreachable!("SIGKILL to self does not return");
        }

        let domain = tempfile::tempdir().unwrap();
        let domain_root = domain.path().join("iox2");

        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "iceoryx2::node::tests::a_node_whose_process_was_killed_is_reclaimed_by_the_sweep",
                "--exact",
                "--test-threads=1",
            ])
            .env(
                DEAD_NODE_CHILD_DOMAIN_ROOT_ENVIRONMENT_VARIABLE,
                &domain_root,
            )
            .output()
            .expect("the test binary re-runs this test in a child process");
        assert!(
            !child.status.success(),
            "the child must die where it stood rather than report a result"
        );

        let reclaimed = reclaim_dead_iceoryx2_nodes_in_engine_owned_domain(&domain_root)
            .expect("the sweep runs against the engine-owned domain");

        assert_eq!(
            reclaimed, 1,
            "the killed child's node was left registered with no process behind it"
        );
        assert_eq!(
            reclaim_dead_iceoryx2_nodes_in_engine_owned_domain(&domain_root).unwrap(),
            0,
            "a swept domain has nothing left to reclaim"
        );
    }

    /// Set only in the child process the stale-holder test re-runs itself in.
    const STALE_HOLDER_CHILD_DOMAIN_ROOT_ENVIRONMENT_VARIABLE: &str =
        "STREAMLIB_TEST_STALE_HOLDER_CHILD_ICEORYX2_DOMAIN_ROOT";

    /// The channel the stale holder leaves behind, shared by both processes.
    const STALE_HOLDER_CHANNEL_SERVICE_NAME: &str = "test/stale-holder/frames_to_downstream";

    /// The depth the holder creates the channel at, and the deeper one a later
    /// destination asks for — `newest`'s old depth against `ordered`'s.
    const STALE_HOLDER_SHALLOW_DEPTH: usize = 4;
    const STALE_HOLDER_DEEPER_DEPTH: usize = 16;
    const STALE_HOLDER_MAX_SUBSCRIBERS: usize = 4;

    /// Mental-revert guard for the sweep-and-retry: drop it and the assertion
    /// below that the raw builder still fails is what this open does forever —
    /// iceoryx2 sweeps dead nodes only after a *successful* open, so the app
    /// process's long-lived node keeps failing the same reopen until some new
    /// node happens to be created anywhere on the machine.
    #[test]
    fn a_deeper_open_survives_a_shallow_service_a_dead_holder_left_behind() {
        if let Some(domain_root) =
            std::env::var_os(STALE_HOLDER_CHILD_DOMAIN_ROOT_ENVIRONMENT_VARIABLE)
        {
            let node = Iceoryx2Node::new(
                std::path::Path::new(&domain_root),
                "streamlib-test/stale-holder",
            )
            .expect("the holder's node opens in the engine-owned domain");
            let _shallow_channel = node
                .open_or_create_service(
                    STALE_HOLDER_CHANNEL_SERVICE_NAME,
                    STALE_HOLDER_MAX_SUBSCRIBERS,
                    STALE_HOLDER_SHALLOW_DEPTH,
                )
                .expect("the holder creates the channel at its own shallow depth");
            // SAFETY: this process signalling itself, which is what leaves the
            // service held by a node with no process behind it.
            unsafe { libc::kill(std::process::id() as libc::pid_t, libc::SIGKILL) };
            unreachable!("SIGKILL to self does not return");
        }

        let domain = tempfile::tempdir().unwrap();
        let domain_root = domain.path().join("iox2");

        // This node is created BEFORE the holder dies and is never replaced, so
        // nothing but an explicit sweep can clear the dead holder out from under
        // it — the app process's own arrangement.
        let long_lived_node = Iceoryx2Node::new(&domain_root, "streamlib-test/long-lived")
            .expect("the long-lived node opens first");

        let holder = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "iceoryx2::node::tests::a_deeper_open_survives_a_shallow_service_a_dead_holder_left_behind",
                "--exact",
                "--test-threads=1",
            ])
            .env(
                STALE_HOLDER_CHILD_DOMAIN_ROOT_ENVIRONMENT_VARIABLE,
                &domain_root,
            )
            .output()
            .expect("the test binary re-runs this test in a holder process");
        assert!(
            !holder.status.success(),
            "the holder must die where it stood rather than report a result"
        );

        // The trap, proven before the fix is exercised: a plain open of the same
        // depth still fails, because a failed open sweeps nothing.
        {
            let raw_node = long_lived_node.inner.lock();
            let service_name = iceoryx2_service_name_of(STALE_HOLDER_CHANNEL_SERVICE_NAME).unwrap();
            let unswept_failure = channel_data_service_builder(&raw_node, &service_name)
                .max_publishers(MAX_PUBLISHERS_PER_CHANNEL)
                .max_subscribers(STALE_HOLDER_MAX_SUBSCRIBERS)
                .max_nodes(STALE_HOLDER_MAX_SUBSCRIBERS * ICEORYX2_NODES_ADMITTED_PER_PORT_SLOT)
                .subscriber_max_buffer_size(STALE_HOLDER_DEEPER_DEPTH)
                .subscriber_max_borrowed_samples(CHANNEL_SUBSCRIBER_MAX_BORROWED_SAMPLES)
                .history_size(CHANNEL_HISTORY_SIZE)
                .enable_safe_overflow(true)
                .open_or_create();
            assert!(
                matches!(
                    unswept_failure,
                    Err(
                        PublishSubscribeOpenOrCreateError::PublishSubscribeOpenError(
                            PublishSubscribeOpenError::DoesNotSupportRequestedMinBufferSize
                        )
                    )
                ),
                "the dead holder's shallow service must be what blocks the deeper open, \
                 got {unswept_failure:?}"
            );
        }

        let deeper_channel = long_lived_node
            .open_or_create_service(
                STALE_HOLDER_CHANNEL_SERVICE_NAME,
                STALE_HOLDER_MAX_SUBSCRIBERS,
                STALE_HOLDER_DEEPER_DEPTH,
            )
            .expect("the sweep-and-retry must get past a shallow service nothing live holds");
        assert_eq!(
            deeper_channel.channel_service_creation_depth(),
            STALE_HOLDER_DEEPER_DEPTH,
            "the recreated service must carry the depth this open asked for"
        );
    }

    #[test]
    fn the_sweep_reads_the_engine_owned_domain_and_never_the_ambient_one() {
        // The root-and-prefix budget belongs to `engine_owned_iceoryx2_config`,
        // so a refusal by name here is the proof the sweep is built from that
        // configuration rather than from iceoryx2's own lookup path.
        let prefix = engine_owned_iceoryx2_prefix_for_this_user();
        let root_one_byte_past_the_budget = format!(
            "/{}",
            "r".repeat(ICEORYX2_DOMAIN_ROOT_AND_PREFIX_BUDGET_BYTES - prefix.len())
        );

        let refusal = reclaim_dead_iceoryx2_nodes_in_engine_owned_domain(std::path::Path::new(
            &root_one_byte_past_the_budget,
        ))
        .expect_err("a root past the budget is refused before any listing")
        .to_string();

        assert!(
            refusal.contains(&root_one_byte_past_the_budget),
            "{refusal}"
        );
    }

    const HIJACKED_MAX_SUBSCRIBERS: usize = 3;

    /// Set only in the child process the working-directory test re-runs itself in.
    const CWD_CONFIG_CHILD_DOMAIN_ROOT_ENVIRONMENT_VARIABLE: &str =
        "STREAMLIB_TEST_CWD_CONFIG_CHILD_ICEORYX2_DOMAIN_ROOT";

    /// The working directory is process-wide, so the node opens in a child test
    /// process started inside the directory holding the config file; changing
    /// this process's directory would move it under every test running beside it.
    #[test]
    fn an_iceoryx2_toml_in_the_working_directory_has_no_effect_on_a_node() {
        if let Some(domain_root) =
            std::env::var_os(CWD_CONFIG_CHILD_DOMAIN_ROOT_ENVIRONMENT_VARIABLE)
        {
            open_a_node_beside_a_working_directory_iceoryx2_toml(std::path::Path::new(
                &domain_root,
            ));
            return;
        }

        let working_directory = tempfile::tempdir().unwrap();
        let hijacked_root = working_directory.path().join("hijacked");
        std::fs::create_dir(working_directory.path().join("config")).unwrap();
        std::fs::write(
            working_directory
                .path()
                .join("config")
                .join("iceoryx2.toml"),
            format!(
                "[global]\nroot-path = \"{}\"\nprefix = \"hijack_\"\n\n\
                 [defaults.publish-subscribe]\nmax-subscribers = {HIJACKED_MAX_SUBSCRIBERS}\n",
                hijacked_root.display()
            ),
        )
        .unwrap();
        let domain = tempfile::tempdir().unwrap();
        let domain_root = domain.path().join("iox2");

        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "iceoryx2::node::tests::an_iceoryx2_toml_in_the_working_directory_has_no_effect_on_a_node",
                "--exact",
                "--test-threads=1",
            ])
            .env(CWD_CONFIG_CHILD_DOMAIN_ROOT_ENVIRONMENT_VARIABLE, &domain_root)
            .current_dir(working_directory.path())
            .output()
            .expect("the test binary re-runs this test in a child process");
        let child_stdout = String::from_utf8_lossy(&child.stdout);
        assert!(
            child.status.success() && child_stdout.contains("1 passed"),
            "the child test process must run this test and pass\nstdout:\n{child_stdout}\nstderr:\n{}",
            String::from_utf8_lossy(&child.stderr)
        );
        assert!(
            !hijacked_root.exists(),
            "nothing may be written where the working directory's config points"
        );
    }

    fn open_a_node_beside_a_working_directory_iceoryx2_toml(domain_root: &std::path::Path) {
        assert!(
            std::path::Path::new("config/iceoryx2.toml").is_file(),
            "the child must start in the directory holding the config file"
        );

        let node = Iceoryx2Node::new(domain_root, "streamlib-test/cwd-config-ignored")
            .expect("a node opens in the engine-owned domain");

        let config = node.config();
        assert_eq!(
            config.global.root_path().as_bytes_const(),
            domain_root.as_os_str().as_encoded_bytes()
        );
        assert_eq!(
            config.global.prefix.as_bytes_const(),
            engine_owned_iceoryx2_prefix_for_this_user().as_bytes()
        );
        assert_ne!(
            Config::default().defaults.publish_subscribe.max_subscribers,
            HIJACKED_MAX_SUBSCRIBERS
        );
        assert_eq!(
            config.defaults.publish_subscribe.max_subscribers,
            Config::default().defaults.publish_subscribe.max_subscribers,
            "a value the engine never sets must still be the library default, not the file's"
        );
        assert!(
            names_of_the_live_nodes_in(&engine_owned_iceoryx2_config(domain_root).unwrap())
                .contains(&"streamlib-test/cwd-config-ignored".to_string()),
            "the named node must be listed in the engine-owned domain"
        );
    }

    #[test]
    fn two_test_process_domains_share_neither_files_nor_shared_memory() {
        let first_process = tempfile::tempdir().unwrap();
        let second_process = tempfile::tempdir().unwrap();
        let first_root = first_process.path().join("iox2");
        let second_root = second_process.path().join("iox2");
        // Prefixes of exited pids, so the next test process's sweep reclaims the
        // shared memory this test leaves behind.
        let uid = current_process_uid();
        let exited = crate::iceoryx2::iceoryx2_domain_for_this_test_process::tests::a_process_id_that_has_exited;
        let first_prefix =
            crate::iceoryx2::iceoryx2_domain_for_this_test_process::test_domain_prefix(
                uid,
                exited(),
            );
        let second_prefix =
            crate::iceoryx2::iceoryx2_domain_for_this_test_process::test_domain_prefix(
                uid,
                exited(),
            );
        let service_name = ServiceName::new(&unique_service_name("disjoint")).unwrap();

        let first_node =
            create_iceoryx2_node_in_domain(&first_root, &first_prefix, "streamlib-test/first")
                .unwrap();
        let _service_in_the_first_domain = first_node
            .service_builder(&service_name)
            .publish_subscribe::<[u8]>()
            .create()
            .expect("the first domain creates the service");

        let second_node =
            create_iceoryx2_node_in_domain(&second_root, &second_prefix, "streamlib-test/second")
                .unwrap();
        assert!(
            second_node
                .service_builder(&service_name)
                .publish_subscribe::<[u8]>()
                .open()
                .is_err(),
            "a service created in one domain must not be visible from another"
        );
        let _service_in_the_second_domain = second_node
            .service_builder(&service_name)
            .publish_subscribe::<[u8]>()
            .create()
            .expect("the same service name creates afresh in a domain with its own shared memory");
        assert!(
            !names_of_the_live_nodes_in(
                &iceoryx2_config_for_domain(&second_root, &second_prefix).unwrap()
            )
            .contains(&"streamlib-test/first".to_string())
        );

        let another_node_in_the_first_domain = create_iceoryx2_node_in_domain(
            &first_root,
            &first_prefix,
            "streamlib-test/first-again",
        )
        .unwrap();
        another_node_in_the_first_domain
            .service_builder(&service_name)
            .publish_subscribe::<[u8]>()
            .open()
            .expect("the same root and prefix are the same domain");
    }
}
