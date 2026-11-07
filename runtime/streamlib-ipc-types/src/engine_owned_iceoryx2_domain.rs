// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The iceoryx2 domain a runtime's channels live in, as every process that
//! opens one of them must configure it, and the channel data service's type
//! pair every opener presents.

use iceoryx2::node::Node;
use iceoryx2::port::publisher::Publisher;
use iceoryx2::port::subscriber::Subscriber;
use iceoryx2::prelude::*;
use iceoryx2::service::builder::publish_subscribe::Builder as PublishSubscribeServiceBuilder;
use streamlib_runtime_client_contract::streamlib_runtime_directory::current_process_uid;

use crate::{DataChannelPayloadCacheLine, DataChannelSampleUserHeader};

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

/// Why the configuration of an iceoryx2 domain, or a node in it, was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Iceoryx2DomainRefusal {
    /// The domain's root, prefix or a node's name is one iceoryx2 refuses.
    Configuration(String),
    /// iceoryx2 failed to create the node.
    NodeCreation(String),
}

impl std::fmt::Display for Iceoryx2DomainRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Configuration(reason) | Self::NodeCreation(reason) => formatter.write_str(reason),
        }
    }
}

impl std::error::Error for Iceoryx2DomainRefusal {}

/// The iceoryx2 configuration of the domain named by `domain_root` and
/// `prefix`, built from the library defaults, never from iceoryx2's lookup path.
///
/// iceoryx2 keeps file-backed state under the root but names its POSIX shared
/// memory from the prefix alone, so two domains are disjoint only when their
/// prefixes differ too.
pub fn iceoryx2_config_for_domain(
    domain_root: &std::path::Path,
    prefix: &str,
) -> Result<Config, Iceoryx2DomainRefusal> {
    let root_bytes = domain_root.as_os_str().as_encoded_bytes();
    let root_and_prefix_bytes = root_bytes.len() + prefix.len();
    if root_and_prefix_bytes > ICEORYX2_DOMAIN_ROOT_AND_PREFIX_BUDGET_BYTES {
        return Err(Iceoryx2DomainRefusal::Configuration(format!(
            "the iceoryx2 domain root {} with prefix {prefix} takes {root_and_prefix_bytes} bytes, \
             past the {ICEORYX2_DOMAIN_ROOT_AND_PREFIX_BUDGET_BYTES}-byte budget a Unix socket \
             path leaves them; set XDG_RUNTIME_DIR to a shorter directory",
            domain_root.display()
        )));
    }

    let mut config = Config::default();
    // A channel open gets past a shallow service whose only holder died by
    // leaning on this reclaim.
    config.global.service.cleanup_dead_nodes_on_open = true;
    config
        .global
        .set_root_path(&Path::new(root_bytes).map_err(|refusal| {
            Iceoryx2DomainRefusal::Configuration(format!(
                "the iceoryx2 domain root {} is not a path iceoryx2 accepts: {refusal:?}",
                domain_root.display()
            ))
        })?);
    config.global.prefix = FileName::new(prefix.as_bytes()).map_err(|refusal| {
        Iceoryx2DomainRefusal::Configuration(format!(
            "the iceoryx2 domain prefix {prefix} is not a file name iceoryx2 accepts: {refusal:?}"
        ))
    })?;
    Ok(config)
}

/// Create a raw iceoryx2 node, labelled `node_name`, in the domain named by
/// `domain_root` and `prefix`.
pub fn create_iceoryx2_node_in_domain(
    domain_root: &std::path::Path,
    prefix: &str,
    node_name: &str,
) -> Result<Node<ipc_threadsafe::Service>, Iceoryx2DomainRefusal> {
    let config = iceoryx2_config_for_domain(domain_root, prefix)?;
    let node_name = NodeName::new(node_name).map_err(|refusal| {
        Iceoryx2DomainRefusal::Configuration(format!(
            "'{node_name}' is not an iceoryx2 node name: {refusal:?}"
        ))
    })?;
    NodeBuilder::new()
        .config(&config)
        .name(&node_name)
        .signal_handling_mode(SignalHandlingMode::Disabled)
        .create::<ipc_threadsafe::Service>()
        .map_err(|failure| {
            Iceoryx2DomainRefusal::NodeCreation(format!(
                "failed to create iceoryx2 node '{}' in the domain rooted at {}: {failure:?}",
                node_name.as_str(),
                domain_root.display()
            ))
        })
}

