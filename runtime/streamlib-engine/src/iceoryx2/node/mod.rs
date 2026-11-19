// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! iceoryx2 Node wrapper for StreamLib runtime.

use std::sync::Arc;

use iceoryx2::identifiers::UniqueSubscriberId;
use iceoryx2::node::{Node, NodeCleanupFailure, NodeView};
use iceoryx2::port::listener::Listener;
use iceoryx2::port::notifier::Notifier;
use iceoryx2::prelude::*;
use parking_lot::Mutex;

use iceoryx2::service::builder::publish_subscribe::{
    Builder as PublishSubscribeServiceBuilder, PublishSubscribeOpenError,
    PublishSubscribeOpenOrCreateError,
};

use super::helper_process_loss_count_board::{
    HelperProcessLossCountBoard, HelperProcessLossCountBoardWriter, inbound_link_slot_keys,
    output_port_keys,
};
use super::stream_clock_board::{
    StreamClockBoard, StreamClockBoardPublishListener, StreamClockBoardReader,
    stream_clock_board_publish_event_service_name,
};
use super::{
    DataChannelPayloadCacheLine, DataChannelSampleUserHeader, MAX_INPUT_PORT_BORROW_QUOTA,
    MAX_PUBLISHERS_PER_CHANNEL,
};
use crate::core::error::{Error, Result};
pub use streamlib_ipc_types::{
    ChannelDataServiceLoanedSample, ChannelDataServicePublisher, ChannelDataServiceSample,
    ChannelDataServiceSubscriber, ChannelReservedSlotSubscribeError,
    ICEORYX2_DOMAIN_ROOT_AND_PREFIX_BUDGET_BYTES, engine_owned_iceoryx2_prefix_for_this_user,
};
use streamlib_ipc_types::{
    ChannelDataServicePortFactory, Iceoryx2DomainRefusal, channel_data_service_builder,
    create_the_reserved_slot_subscriber_of,
};
use streamlib_ipc_types::{
    HelperProcessFunctionNodeCallBreadcrumbBoardEntry, HelperProcessFunctionNodeOverrunBoardEntry,
    HelperProcessFunctionNodeStateBoardEntry, HelperProcessLossCountBoardKey,
    HelperProcessStepProgressBoardEntry, InboundLinkLossCountBoardSlot,
    OutputPortBagCountBoardEntry, StreamClockBoardEntry, StreamClockBoardKey,
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

/// Readers a stream clock board admits: one per helper process the stream
/// may hold at once.
const STREAM_CLOCK_BOARD_MAX_READERS: usize = 256;

/// Nodes a stream clock board admits: its engine's and every reader's, with
/// the same headroom a channel gives each port slot for a node a crash left
/// behind.
const STREAM_CLOCK_BOARD_MAX_NODES: usize =
    ICEORYX2_NODES_ADMITTED_PER_PORT_SLOT * (STREAM_CLOCK_BOARD_MAX_READERS + 1);

/// Samples a channel publisher loans at once: every write loans, fills and sends
/// one before the next.
const CHANNEL_PUBLISHER_MAX_LOANED_SAMPLES: usize = 1;

/// Samples a channel replays to a subscriber that connects late: none, since a
/// replayed bag is stale by the time it arrives.
const CHANNEL_HISTORY_SIZE: usize = 0;

/// How many times a channel open refused for depth reopens before giving up.
///
/// iceoryx2 reclaims a service's dead holders on every open, but a process
/// that has just died is not reclaimable the instant it goes — measured at
/// about 10 ms on Linux 7.0 — so the open that follows a helper crash can lose
/// that race. Three covers the measured window several times over.
const DEAD_HOLDER_RECLAIM_ATTEMPTS: usize = 3;

/// How long to wait before the first reopen, doubling for each one after it.
/// Holds the whole recovery to about 70 ms, so a wiring call the caller is
/// waiting on is never parked long.
const DEAD_HOLDER_RECLAIM_FIRST_RETRY_INTERVAL: std::time::Duration =
    std::time::Duration::from_millis(10);

/// The environment variable a parent hands its helper the iceoryx2 domain root in.
pub const ICEORYX2_DOMAIN_ROOT_ENVIRONMENT_VARIABLE: &str = "STREAMLIB_ICEORYX2_DOMAIN_ROOT";

/// The iceoryx2 configuration every engine-owned node, and every static iceoryx2
/// call, uses — built from the library defaults, never from iceoryx2's lookup path.
pub fn engine_owned_iceoryx2_config(domain_root: &std::path::Path) -> Result<Config> {
    iceoryx2_config_for_domain(domain_root, &engine_owned_iceoryx2_prefix_for_this_user())
}

/// The configuration of the domain named by `domain_root` and `prefix`; see
/// [`streamlib_ipc_types::iceoryx2_config_for_domain`].
pub(crate) fn iceoryx2_config_for_domain(
    domain_root: &std::path::Path,
    prefix: &str,
) -> Result<Config> {
    streamlib_ipc_types::iceoryx2_config_for_domain(domain_root, prefix)
        .map_err(engine_error_of_an_iceoryx2_domain_refusal)
}

fn engine_error_of_an_iceoryx2_domain_refusal(refusal: Iceoryx2DomainRefusal) -> Error {
    match refusal {
        Iceoryx2DomainRefusal::Configuration(reason) => Error::Configuration(reason),
        Iceoryx2DomainRefusal::NodeCreation(reason) => Error::Runtime(reason),
    }
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
    let mut reclaimed_dead_nodes = 0u64;
    let mut dead_nodes_another_cleaner_is_reclaiming = 0u64;
    let listing = Node::<ipc_threadsafe::Service>::list(&config, |node_state| {
        if let NodeState::Dead(dead_node) = node_state {
            let dead_node_id = *dead_node.id();
            match dead_node.try_remove_stale_resources() {
                Ok(()) | Err(NodeCleanupFailure::ResourcesAlreadyCleanedUp) => {
                    reclaimed_dead_nodes += 1
                }
                Err(NodeCleanupFailure::AnotherInstanceIsCleaningUpTheNode) => {
                    dead_nodes_another_cleaner_is_reclaiming += 1
                }
                Err(cannot_reclaim) => tracing::warn!(
                    "dead iceoryx2 node {dead_node_id:?} cannot be reclaimed and keeps its place \
                     in every service it had opened: {cannot_reclaim:?}"
                ),
            }
        }
        CallbackProgression::Continue
    });
    if let Err(listing_failure) = listing {
        tracing::debug!(
            "the engine's iceoryx2 domain could not be listed in full, so the sweep reclaimed \
             only what it reached: {listing_failure:?}"
        );
    }
    if dead_nodes_another_cleaner_is_reclaiming > 0 {
        tracing::debug!(
            "{dead_nodes_another_cleaner_is_reclaiming} dead iceoryx2 node(s) are being \
             reclaimed by another process or thread",
        );
    }
    Ok(reclaimed_dead_nodes)
}

/// Create a raw iceoryx2 node, labelled `node_name`, in the engine-owned domain rooted at `domain_root`.
pub fn create_iceoryx2_node_in_engine_owned_domain(
    domain_root: &std::path::Path,
    node_name: &str,
) -> Result<Node<ipc_threadsafe::Service>> {
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
) -> Result<Node<ipc_threadsafe::Service>> {
    streamlib_ipc_types::create_iceoryx2_node_in_domain(domain_root, prefix, node_name)
        .map_err(engine_error_of_an_iceoryx2_domain_refusal)
}

/// The sizing a channel data service is created with, and that every opener
/// reopens it at — the parameters iceoryx2 verifies on each open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ChannelSizing {
    /// The fixed destination slot count plus the publisher's reserved slot.
    pub(crate) max_subscribers: usize,
    /// The deepest ring any subscriber on the channel may take.
    pub(crate) channel_service_creation_depth: usize,
}

