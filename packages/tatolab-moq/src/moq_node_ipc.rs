// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

//! How a stream's MoQ nodes reach one another: each runs in its own helper
//! process, and the gateway holds the one relay session, so the track, send and
//! receive nodes reach it over iceoryx2 shared memory in a domain of this
//! module's own, beside the engine's.
//!
//! Every service is named under `tatolab-moq/<runtime id>/<stream id>/`:
//! `gateway/<node>/control` carries the track declarations and receive requests
//! a gateway is sent, and `gateway/<node>/wake` wakes it; `track/<node>/description`
//! names a track's gateway to its send node, and `track/<node>/content` carries
//! what the send node writes; `receive/<node>/content` carries what a gateway
//! reads for a receive node, and `receive/<node>/wake` wakes that node.

use std::path::Path;
use std::sync::Arc;

use iceoryx2::config::Config;
use iceoryx2::port::listener::Listener;
use iceoryx2::port::notifier::Notifier;
use iceoryx2::port::publisher::Publisher;
use iceoryx2::port::subscriber::Subscriber;
use iceoryx2::prelude::*;
use iceoryx2::service::port_factory::event::PortFactory as EventServicePortFactory;
use iceoryx2::service::port_factory::publish_subscribe::PortFactory as PublishSubscribePortFactory;
use serde::{Deserialize, Serialize};

use crate::error::{MoqModuleError, Result};

/// The environment variable a runtime hands its helper processes the iceoryx2
/// domain root in.
pub const ICEORYX2_DOMAIN_ROOT_ENVIRONMENT_VARIABLE: &str = "STREAMLIB_ICEORYX2_DOMAIN_ROOT";

/// The most bytes a domain root and its prefix may take together: iceoryx2
/// names its Unix sockets `<root>/<prefix><entity file name>`, and a socket path
/// must fit `sun_path` with room for the longest name iceoryx2 appends.
pub const ICEORYX2_DOMAIN_ROOT_AND_PREFIX_BUDGET_BYTES: usize =
    if cfg!(target_os = "macos") { 58 } else { 63 };

/// Bytes a sample's slice starts at; a longer payload grows it.
const SLICE_BYTES_A_CONTENT_PUBLISHER_STARTS_AT: usize = 64 * 1024;

/// Samples a content subscriber holds before the oldest is overwritten.
const CONTENT_SUBSCRIBER_BUFFER_SAMPLES: usize = 64;

/// Samples the gateway's control subscriber holds.
const CONTROL_SUBSCRIBER_BUFFER_SAMPLES: usize = 256;

/// Publishers and notifiers a gateway's control and wake services admit: one
/// per track, send and receive node naming it, with headroom for a node a
/// crash left behind.
const NODES_A_GATEWAY_ADMITS: usize = 256;

/// The services of one stream's MoQ nodes, under one runtime run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoqStreamServiceNames {
    runtime_id: String,
    stream_id: String,
}

impl MoqStreamServiceNames {
    /// The services of the stream `stream_id` while the runtime `runtime_id` runs it.
    pub fn of_the_stream(runtime_id: &str, stream_id: &str) -> Self {
        Self {
            runtime_id: runtime_id.to_string(),
            stream_id: stream_id.to_string(),
        }
    }

    fn named(&self, role: &str, node: &str, service: &str) -> String {
        format!(
            "tatolab-moq/{}/{}/{role}/{node}/{service}",
            self.runtime_id, self.stream_id
        )
    }

    /// Where `gateway` is sent track declarations and receive requests.
    pub fn gateway_control(&self, gateway: &str) -> String {
        self.named("gateway", gateway, "control")
    }

    /// What wakes `gateway`.
    pub fn gateway_wake(&self, gateway: &str) -> String {
        self.named("gateway", gateway, "wake")
    }

    /// Where the track node `track_node` names its gateway.
    pub fn track_description(&self, track_node: &str) -> String {
        self.named("track", track_node, "description")
    }

    /// What the send node writes to the track node `track_node`.
    pub fn track_content(&self, track_node: &str) -> String {
        self.named("track", track_node, "content")
    }

    /// What a gateway reads for the receive node `receive_node`.
    pub fn receive_content(&self, receive_node: &str) -> String {
        self.named("receive", receive_node, "content")
    }

    /// What wakes the receive node `receive_node`.
    pub fn receive_wake(&self, receive_node: &str) -> String {
        self.named("receive", receive_node, "wake")
    }
}

/// What a sample on a track's content service carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum MoqTrackContentSampleKind {
    /// One message's payload, as written.
    Object = 0,
    /// The hang entry the stream facts of the port feeding the send node
    /// build, as JSON; empty when they build none.
    HangCatalogEntryBuiltFromUpstreamStreamFacts = 1,
}