/// The publisher of a channel data service: payload cache lines under the
/// channel sample user header.
pub type ChannelDataServicePublisher =
    Publisher<ipc_threadsafe::Service, [DataChannelPayloadCacheLine], DataChannelSampleUserHeader>;

/// A subscriber to a channel data service: payload cache lines under the
/// channel sample user header.
pub type ChannelDataServiceSubscriber =
    Subscriber<ipc_threadsafe::Service, [DataChannelPayloadCacheLine], DataChannelSampleUserHeader>;

/// One sample a channel subscriber received.
pub type ChannelDataServiceSample = iceoryx2::sample::Sample<
    ipc_threadsafe::Service,
    [DataChannelPayloadCacheLine],
    DataChannelSampleUserHeader,
>;

/// One sample a channel publisher loaned, not yet sent.
pub type ChannelDataServiceLoanedSample = iceoryx2::sample_mut::SampleMut<
    ipc_threadsafe::Service,
    [DataChannelPayloadCacheLine],
    DataChannelSampleUserHeader,
>;

/// The open port factory of a channel data service.
pub type ChannelDataServicePortFactory =
    iceoryx2::service::port_factory::publish_subscribe::PortFactory<
        ipc_threadsafe::Service,
        [DataChannelPayloadCacheLine],
        DataChannelSampleUserHeader,
    >;

/// The channel data service builder: payload cache lines under
/// [`DataChannelSampleUserHeader`], the one type pair every opener presents.
pub fn channel_data_service_builder(
    node: &Node<ipc_threadsafe::Service>,
    service_name: &ServiceName,
) -> PublishSubscribeServiceBuilder<
    [DataChannelPayloadCacheLine],
    DataChannelSampleUserHeader,
    ipc_threadsafe::Service,
> {
    node.service_builder(service_name)
        .publish_subscribe::<[DataChannelPayloadCacheLine]>()
        .user_header::<DataChannelSampleUserHeader>()
}

/// Why creating a channel's reserved-slot subscriber failed.
#[derive(Debug)]
pub enum ChannelReservedSlotSubscribeError {
    /// Every subscriber slot on the channel is held — iceoryx2 refused the
    /// create with `ExceedsMaxSupportedSubscribers`.
    ReservedSlotOccupied,
    /// Any other iceoryx2 subscriber-create failure, rendered for the log.
    Transport(String),
}

/// Create the reserved-slot subscriber of the channel `channel_service` opens,
/// with a ring `ring_depth` deep, telling the slot already held apart from
/// every other transport failure. iceoryx2 counts subscriber slots rather than
/// naming them, so the slot is refused only once every slot on the channel is
/// held.
pub fn create_the_reserved_slot_subscriber_of(
    channel_service: &ChannelDataServicePortFactory,
    ring_depth: usize,
) -> Result<ChannelDataServiceSubscriber, ChannelReservedSlotSubscribeError> {
    use iceoryx2::port::subscriber::SubscriberCreateError;
    channel_service
        .subscriber_builder()
        .buffer_size(ring_depth)
        .create()
        .map_err(|failure| match failure {
            SubscriberCreateError::ExceedsMaxSupportedSubscribers => {
                ChannelReservedSlotSubscribeError::ReservedSlotOccupied
            }
            other => ChannelReservedSlotSubscribeError::Transport(format!("{other:?}")),
        })
}