/// The samples a channel subscriber may borrow at once on a service created
/// `channel_service_creation_depth` deep: a full mailbox, the most a reader may
/// hold past it, and the sample being admitted. Each link holds itself to its
/// own port's depth and quota inside this.
pub(crate) const fn channel_subscriber_max_borrowed_samples(
    channel_service_creation_depth: usize,
) -> usize {
    channel_service_creation_depth + MAX_INPUT_PORT_BORROW_QUOTA + 1
}

#[derive(Clone)]
pub struct Iceoryx2Node {
    inner: Arc<Mutex<Node<ipc_threadsafe::Service>>>,
}

impl Iceoryx2Node {
    /// Create a node, labelled `node_name`, in the engine-owned domain rooted at `domain_root`.
    pub fn new(domain_root: &std::path::Path, node_name: &str) -> Result<Self> {
        let node = create_iceoryx2_node_in_engine_owned_domain(domain_root, node_name)?;
        Ok(Self::wrapping(node))
    }

    /// Share an already-created raw node.
    pub(crate) fn wrapping(node: Node<ipc_threadsafe::Service>) -> Self {
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

    pub fn open_or_create_service(
        &self,
        service_name: &str,
        max_subscribers: usize,
        channel_service_creation_depth: usize,
    ) -> Result<Iceoryx2Service> {
        let service_name = iceoryx2_service_name_of(service_name)?;
        // Locked per attempt rather than across the whole recovery, so the
        // budget below never holds this node's other openers behind it.
        let open_or_create_once = || {
            let node = self.inner.lock();
            channel_data_service_builder_under_the_channel_policy(
                &node,
                &service_name,
                max_subscribers,
                channel_service_creation_depth,
            )
            .open_or_create()
        };

        let started_opening = std::time::Instant::now();
        let mut reopens = 0;
        let mut retry_interval = DEAD_HOLDER_RECLAIM_FIRST_RETRY_INTERVAL;
        loop {
            let failure = match open_or_create_once() {
                Ok(service) => {
                    if reopens > 0 {
                        tracing::info!(
                            service = service_name.as_str(),
                            channel_service_creation_depth,
                            recovered_in_ms = started_opening.elapsed().as_millis() as u64,
                            "a channel service was held shallower than this open asks for by a \
                             node whose process had just died; reopened it once iceoryx2 \
                             reclaimed the node"
                        );
                    }
                    return Ok(Iceoryx2Service {
                        inner: Arc::new(service),
                    });
                }
                Err(failure) => failure,
            };
            if !a_reopen_can_get_past(&failure) {
                return Err(channel_data_service_open_failure(
                    &service_name,
                    ChannelDataServiceAskedFor {
                        max_subscribers,
                        channel_service_creation_depth,
                    },
                    &failure,
                    ChannelDataServiceOpenRefusedFor::SomethingAReopenCannotChange,
                ));
            }
            if reopens == DEAD_HOLDER_RECLAIM_ATTEMPTS {
                return Err(channel_data_service_open_failure(
                    &service_name,
                    ChannelDataServiceAskedFor {
                        max_subscribers,
                        channel_service_creation_depth,
                    },
                    &failure,
                    ChannelDataServiceOpenRefusedFor::DepthOnEveryReopen,
                ));
            }
            reopens += 1;
            std::thread::sleep(retry_interval);
            retry_interval *= 2;
        }
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
            Ok(service) => Ok(Some(Iceoryx2Service {
                inner: Arc::new(service),
            })),
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
    /// slot, an entry per declared output port, the step-progress entry, the
    /// call breadcrumb, the overrun entry and a state entry per stateful
    /// function node the helper calls, all zero.
    pub fn create_helper_process_loss_count_board(
        &self,
        service_name: &str,
        output_port_names: Vec<String>,
        stateful_function_node_breadcrumb_numbers: Vec<u32>,
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
                creator = creator.add_with_default::<OutputPortBagCountBoardEntry>(key);
            }
            creator = creator.add_with_default::<HelperProcessStepProgressBoardEntry>(
                HelperProcessLossCountBoardKey::STEP_PROGRESS,
            );
            creator = creator
                .add_with_default::<HelperProcessFunctionNodeCallBreadcrumbBoardEntry>(
                    HelperProcessLossCountBoardKey::FUNCTION_NODE_CALL_BREADCRUMB,
                );
            creator = creator.add_with_default::<HelperProcessFunctionNodeOverrunBoardEntry>(
                HelperProcessLossCountBoardKey::FUNCTION_NODE_OVERRUN,
            );
            for &breadcrumb_number in &stateful_function_node_breadcrumb_numbers {
                creator = creator.add_with_default::<HelperProcessFunctionNodeStateBoardEntry>(
                    HelperProcessLossCountBoardKey::function_node_state(breadcrumb_number),
                );
            }
            creator.create().map_err(|refusal| {
                Error::Runtime(format!(
                    "could not create the loss-count board '{service_name}': {refusal:?}"
                ))
            })?
        };
        HelperProcessLossCountBoard::reading(
            service_name.to_string(),
            output_port_names,
            stateful_function_node_breadcrumb_numbers,
            service,
        )
    }