/// The header of a sample a send node writes to its track's content service.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ZeroCopySend)]
#[type_name("MoqTrackContentSentToTheGatewayHeader")]
#[repr(C)]
pub struct MoqTrackContentSentToTheGatewayHeader {
    /// Counted from zero per send node; a gap is a sample overwritten unread.
    pub sequence_number: u64,
    /// Messages the send node's input reported lost before this one.
    pub lost_since_last_read: u64,
    /// The message's stamp on the writing stream's clock.
    pub timestamp_ns: i64,
    /// A [`MoqTrackContentSampleKind`].
    pub kind: u32,
    /// 1 when the message opens a group.
    pub starts_group: u32,
}

/// What a sample on a receive node's content service carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum MoqReceivedContentSampleKind {
    /// One object of the track, as the relay delivered it.
    Object = 0,
    /// The track's entry in its broadcast's catalog as `{"section", "entry"}`
    /// JSON; empty when the catalog lists none.
    CatalogEntry = 1,
    /// Consecutive groups the gateway gave up, counted in the header.
    GroupsGivenUp = 2,
}

/// The header of a sample a gateway writes to a receive node's content service.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ZeroCopySend)]
#[type_name("MoqTrackContentReceivedFromTheGatewayHeader")]
#[repr(C)]
pub struct MoqTrackContentReceivedFromTheGatewayHeader {
    /// Counted from zero per gateway read; a gap is a sample overwritten unread.
    pub sequence_number: u64,
    /// Groups given up, on a [`MoqReceivedContentSampleKind::GroupsGivenUp`] sample.
    pub groups_given_up: u64,
    /// The object's own timestamp in nanoseconds; zero when it carries none.
    pub object_timestamp_ns: u64,
    /// A [`MoqReceivedContentSampleKind`].
    pub kind: u32,
    /// 1 on the first object of a group.
    pub starts_group: u32,
}

/// What a gateway's control service carries, as JSON.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "message", rename_all = "snake_case", deny_unknown_fields)]
pub enum MoqGatewayControlMessage {
    /// A track node declares its track on the gateway, again every second
    /// while it runs.
    TrackDeclared {
        track_node: String,
        track: String,
        /// `{"section", "entry"}`, replacing the upstream port's entry.
        catalog_entry: Option<serde_json::Value>,
    },
    /// A receive node asks the gateway to read a track for it, again every
    /// second while it runs.
    TrackReceiveRequested {
        receive_node: String,
        broadcast: String,
        track: String,
        max_age_milliseconds: u64,
    },
}

/// What a track node's description service carries, as JSON.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MoqTrackDescription {
    /// The gateway node the track is declared on.
    pub gateway_node: String,
}

/// A publisher of `[u8]` samples under a user header `H`.
pub type MoqNodeSlicePublisher<H> = Publisher<ipc_threadsafe::Service, [u8], H>;

/// A subscriber of `[u8]` samples under a user header `H`.
pub type MoqNodeSliceSubscriber<H> = Subscriber<ipc_threadsafe::Service, [u8], H>;

/// One process's iceoryx2 node in this module's domain.
#[derive(Clone)]
pub struct MoqNodeIpc {
    node: Arc<Node<ipc_threadsafe::Service>>,
    pub service_names: MoqStreamServiceNames,
}

impl std::fmt::Debug for MoqNodeIpc {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MoqNodeIpc")
            .field("service_names", &self.service_names)
            .finish_non_exhaustive()
    }
}

impl MoqNodeIpc {
    /// Open this process's node in the domain under the root the runtime
    /// handed its helper, for the stream `service_names` names.
    pub fn open_under_the_runtimes_domain_root(
        service_names: MoqStreamServiceNames,
    ) -> Result<Self> {
        let domain_root =
            std::env::var_os(ICEORYX2_DOMAIN_ROOT_ENVIRONMENT_VARIABLE).ok_or_else(|| {
                MoqModuleError::Ipc {
                    what: format!(
                        "{ICEORYX2_DOMAIN_ROOT_ENVIRONMENT_VARIABLE} is not set, so this node runs \
                     outside a runtime's helper process"
                    ),
                }
            })?;
        Self::open_under_the_domain_root(Path::new(&domain_root), service_names)
    }

    /// Open this process's node in the domain under `domain_root`.
    pub fn open_under_the_domain_root(
        domain_root: &Path,
        service_names: MoqStreamServiceNames,
    ) -> Result<Self> {
        set_log_level(LogLevel::Error);
        let config = moq_module_iceoryx2_config(domain_root)?;
        let node = NodeBuilder::new()
            .config(&config)
            .create::<ipc_threadsafe::Service>()
            .map_err(|refusal| MoqModuleError::Ipc {
                what: format!("the module's iceoryx2 node did not open: {refusal:?}"),
            })?;
        Ok(Self {
            node: Arc::new(node),
            service_names,
        })
    }

    fn service_name(name: &str) -> Result<ServiceName> {
        ServiceName::new(name).map_err(|refusal| MoqModuleError::Ipc {
            what: format!("`{name}` is not an iceoryx2 service name: {refusal:?}"),
        })
    }

    fn slice_service<H: std::fmt::Debug + ZeroCopySend + Default>(
        &self,
        name: &str,
        max_publishers: usize,
        subscriber_buffer_samples: usize,
        history_samples: usize,
    ) -> Result<PublishSubscribePortFactory<ipc_threadsafe::Service, [u8], H>> {
        self.node
            .service_builder(&Self::service_name(name)?)
            .publish_subscribe::<[u8]>()
            .user_header::<H>()
            .max_publishers(max_publishers)
            .max_subscribers(1)
            .max_nodes(NODES_A_GATEWAY_ADMITS)
            .subscriber_max_buffer_size(subscriber_buffer_samples)
            .history_size(history_samples)
            .enable_safe_overflow(true)
            .open_or_create()
            .map_err(|refusal| MoqModuleError::Ipc {
                what: format!("the service `{name}` did not open: {refusal:?}"),
            })
    }

    fn event_service(
        &self,
        name: &str,
    ) -> Result<EventServicePortFactory<ipc_threadsafe::Service>> {
        self.node
            .service_builder(&Self::service_name(name)?)
            .event()
            .max_notifiers(NODES_A_GATEWAY_ADMITS)
            .max_listeners(1)
            .max_nodes(NODES_A_GATEWAY_ADMITS)
            .open_or_create()
            .map_err(|refusal| MoqModuleError::Ipc {
                what: format!("the event service `{name}` did not open: {refusal:?}"),
            })
    }

    fn content_service<H: std::fmt::Debug + ZeroCopySend + Default>(
        &self,
        name: &str,
    ) -> Result<PublishSubscribePortFactory<ipc_threadsafe::Service, [u8], H>> {
        self.slice_service::<H>(name, 1, CONTENT_SUBSCRIBER_BUFFER_SAMPLES, 0)
    }

    fn control_service(
        &self,
        gateway: &str,
    ) -> Result<PublishSubscribePortFactory<ipc_threadsafe::Service, [u8], ()>> {
        self.slice_service::<()>(
            &self.service_names.gateway_control(gateway),
            NODES_A_GATEWAY_ADMITS,
            CONTROL_SUBSCRIBER_BUFFER_SAMPLES,
            0,
        )
    }

    fn description_service(
        &self,
        track_node: &str,
    ) -> Result<PublishSubscribePortFactory<ipc_threadsafe::Service, [u8], ()>> {
        self.slice_service::<()>(&self.service_names.track_description(track_node), 1, 1, 1)
    }

    /// A publisher on the content service of `track_node`.
    pub fn track_content_publisher(
        &self,
        track_node: &str,
    ) -> Result<MoqNodeSlicePublisher<MoqTrackContentSentToTheGatewayHeader>> {
        slice_publisher(
            &self.content_service(&self.service_names.track_content(track_node))?,
            "track content",
        )
    }

    /// The gateway's subscriber on the content service of `track_node`.
    pub fn track_content_subscriber(
        &self,
        track_node: &str,
    ) -> Result<MoqNodeSliceSubscriber<MoqTrackContentSentToTheGatewayHeader>> {
        slice_subscriber(
            &self.content_service(&self.service_names.track_content(track_node))?,
            CONTENT_SUBSCRIBER_BUFFER_SAMPLES,
            "track content",
        )
    }

    /// The gateway's publisher on the content service of `receive_node`.
    pub fn receive_content_publisher(
        &self,
        receive_node: &str,
    ) -> Result<MoqNodeSlicePublisher<MoqTrackContentReceivedFromTheGatewayHeader>> {
        slice_publisher(
            &self.content_service(&self.service_names.receive_content(receive_node))?,
            "receive content",
        )
    }

    /// The receive node's subscriber on its own content service.
    pub fn receive_content_subscriber(
        &self,
        receive_node: &str,
    ) -> Result<MoqNodeSliceSubscriber<MoqTrackContentReceivedFromTheGatewayHeader>> {
        slice_subscriber(
            &self.content_service(&self.service_names.receive_content(receive_node))?,
            CONTENT_SUBSCRIBER_BUFFER_SAMPLES,
            "receive content",
        )
    }