    /// Open the loss-count board a helper's parent created and named, as the
    /// one writer it admits.
    pub fn open_helper_process_loss_count_board_writer(
        &self,
        service_name: &str,
        output_port_names: &[String],
        stateful_function_node_breadcrumb_numbers: &[u32],
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
            stateful_function_node_breadcrumb_numbers,
            service,
        )
    }

    /// Create the clock board a scaled or stepped stream publishes `initial`
    /// on, as its one writer.
    pub fn create_stream_clock_board(
        &self,
        service_name: &str,
        initial: StreamClockBoardEntry,
    ) -> Result<StreamClockBoard> {
        let service = {
            let node = self.inner.lock();
            let iceoryx2_service_name = iceoryx2_service_name_of(service_name)?;
            node.service_builder(&iceoryx2_service_name)
                .blackboard_creator::<StreamClockBoardKey>()
                .max_readers(STREAM_CLOCK_BOARD_MAX_READERS)
                .max_nodes(STREAM_CLOCK_BOARD_MAX_NODES)
                .add::<StreamClockBoardEntry>(StreamClockBoardKey::STREAM_CLOCK, initial)
                .create()
                .map_err(|refusal| {
                    Error::Runtime(format!(
                        "could not create the stream clock board '{service_name}': {refusal:?}"
                    ))
                })?
        };
        let publish_event_service =
            self.with_stream_clock_board_publish_event_service(service_name, |builder| {
                builder.create().map_err(|refusal| {
                    Error::Runtime(format!(
                        "could not create the event each publish on the stream clock board \
                         '{service_name}' notifies: {refusal:?}"
                    ))
                })
            })??;
        StreamClockBoard::writing(service_name.to_string(), service, publish_event_service)
    }

    /// Listen for each publish on the clock board a helper's stream published.
    pub fn listen_for_each_stream_clock_board_publish(
        &self,
        board_service_name: &str,
    ) -> Result<StreamClockBoardPublishListener> {
        let publish_event_service =
            self.with_stream_clock_board_publish_event_service(board_service_name, |builder| {
                builder.open().map_err(|refusal| {
                    Error::Runtime(format!(
                        "could not open the event each publish on the stream clock board \
                         '{board_service_name}' notifies: {refusal:?}"
                    ))
                })
            })??;
        let listener =
            crate::iceoryx2::bind_an_iceoryx2_listener_outside_every_child_process_start(
                &publish_event_service,
            )
            .map_err(|refusal| {
                Error::Runtime(format!(
                    "the event each publish on the stream clock board '{board_service_name}' \
                 notifies refused this helper a listener: {refusal:?}"
                ))
            })?;
        Ok(StreamClockBoardPublishListener::listening(
            listener,
            publish_event_service,
        ))
    }

    /// Hand `create_or_open` the builder of the event each publish on the clock
    /// board named `board_service_name` notifies, with the limits every opener
    /// presents.
    fn with_stream_clock_board_publish_event_service<T>(
        &self,
        board_service_name: &str,
        create_or_open: impl FnOnce(
            iceoryx2::service::builder::event::Builder<ipc_threadsafe::Service>,
        ) -> T,
    ) -> Result<T> {
        let node = self.inner.lock();
        let iceoryx2_service_name = iceoryx2_service_name_of(
            &stream_clock_board_publish_event_service_name(board_service_name),
        )?;
        Ok(create_or_open(
            node.service_builder(&iceoryx2_service_name)
                .event()
                .max_notifiers(1)
                .max_listeners(STREAM_CLOCK_BOARD_MAX_READERS)
                .max_nodes(STREAM_CLOCK_BOARD_MAX_NODES),
        ))
    }

    /// Open the clock board a helper's stream published, as one of its readers.
    pub fn open_stream_clock_board_reader(
        &self,
        service_name: &str,
    ) -> Result<StreamClockBoardReader> {
        let service = {
            let node = self.inner.lock();
            let iceoryx2_service_name = iceoryx2_service_name_of(service_name)?;
            node.service_builder(&iceoryx2_service_name)
                .blackboard_opener::<StreamClockBoardKey>()
                .open()
                .map_err(|refusal| {
                    Error::Runtime(format!(
                        "could not open the stream clock board '{service_name}' its engine \
                         named: {refusal:?}"
                    ))
                })?
        };
        StreamClockBoardReader::reading(service_name.to_string(), service)
    }
}

/// `service_name` as a name iceoryx2 accepts, refused by name when it is not one.
fn iceoryx2_service_name_of(service_name: &str) -> Result<ServiceName> {
    service_name.try_into().map_err(|e| {
        Error::Configuration(format!("Invalid service name '{}': {:?}", service_name, e))
    })
}

/// The channel data service builder carrying the whole channel policy every
/// opener presents — the fixed slot counts, the creation depth, and the
/// borrow, no-history and safe-overflow limits iceoryx2 verifies on each open.
///
/// One spelling, because a test that re-states the chain to reproduce an open
/// failure stops reproducing it the moment a setting moves on one side only.
fn channel_data_service_builder_under_the_channel_policy(
    node: &Node<ipc_threadsafe::Service>,
    service_name: &ServiceName,
    max_subscribers: usize,
    channel_service_creation_depth: usize,
) -> PublishSubscribeServiceBuilder<
    [DataChannelPayloadCacheLine],
    DataChannelSampleUserHeader,
    ipc_threadsafe::Service,
> {
    channel_data_service_builder(node, service_name)
        .max_publishers(MAX_PUBLISHERS_PER_CHANNEL)
        .max_subscribers(max_subscribers)
        .max_nodes(max_subscribers * ICEORYX2_NODES_ADMITTED_PER_PORT_SLOT)
        .subscriber_max_buffer_size(channel_service_creation_depth)
        .subscriber_max_borrowed_samples(channel_subscriber_max_borrowed_samples(
            channel_service_creation_depth,
        ))
        .history_size(CHANNEL_HISTORY_SIZE)
        .enable_safe_overflow(true)
}