    /// A publisher on `gateway`'s control service.
    pub fn gateway_control_publisher(&self, gateway: &str) -> Result<MoqNodeSlicePublisher<()>> {
        slice_publisher(&self.control_service(gateway)?, "gateway control")
    }

    /// `gateway`'s own subscriber on its control service.
    pub fn gateway_control_subscriber(&self, gateway: &str) -> Result<MoqNodeSliceSubscriber<()>> {
        slice_subscriber(
            &self.control_service(gateway)?,
            CONTROL_SUBSCRIBER_BUFFER_SAMPLES,
            "gateway control",
        )
    }

    /// The track node's publisher on its description service.
    pub fn track_description_publisher(
        &self,
        track_node: &str,
    ) -> Result<MoqNodeSlicePublisher<()>> {
        slice_publisher(&self.description_service(track_node)?, "track description")
    }

    /// The send node's subscriber on its track's description service.
    pub fn track_description_subscriber(
        &self,
        track_node: &str,
    ) -> Result<MoqNodeSliceSubscriber<()>> {
        slice_subscriber(
            &self.description_service(track_node)?,
            1,
            "track description",
        )
    }

    /// A notifier that wakes the node listening on `wake_service`.
    pub fn notifier(&self, wake_service: &str) -> Result<Notifier<ipc_threadsafe::Service>> {
        self.event_service(wake_service)?
            .notifier_builder()
            .create()
            .map_err(|refusal| MoqModuleError::Ipc {
                what: format!("no notifier opened on `{wake_service}`: {refusal:?}"),
            })
    }

    /// The one listener on `wake_service`.
    pub fn listener(&self, wake_service: &str) -> Result<Listener<ipc_threadsafe::Service>> {
        self.event_service(wake_service)?
            .listener_builder()
            .create()
            .map_err(|refusal| MoqModuleError::Ipc {
                what: format!("no listener opened on `{wake_service}`: {refusal:?}"),
            })
    }
}

fn slice_publisher<H: std::fmt::Debug + ZeroCopySend + Default>(
    service: &PublishSubscribePortFactory<ipc_threadsafe::Service, [u8], H>,
    what: &str,
) -> Result<MoqNodeSlicePublisher<H>> {
    service
        .publisher_builder()
        .initial_max_slice_len(SLICE_BYTES_A_CONTENT_PUBLISHER_STARTS_AT)
        .allocation_strategy(AllocationStrategy::PowerOfTwo)
        .create()
        .map_err(|refusal| MoqModuleError::Ipc {
            what: format!("no {what} publisher opened: {refusal:?}"),
        })
}

fn slice_subscriber<H: std::fmt::Debug + ZeroCopySend + Default>(
    service: &PublishSubscribePortFactory<ipc_threadsafe::Service, [u8], H>,
    buffer_samples: usize,
    what: &str,
) -> Result<MoqNodeSliceSubscriber<H>> {
    service
        .subscriber_builder()
        .buffer_size(buffer_samples)
        .create()
        .map_err(|refusal| MoqModuleError::Ipc {
            what: format!("no {what} subscriber opened: {refusal:?}"),
        })
}

/// Publish `payload` under `user_header` on `publisher`.
pub fn publish_slice<H: std::fmt::Debug + ZeroCopySend + Default>(
    publisher: &MoqNodeSlicePublisher<H>,
    user_header: H,
    payload: &[u8],
) -> Result<()> {
    let mut sample = publisher
        .loan_slice_uninit(payload.len())
        .map_err(|refusal| MoqModuleError::Ipc {
            what: format!("no {}-byte sample was loaned: {refusal:?}", payload.len()),
        })?;
    *sample.user_header_mut() = user_header;
    let sample = sample.write_from_slice(payload);
    sample.send().map_err(|refusal| MoqModuleError::Ipc {
        what: format!("a sample was not sent: {refusal:?}"),
    })?;
    Ok(())
}

/// The file prefix every iceoryx2 node of this module and OS user shares,
/// distinct from the engine's, so the two domains never share memory.
fn moq_module_iceoryx2_prefix_for_this_user() -> String {
    #[cfg(unix)]
    // SAFETY: `getuid` reads the calling process's real user id and cannot fail.
    let user_id = unsafe { libc::getuid() };
    #[cfg(not(unix))]
    let user_id = 0;
    format!("tm{user_id}_")
}