/// Whether a reopen can get past `failure`: only a refusal for depth, which a
/// holder that has just died stops causing once iceoryx2 reclaims it.
fn a_reopen_can_get_past(failure: &PublishSubscribeOpenOrCreateError) -> bool {
    matches!(
        failure,
        PublishSubscribeOpenOrCreateError::PublishSubscribeOpenError(
            PublishSubscribeOpenError::DoesNotSupportRequestedMinBufferSize
        )
    )
}

/// Why [`Iceoryx2Node::open_or_create_service`] gave up.
enum ChannelDataServiceOpenRefusedFor {
    /// Something a reopen cannot change.
    SomethingAReopenCannotChange,
    /// Depth, on every reopen the dead-holder budget allows.
    DepthOnEveryReopen,
}

/// What an open-or-create of a channel data service asked iceoryx2 for.
#[derive(Debug, Clone, Copy)]
struct ChannelDataServiceAskedFor {
    max_subscribers: usize,
    channel_service_creation_depth: usize,
}

/// The refusal for a failed open-or-create of a channel data service, naming
/// the service and what the open asked for, and saying whether the dead-holder
/// reopen budget was spent.
fn channel_data_service_open_failure(
    service_name: &ServiceName,
    asked_for: ChannelDataServiceAskedFor,
    failure: &PublishSubscribeOpenOrCreateError,
    refused_for: ChannelDataServiceOpenRefusedFor,
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
    let ChannelDataServiceAskedFor {
        max_subscribers,
        channel_service_creation_depth,
    } = asked_for;
    let refused = format!(
        "the channel data service '{}' could not be opened with a subscriber buffer of \
         {channel_service_creation_depth} samples and room for {max_subscribers} subscribers: \
         {failure:?}",
        service_name.as_str()
    );
    match refused_for {
        ChannelDataServiceOpenRefusedFor::SomethingAReopenCannotChange => Error::Runtime(refused),
        ChannelDataServiceOpenRefusedFor::DepthOnEveryReopen => Error::Runtime(format!(
            "{refused} (still refused after {DEAD_HOLDER_RECLAIM_ATTEMPTS} reopens, each of which \
             reclaims the service's dead holders, so a live holder is genuinely shallower than \
             this open asks for)"
        )),
    }
}

/// The refusal for a channel data service whose type pair is not this engine's.
fn channel_data_service_built_by_something_other_than_this_engine(
    service_name: &ServiceName,
    failure: &dyn std::fmt::Debug,
) -> Error {
    Error::Runtime(format!(
        "channel data service '{}' exists with a sample type other than payload cache lines \
         under the `DataChannelSampleUserHeader` user header, so it was built by \
         something other than this engine: {failure:?}",
        service_name.as_str()
    ))
}

/// Handle to an iceoryx2 channel data service.
#[derive(Clone)]
pub struct Iceoryx2Service {
    inner: Arc<ChannelDataServicePortFactory>,
}

impl Iceoryx2Service {
    /// Replace `subscriber_ids` with the id of every subscriber connected to
    /// this channel now, wherever it lives.
    pub fn collect_connected_subscriber_ids_into(
        &self,
        subscriber_ids: &mut Vec<UniqueSubscriberId>,
    ) {
        subscriber_ids.clear();
        self.inner.dynamic_config().list_subscribers(|subscriber| {
            subscriber_ids.push(subscriber.subscriber_id);
            CallbackProgression::Continue
        });
    }

    /// Whether a subscriber connected to this channel now is missing from
    /// `subscriber_ids`.
    pub fn has_a_connected_subscriber_outside(
        &self,
        subscriber_ids: &[UniqueSubscriberId],
    ) -> bool {
        let mut found_one_outside = false;
        self.inner.dynamic_config().list_subscribers(|subscriber| {
            if subscriber_ids.contains(&subscriber.subscriber_id) {
                CallbackProgression::Continue
            } else {
                found_one_outside = true;
                CallbackProgression::Stop
            }
        });
        found_one_outside
    }

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
    /// `expected_payload_bytes` primes the initial per-slot data segment — it is
    /// a HINT, never a
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
            .initial_max_slice_len(streamlib_ipc_types::data_channel_payload_cache_lines_for(
                expected_payload_bytes,
            ))
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
            .initial_max_slice_len(streamlib_ipc_types::data_channel_payload_cache_lines_for(
                primed_slice_bytes,
            ))
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

    /// Create the channel's reserved-slot subscriber with a ring `ring_depth`
    /// deep, telling the slot already held apart from every other transport
    /// failure. iceoryx2 counts subscriber slots rather than naming them, so the
    /// slot is refused only once every slot on the channel is held.
    pub fn create_reserved_slot_subscriber(
        &self,
        ring_depth: usize,
    ) -> std::result::Result<ChannelDataServiceSubscriber, ChannelReservedSlotSubscribeError> {
        create_the_reserved_slot_subscriber_of(&self.inner, ring_depth)
    }
}

/// Handle to an iceoryx2 Event service used for fd-multiplexed wakeups.
///
/// This wraps iceoryx2's `MessagingPattern::Event` — `Notifier::notify()` causes any
/// `Listener` on the same service to become readable on its underlying fd.
pub struct Iceoryx2NotifyService {
    inner: iceoryx2::service::port_factory::event::PortFactory<ipc_threadsafe::Service>,
}

impl Iceoryx2NotifyService {
    /// Create a notifier for this service.
    pub fn create_notifier(&self) -> Result<Notifier<ipc_threadsafe::Service>> {
        self.inner
            .notifier_builder()
            .create()
            .map_err(|e| Error::Runtime(format!("Failed to create notifier: {:?}", e)))
    }

    /// Create a listener for this service.
    pub fn create_listener(&self) -> Result<Listener<ipc_threadsafe::Service>> {
        crate::iceoryx2::bind_an_iceoryx2_listener_outside_every_child_process_start(&self.inner)
            .map_err(|e| Error::Runtime(format!("Failed to create listener: {:?}", e)))
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod domain_and_reclaim_tests;