fn moq_module_iceoryx2_config(domain_root: &Path) -> Result<Config> {
    let prefix = moq_module_iceoryx2_prefix_for_this_user();
    let root_bytes = domain_root.as_os_str().as_encoded_bytes();
    if root_bytes.len() + prefix.len() > ICEORYX2_DOMAIN_ROOT_AND_PREFIX_BUDGET_BYTES {
        return Err(MoqModuleError::Ipc {
            what: format!(
                "the iceoryx2 domain root {} with prefix {prefix} is past the \
                 {ICEORYX2_DOMAIN_ROOT_AND_PREFIX_BUDGET_BYTES}-byte budget a Unix socket path \
                 leaves them; set XDG_RUNTIME_DIR to a shorter directory",
                domain_root.display()
            ),
        });
    }
    let mut config = Config::default();
    config.global.service.cleanup_dead_nodes_on_open = true;
    config.global.set_root_path(
        &iceoryx2::prelude::Path::new(root_bytes).map_err(|refusal| MoqModuleError::Ipc {
            what: format!(
                "the iceoryx2 domain root {} is not a path iceoryx2 accepts: {refusal:?}",
                domain_root.display()
            ),
        })?,
    );
    config.global.prefix =
        FileName::new(prefix.as_bytes()).map_err(|refusal| MoqModuleError::Ipc {
            what: format!("the iceoryx2 prefix {prefix} is not a file name: {refusal:?}"),
        })?;
    Ok(config)
}

#[cfg(test)]
mod layout_regression_tests {
    use super::*;
    use std::mem::{align_of, offset_of, size_of};

    #[test]
    fn the_header_a_send_node_writes_lays_out_as_its_readers_read_it() {
        assert_eq!(size_of::<MoqTrackContentSentToTheGatewayHeader>(), 32);
        assert_eq!(align_of::<MoqTrackContentSentToTheGatewayHeader>(), 8);
        assert_eq!(
            offset_of!(MoqTrackContentSentToTheGatewayHeader, sequence_number),
            0
        );
        assert_eq!(
            offset_of!(MoqTrackContentSentToTheGatewayHeader, lost_since_last_read),
            8
        );
        assert_eq!(
            offset_of!(MoqTrackContentSentToTheGatewayHeader, timestamp_ns),
            16
        );
        assert_eq!(offset_of!(MoqTrackContentSentToTheGatewayHeader, kind), 24);
        assert_eq!(
            offset_of!(MoqTrackContentSentToTheGatewayHeader, starts_group),
            28
        );
    }

    #[test]
    fn the_header_a_gateway_writes_lays_out_as_its_readers_read_it() {
        assert_eq!(size_of::<MoqTrackContentReceivedFromTheGatewayHeader>(), 32);
        assert_eq!(align_of::<MoqTrackContentReceivedFromTheGatewayHeader>(), 8);
        assert_eq!(
            offset_of!(MoqTrackContentReceivedFromTheGatewayHeader, sequence_number),
            0
        );
        assert_eq!(
            offset_of!(MoqTrackContentReceivedFromTheGatewayHeader, groups_given_up),
            8
        );
        assert_eq!(
            offset_of!(
                MoqTrackContentReceivedFromTheGatewayHeader,
                object_timestamp_ns
            ),
            16
        );
        assert_eq!(
            offset_of!(MoqTrackContentReceivedFromTheGatewayHeader, kind),
            24
        );
        assert_eq!(
            offset_of!(MoqTrackContentReceivedFromTheGatewayHeader, starts_group),
            28
        );
    }

    #[test]
    fn every_service_of_a_stream_is_named_under_its_run_and_stream() {
        let names = MoqStreamServiceNames::of_the_stream("run-1", "desk-1a2b");
        assert_eq!(
            names.gateway_control("cloudflare"),
            "tatolab-moq/run-1/desk-1a2b/gateway/cloudflare/control"
        );
        assert_eq!(
            names.track_content("camera-track"),
            "tatolab-moq/run-1/desk-1a2b/track/camera-track/content"
        );
        assert_eq!(
            names.receive_wake("remote"),
            "tatolab-moq/run-1/desk-1a2b/receive/remote/wake"
        );
    }

    #[test]
    fn a_control_message_reads_back_as_it_was_written() {
        let declared = MoqGatewayControlMessage::TrackDeclared {
            track_node: "camera-track".to_string(),
            track: "camera".to_string(),
            catalog_entry: None,
        };
        let written = serde_json::to_vec(&declared).unwrap();
        assert_eq!(
            serde_json::from_slice::<MoqGatewayControlMessage>(&written).unwrap(),
            declared
        );
        assert!(
            String::from_utf8(written)
                .unwrap()
                .contains("\"message\":\"track_declared\"")
        );
    }
}
