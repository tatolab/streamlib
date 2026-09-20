// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! JSON Schema output types for API documentation.
//!
//! These structs mirror the serialization output of the runtime types and are used
//! for generating JSON Schema files. They implement both `Serialize` and `JsonSchema`
//! to ensure schemas stay in sync with actual serialization.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::core::graph::{GraphEdgeWithComponents, GraphNodeWithComponents};
use crate::core::runtime::LoadedCapabilityExtension;

/// The processor-identity wire type. Defined in the engine-free
/// `streamlib-processor-schema` crate so the MoQ catalog and the authoring
/// chain share one definition; re-exported here so the
/// `streamlib::sdk::json_schema` facade the API server consumes resolves it.
/// The `utoipa` feature the engine enables gives it the `utoipa::ToSchema`
/// derive the aggregate response types below require.
pub use streamlib_processor_schema::ProcessorClassImportPath;

// =============================================================================
// Graph Response Schema (/api/graph)
// =============================================================================

/// Response from the `/api/graph` endpoint.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
pub struct GraphResponse {
    /// All processor nodes in the graph.
    pub nodes: Vec<ProcessorNodeOutput>,
    /// All links (connections) between processors.
    pub links: Vec<LinkOutput>,
    /// The capabilities the extension wheels installed beside this engine
    /// registered at startup. Always present; empty when none loaded.
    pub extensions: Vec<LoadedCapabilityExtensionOutput>,
    /// Where this runtime sits on the runtime mesh, and who else it sees
    /// there. Always present: every runtime is on a mesh.
    pub mesh: RuntimeMeshOutput,
}

/// This runtime's place on the runtime mesh.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
pub struct RuntimeMeshOutput {
    /// The mesh everything this runtime announces lives under.
    pub mesh_name: String,
    /// The name this runtime is addressed by on that mesh.
    pub runtime_name: String,
    /// Whether this runtime's Zenoh session opened.
    pub session: RuntimeMeshSessionOutput,
    /// Why the session did not open. Present only when it did not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_only_reason: Option<String>,
    /// The other runtimes this one currently sees, sorted by name. A session
    /// that is `open` with no peers is isolated rather than local-only.
    pub peers: Vec<RuntimeMeshPeerOutput>,
    /// The output ports of this runtime that other runtimes are reading over
    /// the mesh. Always present, and empty until one is — a sending runtime
    /// does no network work for a port until a remote link reads it.
    pub egress_ports: Vec<MeshEgressPortOutput>,
    /// Every link this runtime has asked another runtime to apply, and that
    /// runtime has not. Always present, and empty when there are none.
    ///
    /// A request leaves this list when the runtime that owns the input applies
    /// it — the link is then that runtime's to render. One it refused stays,
    /// because asking never waits and a refusal has nowhere else to land.
    pub link_requests_awaiting_runtime: Vec<LinkRequestAwaitingARuntimeOutput>,
}

/// One link this runtime has asked another runtime for, still unapplied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
pub struct LinkRequestAwaitingARuntimeOutput {
    /// The id this runtime minted for the request. `disconnect` takes it to
    /// cancel the request.
    pub link_request_id: String,
    /// What the request asks for.
    pub operation: LinkRequestOperationOutput,
    /// The runtime being asked — the one that owns the input.
    pub input_runtime_name: String,
    /// The port the link would carry from, as `<runtime>/<display name>/<port>`.
    /// Absent on a request asking for a link to go.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// The port the link would carry into, spelled the same way. Absent on a
    /// request asking for a link to go.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination: Option<String>,
    /// The link a request asking for one to go names. Absent otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link_id: Option<String>,
    /// How far the request has got.
    pub state: LinkRequestStateOutput,
    /// What that state is about, in terms the author who asked can act on.
    pub reason: String,
}

/// What a link request asks the runtime that owns the input to do.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum LinkRequestOperationOutput {
    /// Apply a link into one of that runtime's inputs.
    Connect,
    /// Remove a link that runtime holds.
    Disconnect,
}

impl From<crate::core::runtime::mesh::WhichOperationALinkRequestNames>
    for LinkRequestOperationOutput
{
    fn from(operation: crate::core::runtime::mesh::WhichOperationALinkRequestNames) -> Self {
        match operation {
            crate::core::runtime::mesh::WhichOperationALinkRequestNames::Connect => Self::Connect,
            crate::core::runtime::mesh::WhichOperationALinkRequestNames::Disconnect => {
                Self::Disconnect
            }
        }
    }
}

/// How far a link request this runtime made has got.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum LinkRequestStateOutput {
    /// The runtime it names is not on the mesh, so it has not been sent. Not
    /// final: it is sent the moment that runtime appears.
    AwaitingRuntime,
    /// It was sent and nothing came back. Not final either — a request sent at
    /// `Drop`, or its reply, can go missing with nothing said — so it is sent
    /// again on a backoff.
    Unanswered,
    /// The runtime it names refused it. Final: a resend would be refused in
    /// the same words. `reason` is that runtime's own.
    Refused,
}

/// One output port of this runtime that the mesh is sending, and to whom.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
pub struct MeshEgressPortOutput {
    /// The display name of the processor that owns the port — the middle chunk
    /// of the port's mesh address.
    pub processor_display_name: String,
    /// The port's own name on that processor.
    pub port_name: String,
    /// Every runtime currently reading it, sorted by name.
    pub reader_runtime_names: Vec<String>,
}

/// Whether a runtime reached its mesh at all.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeMeshSessionOutput {
    /// The session opened. It may still reach nobody.
    Open,
    /// The session could not open, so this runtime reaches no other.
    LocalOnly,
}

/// One other runtime on the mesh.
///
/// Only the name comes off the liveliness token; the rest is what the peer
/// answered when asked, so each of the four is absent until it does.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
pub struct RuntimeMeshPeerOutput {
    /// The name the peer is addressed by on the mesh.
    pub runtime_name: String,
    /// The peer's per-run id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_id: Option<String>,
    /// What the peer's host calls itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_name: Option<String>,
    /// The engine version the peer runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine_version: Option<String>,
    /// Where the peer's control plane can be reached, if it hosts one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control_plane_urls: Option<Vec<String>>,
}

/// A capability a loaded extension wheel registered.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
pub struct LoadedCapabilityExtensionOutput {
    /// The capability's name, unique across every loaded distribution.
    pub name: String,
    /// The version the registering distribution declared for it.
    pub version: String,
    /// The distribution whose entry point registered it.
    pub distribution: String,
}

impl From<LoadedCapabilityExtension> for LoadedCapabilityExtensionOutput {
    fn from(registered: LoadedCapabilityExtension) -> Self {
        Self {
            name: registered.name,
            version: registered.version,
            distribution: registered.distribution,
        }
    }
}

/// A processor node in the graph.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
pub struct ProcessorNodeOutput {
    /// Unique identifier for this processor instance.
    pub id: String,
    /// The import path of the class this processor is — a plain string.
    #[serde(rename = "type")]
    pub processor_type: ProcessorClassImportPath,
    /// Display name for UI. May differ from type for hosted processors.
    pub display_name: String,
    /// Processor configuration as JSON.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<serde_json::Value>,
    /// Checksum of config for change detection.
    #[serde(default)]
    pub config_checksum: u64,
    /// Input and output ports.
    pub ports: ProcessorNodePortsOutput,
    /// Runtime components (dynamic, varies based on processor state).
    pub components: serde_json::Map<String, serde_json::Value>,
}

/// Container for processor input and output ports.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
pub struct ProcessorNodePortsOutput {
    /// Input ports that receive data.
    pub inputs: Vec<PortInfoOutput>,
    /// Output ports that send data.
    pub outputs: Vec<PortInfoOutput>,
}

/// Metadata about a port.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
pub struct PortInfoOutput {
    /// Port name (e.g., "video_in", "audio_out").
    pub name: String,
    /// Human-readable description declared alongside the port.
    #[serde(default)]
    pub description: String,
    /// Kind of port: data, event, or control.
    #[serde(default)]
    pub port_kind: PortKindOutput,
    /// Delivery profile declared by this input port — `"newest"` or
    /// `"ordered"`; `None` on an output port.
    pub delivery_profile: Option<String>,
    /// Window contract declared by this audio input port. Absent from the
    /// rendering on a port that declares none — the contract is opt-in, and a
    /// port without one renders exactly what it always did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_window: Option<crate::core::descriptors::AudioWindowContract>,
}

/// The kind of port - determines how data flows.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum PortKindOutput {
    #[default]
    Data,
    Event,
    Control,
}

/// A link (connection) between two processor ports.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
pub struct LinkOutput {
    /// Unique identifier for this link.
    pub id: String,
    /// Source endpoint (output port).
    pub source: LinkPortRefOutput,
    /// Target endpoint (input port).
    pub target: LinkPortRefOutput,
    /// Ring buffer capacity for the channel.
    #[serde(default)]
    pub capacity: usize,
    /// Current state of the link.
    #[serde(default)]
    pub state: LinkStateOutput,
    /// The runtime that asked for this link — this node's own name for a link
    /// its own app or control plane wired, and the asking runtime's name for
    /// one another runtime pushed here or wired on its behalf.
    ///
    /// Always present, so a reader never has to tell "nobody asked" from "this
    /// engine predates the key".
    pub created_by_runtime_name: String,
    /// Why the link is in the `error` state, in the words of whoever refused
    /// it — today always the helper process that could not open its port.
    ///
    /// Absent in every other state, so a reader that finds it knows the link
    /// carries nothing and will not start to: disconnect it and wire again.
    /// `state` stays a plain string so a check against `"wired"` is unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_reason: Option<String>,
    /// What a link in the `awaiting_remote` state is waiting on — the source
    /// runtime, which is not on the mesh, or the port, which the runtime that
    /// is here does not offer.
    ///
    /// Absent in every other state. Unlike `error_reason` this is not final:
    /// the link wires itself the moment what it names turns up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub awaiting_remote_reason: Option<String>,
    /// Which machine's monotonic clock the stamps on this link's bags were
    /// taken on, as the canonical lowercase UUID text of that machine's
    /// boot-session id.
    ///
    /// Every stamp is a machine's monotonic clock, whose epoch is that
    /// machine's own boot, so two stamps from two of these are readings of two
    /// unrelated clocks and subtracting them means nothing. A link inside this
    /// node always names this machine; one from another runtime names whatever
    /// machine the mesh is carrying it from, and is absent until its first bag
    /// lands or while its source runtime is away.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stamp_clock_identity: Option<String>,
    /// Runtime components (dynamic, varies based on link state).
    pub components: serde_json::Map<String, serde_json::Value>,
}

/// Reference to a port on a processor — on this node, or on another runtime
/// over the mesh.
///
/// One of two shapes, told apart by their keys and not by a tag: a port on this
/// node carries `processor_id`, and a port on another runtime carries the three
/// parts of its mesh address. A reader checking `processor_id` therefore finds
/// nothing on a remote end rather than a processor id this node does not have.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[serde(untagged)]
pub enum LinkPortRefOutput {
    /// A port on a processor this node holds.
    OnThisRuntime {
        /// Processor instance ID.
        processor_id: String,
        /// Port name on that processor.
        port_name: String,
    },
    /// A port on a processor another runtime holds, addressed
    /// `<runtime name>/<display name>/<port>`.
    OnAnotherRuntime {
        /// The name the owning runtime is addressed by on the mesh.
        runtime_name: String,
        /// The display name of the processor that owns the port, there.
        processor_display_name: String,
        /// The port's own name on that processor.
        port_name: String,
    },
}

impl LinkPortRefOutput {
    /// The processor this port belongs to when this node holds it, and `None`
    /// for a port on another runtime.
    pub fn processor_id_on_this_runtime(&self) -> Option<&str> {
        match self {
            Self::OnThisRuntime { processor_id, .. } => Some(processor_id),
            Self::OnAnotherRuntime { .. } => None,
        }
    }

    /// The port's own name, wherever the port lives.
    pub fn port_name(&self) -> &str {
        match self {
            Self::OnThisRuntime { port_name, .. } | Self::OnAnotherRuntime { port_name, .. } => {
                port_name
            }
        }
    }
}

/// State of a link in the graph.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum LinkStateOutput {
    /// Link exists in graph but not yet wired.
    #[default]
    Pending,
    /// The link's source is a port on another runtime and nothing carries yet.
    /// `awaiting_remote_reason` says what is missing.
    AwaitingRemote,
    /// Link is actively wired with a ring buffer channel.
    Wired,
    /// Link is being disconnected.
    Disconnecting,
    /// Link was disconnected.
    Disconnected,
    /// Link is in error state.
    Error,
}

// =============================================================================
// Registry Response Schema (/api/registry)
// =============================================================================

/// Response from the `/api/registry` endpoint.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
pub struct RegistryResponse {
    /// Available processor types with their descriptors.
    pub processors: Vec<ProcessorDescriptorOutput>,
}

/// Runtime environment for a processor.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum ProcessorRuntimeOutput {
    #[default]
    Rust,
    Python,
}

/// Descriptor for a processor type.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
pub struct ProcessorDescriptorOutput {
    /// The import path of the class this processor is.
    ///
    /// The same value a graph node carries, but under its own key: a node
    /// renames the field to `type`, because there it is the node's type; here
    /// it is what the registry is keyed on.
    pub processor_class_import_path: ProcessorClassImportPath,
    /// Human-readable description.
    pub description: String,
    /// Repository URL.
    pub repository: String,
    /// Runtime environment.
    #[serde(default)]
    pub runtime: ProcessorRuntimeOutput,
    /// Entrypoint for non-Rust runtimes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entrypoint: Option<String>,
    /// The config type's JSON Schema, as JSON Schema draft 2020-12 — what an
    /// agent reads to learn which keys this processor's config takes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_schema: Option<serde_json::Value>,
    /// Input port descriptors.
    pub inputs: Vec<PortDescriptorOutput>,
    /// Output port descriptors.
    pub outputs: Vec<PortDescriptorOutput>,
    /// Code examples in different languages.
    pub examples: CodeExamplesOutput,
}

/// Descriptor for a processor port.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
pub struct PortDescriptorOutput {
    /// Port name.
    pub name: String,
    /// Human-readable description.
    pub description: String,
    /// Whether the port is required.
    pub required: bool,
    /// Delivery profile declared by this input port — `"newest"` or
    /// `"ordered"`; `None` on an output port.
    pub delivery_profile: Option<String>,
    /// Window contract declared by this audio input port. Absent from the
    /// rendering on a port that declares none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_window: Option<crate::core::descriptors::AudioWindowContract>,
}

/// Code examples for a processor in different languages.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
pub struct CodeExamplesOutput {
    /// Rust code example.
    pub rust: String,
    /// Python code example.
    pub python: String,
    /// TypeScript code example.
    pub typescript: String,
}

// =============================================================================
// Conversion from Runtime Types
// =============================================================================

impl From<&crate::core::graph::ProcessorNode> for ProcessorNodeOutput {
    fn from(node: &crate::core::graph::ProcessorNode) -> Self {
        Self {
            id: node.id.to_string(),
            processor_type: node.processor_type.clone(),
            display_name: node.display_name.clone(),
            config: node.config.clone(),
            config_checksum: node.config_checksum,
            ports: ProcessorNodePortsOutput::rendered_for_a_node_over_its_settled_contracts(node),
            components: node.serialize_components(),
        }
    }
}

impl ProcessorNodePortsOutput {
    /// This node's ports, with any `match_device` contract its own device
    /// stream has settled rendered as the five values it settled to.
    ///
    /// A port declaring the sentinel renders it until its processor's `setup()`
    /// opens a device, and the resolved values from then on: machine-dependent
    /// because the device format is, which is truer than a static lie.
    ///
    /// Takes the node rather than its ports, and is the only way they are
    /// rendered: the settled values live beside the ports rather than in them,
    /// so a renderer handed the ports alone could only ever produce the
    /// sentinel — and would look like the right call to reach for.
    fn rendered_for_a_node_over_its_settled_contracts(
        node: &crate::core::graph::ProcessorNode,
    ) -> Self {
        let settled = node
            .get::<crate::core::graph::DeviceMatchedAudioWindowContractsComponent>()
            .map(|component| &*component.0);
        Self {
            inputs: node
                .ports
                .inputs
                .iter()
                .map(|port| PortInfoOutput::rendered_over_any_settled_contract(port, settled))
                .collect(),
            outputs: node
                .ports
                .outputs
                .iter()
                .map(PortInfoOutput::from)
                .collect(),
        }
    }
}

impl PortInfoOutput {
    /// One input port, with a `match_device` declaration replaced by whatever
    /// its own device stream settled it to.
    fn rendered_over_any_settled_contract(
        port: &crate::core::graph::PortInfo,
        settled: Option<&crate::iceoryx2::DeviceMatchedAudioWindowContractsByInputPort>,
    ) -> Self {
        let mut rendered = PortInfoOutput::from(port);
        if !matches!(
            rendered.audio_window,
            Some(crate::core::descriptors::AudioWindowContract::MatchDevice {})
        ) {
            return rendered;
        }
        if let Some(values) =
            settled.and_then(|contracts| contracts.settled_declaration_for_input_port(&port.name))
        {
            rendered.audio_window = Some(crate::core::descriptors::AudioWindowContract::Device(
                values,
            ));
        }
        rendered
    }
}

impl From<&crate::core::graph::PortInfo> for PortInfoOutput {
    fn from(port: &crate::core::graph::PortInfo) -> Self {
        Self {
            name: port.name.clone(),
            description: port.description.clone(),
            port_kind: PortKindOutput::from(port.port_kind),
            delivery_profile: port.delivery_profile.clone(),
            audio_window: port.audio_window.clone(),
        }
    }
}

impl From<crate::core::graph::PortKind> for PortKindOutput {
    fn from(kind: crate::core::graph::PortKind) -> Self {
        match kind {
            crate::core::graph::PortKind::Data => PortKindOutput::Data,
            crate::core::graph::PortKind::Event => PortKindOutput::Event,
            crate::core::graph::PortKind::Control => PortKindOutput::Control,
        }
    }
}

impl LinkOutput {
    /// Render `link` as the runtime named `this_runtimes_name` sees it.
    ///
    /// The renderer's own name is what a link carries no room for: a link this
    /// runtime wired was asked for here, and only a link another runtime
    /// requested carries a name of its own to render instead.
    pub fn of_a_link_on_the_runtime_named(
        link: &crate::core::graph::Link,
        this_runtimes_name: &str,
    ) -> Self {
        let rendered = RenderedLinkState::of(link);
        let mut components = link.serialize_components();
        // `LinkStateComponent` renders under `components.state` too, and for a
        // link waiting on a helper it holds the `Pending` the op stamped. Left
        // alone it would contradict the top-level state a reader was told to
        // check — the same disagreement the top-level state reads the component
        // to avoid, in the other direction.
        if let Some(rendered_state) = components.get_mut("state") {
            *rendered_state = serde_json::json!(format!("{:?}", rendered.state));
        }
        Self {
            id: link.id.to_string(),
            source: LinkPortRefOutput::from(&link.source),
            target: LinkPortRefOutput::from(&link.target),
            capacity: link.capacity.get(),
            state: rendered.state,
            error_reason: rendered.error_reason,
            awaiting_remote_reason: rendered.awaiting_remote_reason,
            created_by_runtime_name: link
                .get::<crate::core::graph::TheRequestThatAppliedThisLinkComponent>()
                .map(|applied| applied.requester_runtime_name.clone())
                .unwrap_or_else(|| this_runtimes_name.to_string()),
            stamp_clock_identity: the_machine_a_links_stamps_are_taken_on(link),
            components,
        }
    }
}

/// Which machine's clock a link's stamps are taken on, as `graph` renders it.
///
/// Three ways to name no machine, and each is a link a reader must not compare
/// a stamp against:
///
/// - **A link on its way out.** It carries nothing, and the cell it still holds
///   a clone of may name the machine another link is carrying from — the same
///   reason the state beside it reads `disconnecting` rather than `wired`.
/// - **A link whose source is on another runtime and is not wired yet.** The
///   cell is attached when the link is wired, and such a link is in the graph
///   from the moment `connect` applies it. Keyed on the source rather than on
///   the cell being there, or it would claim this machine for its whole
///   `awaiting_remote` life.
/// - **A machine that named no clock of its own**, on either arm. Every such
///   machine renders the same nil id, so rendering it would hand a reader a
///   string two unrelated clocks match on.
fn the_machine_a_links_stamps_are_taken_on(link: &crate::core::graph::Link) -> Option<String> {
    let stamped = link
        .get::<crate::core::graph::LinkStateComponent>()
        .map(|state| state.0)
        .unwrap_or(link.state);
    if matches!(
        stamped,
        crate::core::graph::LinkState::Disconnecting | crate::core::graph::LinkState::Disconnected
    ) {
        return None;
    }
    if link.source.mesh_port_address().is_none() {
        return crate::iceoryx2::WhatIsKnownOfAnInboundLinksStampClock::from(Some(
            crate::core::runtime::mesh::MachineClockIdentity::of_this_machine(),
        ))
        .the_machine_if_it_is_known()
        .map(|machine| machine.to_string());
    }
    link.get::<crate::core::graph::TheMachineClockALinksStampsAreTakenOnComponent>()
        .and_then(|machine_clock| machine_clock.as_uuid_text())
}

/// What a link reports as its state, and why where that is `error` or
/// `awaiting_remote`.
struct RenderedLinkState {
    state: LinkStateOutput,
    error_reason: Option<String>,
    awaiting_remote_reason: Option<String>,
}

impl RenderedLinkState {
    /// Read one link's state off whichever cell holds the live answer.
    ///
    /// Wiring records its outcome on the component; the field is the state the
    /// link was created in. A link handed to an out-of-process end sits at
    /// `Pending` there until that end answers — the answer lands on a cell its
    /// bridge's reader thread fills, which holds no graph lock — so a link still
    /// carrying those cells reads its state off them instead. A link whose
    /// source is on another runtime reads its own cell the same way, which the
    /// mesh writes as the source runtime appears, offers the port, and leaves.
    /// Once the disconnect path has moved the component past `Pending`, that
    /// stamp is the answer: a link on its way out is not `wired` because a
    /// helper once said so.
    fn of(link: &crate::core::graph::Link) -> Self {
        let stamped = link
            .get::<crate::core::graph::LinkStateComponent>()
            .map(|state| state.0)
            .unwrap_or(link.state);
        if let Some(resolution) = link.get::<crate::core::graph::RemoteLinkResolutionComponent>() {
            return Self::of_a_link_from_another_runtime(stamped, resolution);
        }
        if stamped != crate::core::graph::LinkState::Pending {
            return Self::plain(LinkStateOutput::from(stamped));
        }
        let Some(replies) = link.get::<crate::core::graph::OutOfProcessLinkWireRepliesComponent>()
        else {
            return Self::plain(LinkStateOutput::from(stamped));
        };
        match replies.what_its_out_of_process_ends_have_answered() {
            crate::core::graph::OutOfProcessLinkWireProgress::AnEndHasNotAnsweredYet => {
                Self::plain(LinkStateOutput::Pending)
            }
            crate::core::graph::OutOfProcessLinkWireProgress::EveryEndOpenedItsPort => {
                Self::plain(LinkStateOutput::Wired)
            }
            crate::core::graph::OutOfProcessLinkWireProgress::AnEndRefused { reason } => Self {
                state: LinkStateOutput::Error,
                error_reason: Some(reason),
                awaiting_remote_reason: None,
            },
        }
    }

    /// A link whose source is on another runtime, read off the cell the mesh
    /// writes — except once the disconnect path has stamped it, which outranks
    /// a mesh answer the same way it outranks a helper's.
    fn of_a_link_from_another_runtime(
        stamped: crate::core::graph::LinkState,
        resolution: &crate::core::graph::RemoteLinkResolutionComponent,
    ) -> Self {
        if matches!(
            stamped,
            crate::core::graph::LinkState::Disconnecting
                | crate::core::graph::LinkState::Disconnected
        ) {
            return Self::plain(LinkStateOutput::from(stamped));
        }
        match resolution.how_far_it_has_got() {
            crate::core::graph::RemoteLinkResolution::AwaitingRemote { reason } => Self {
                state: LinkStateOutput::AwaitingRemote,
                error_reason: None,
                awaiting_remote_reason: Some(reason),
            },
            crate::core::graph::RemoteLinkResolution::Wired => Self::plain(LinkStateOutput::Wired),
            crate::core::graph::RemoteLinkResolution::Refused { reason } => Self {
                state: LinkStateOutput::Error,
                error_reason: Some(reason),
                awaiting_remote_reason: None,
            },
        }
    }

    fn plain(state: LinkStateOutput) -> Self {
        Self {
            state,
            error_reason: None,
            awaiting_remote_reason: None,
        }
    }
}

impl From<&crate::core::graph::OutputLinkPortRef> for LinkPortRefOutput {
    fn from(port_ref: &crate::core::graph::OutputLinkPortRef) -> Self {
        match port_ref {
            crate::core::graph::OutputLinkPortRef::OnThisRuntime {
                processor_id,
                port_name,
            } => Self::OnThisRuntime {
                processor_id: processor_id.to_string(),
                port_name: port_name.clone(),
            },
            crate::core::graph::OutputLinkPortRef::OnAnotherRuntime(address) => {
                Self::OnAnotherRuntime {
                    runtime_name: address.runtime_name().to_string(),
                    processor_display_name: address.processor_display_name().to_string(),
                    port_name: address.port_name().to_string(),
                }
            }
        }
    }
}

impl From<&crate::core::graph::InputLinkPortRef> for LinkPortRefOutput {
    fn from(port_ref: &crate::core::graph::InputLinkPortRef) -> Self {
        match port_ref {
            crate::core::graph::InputLinkPortRef::OnThisRuntime {
                processor_id,
                port_name,
            } => Self::OnThisRuntime {
                processor_id: processor_id.to_string(),
                port_name: port_name.clone(),
            },
            crate::core::graph::InputLinkPortRef::OnAnotherRuntime(address) => {
                Self::OnAnotherRuntime {
                    runtime_name: address.runtime_name().to_string(),
                    processor_display_name: address.processor_display_name().to_string(),
                    port_name: address.port_name().to_string(),
                }
            }
        }
    }
}

impl From<crate::core::graph::LinkState> for LinkStateOutput {
    fn from(state: crate::core::graph::LinkState) -> Self {
        match state {
            crate::core::graph::LinkState::Pending => LinkStateOutput::Pending,
            crate::core::graph::LinkState::AwaitingRemote => LinkStateOutput::AwaitingRemote,
            crate::core::graph::LinkState::Wired => LinkStateOutput::Wired,
            crate::core::graph::LinkState::Disconnecting => LinkStateOutput::Disconnecting,
            crate::core::graph::LinkState::Disconnected => LinkStateOutput::Disconnected,
            crate::core::graph::LinkState::Error => LinkStateOutput::Error,
        }
    }
}

impl From<&crate::core::ProcessorDescriptor> for ProcessorDescriptorOutput {
    fn from(desc: &crate::core::ProcessorDescriptor) -> Self {
        Self {
            processor_class_import_path: desc.processor_class_import_path.clone(),
            description: desc.description.clone(),
            repository: desc.repository.clone(),
            runtime: ProcessorRuntimeOutput::from(&desc.runtime),
            entrypoint: desc.entrypoint.clone(),
            config_schema: desc.config_schema.clone(),
            inputs: desc.inputs.iter().map(PortDescriptorOutput::from).collect(),
            outputs: desc
                .outputs
                .iter()
                .map(PortDescriptorOutput::from)
                .collect(),
            examples: CodeExamplesOutput::from(&desc.examples),
        }
    }
}

impl From<&crate::core::ProcessorRuntime> for ProcessorRuntimeOutput {
    fn from(runtime: &crate::core::ProcessorRuntime) -> Self {
        match runtime {
            crate::core::ProcessorRuntime::Rust => ProcessorRuntimeOutput::Rust,
            crate::core::ProcessorRuntime::Python => ProcessorRuntimeOutput::Python,
        }
    }
}

impl From<&crate::core::PortDescriptor> for PortDescriptorOutput {
    fn from(port: &crate::core::PortDescriptor) -> Self {
        Self {
            name: port.name.clone(),
            description: port.description.clone(),
            required: port.required,
            delivery_profile: port.delivery_profile.clone(),
            audio_window: port.audio_window.clone(),
        }
    }
}

impl From<&crate::core::CodeExamples> for CodeExamplesOutput {
    fn from(examples: &crate::core::CodeExamples) -> Self {
        Self {
            rust: examples.rust.clone(),
            python: examples.python.clone(),
            typescript: examples.typescript.clone(),
        }
    }
}

#[cfg(test)]
mod link_rendering_tests {
    use super::*;
    use crate::core::graph::{
        GraphEdgeWithComponents, InputLinkPortRef, Link, LinkState, LinkStateComponent,
        OutputLinkPortRef,
    };

    /// The name of the runtime these tests render against.
    const A_RENDERING_RUNTIME: &str = "rig-desk-a1b2";

    /// Every link says who asked for it, and a link this runtime wired says
    /// this runtime — which is what makes the key readable without a reader
    /// having to know whether the mesh was involved.
    #[test]
    fn a_link_this_runtime_wired_is_created_by_this_runtime() {
        let link = Link::between(
            OutputLinkPortRef::new("Psrc", "out1"),
            InputLinkPortRef::new("Pdst", "in1"),
        );
        let rendered = serde_json::to_value(LinkOutput::of_a_link_on_the_runtime_named(
            &link,
            A_RENDERING_RUNTIME,
        ))
        .unwrap();
        assert_eq!(rendered["created_by_runtime_name"], A_RENDERING_RUNTIME);
    }

    /// A link inside this node was stamped on this machine, so it renders this
    /// machine's clock with nothing to wait for. Every link renders the key,
    /// not only a remote one: a reader comparing two links' stamps compares two
    /// strings rather than having to know which of them crossed a mesh.
    #[test]
    fn a_link_inside_this_node_renders_this_machines_clock() {
        let link = Link::between(
            OutputLinkPortRef::new("Psrc", "out1"),
            InputLinkPortRef::new("Pdst", "in1"),
        );
        let rendered = serde_json::to_value(LinkOutput::of_a_link_on_the_runtime_named(
            &link,
            A_RENDERING_RUNTIME,
        ))
        .unwrap();
        let this_machine = crate::core::runtime::mesh::MachineClockIdentity::of_this_machine();
        if this_machine.is_unidentified() {
            assert_eq!(
                rendered.get("stamp_clock_identity"),
                None,
                "this machine names no clock, and every such machine renders the same nil id"
            );
        } else {
            assert_eq!(rendered["stamp_clock_identity"], this_machine.to_string());
        }
    }

    /// A link whose source is on another runtime names no machine before it is
    /// wired — which is its whole `awaiting_remote` life, because `connect`
    /// puts it in the graph and the wiring op attaches the mesh's cell only
    /// afterwards.
    ///
    /// Fail-without-fix: read the absence of the cell as "stamped here" and
    /// every remote link claims this machine's clock from the moment it is
    /// connected until the moment it wires — the one answer that lets a reader
    /// compare it against a local stamp.
    #[test]
    fn a_link_from_another_runtime_names_no_machine_before_it_is_wired() {
        let link = Link::between(
            OutputLinkPortRef::on_another_runtime(
                crate::core::graph::MeshPortAddress::new(
                    "bench-cam-a1b2",
                    "Camera Source",
                    "video",
                )
                .expect("a legal address"),
            ),
            InputLinkPortRef::new("Pdst", "in1"),
        );
        let rendered = serde_json::to_value(LinkOutput::of_a_link_on_the_runtime_named(
            &link,
            A_RENDERING_RUNTIME,
        ))
        .unwrap();

        assert_eq!(
            rendered.get("stamp_clock_identity"),
            None,
            "nothing has said which machine stamps this link's bags, and this node is not it"
        );
    }

    /// A link on its way out names no machine, whichever end its source is on.
    ///
    /// It carries nothing, and for a remote one the cell it still holds a clone
    /// of may name the machine a *surviving* link is carrying from — so the key
    /// would outlive the link that earned it.
    #[test]
    fn a_link_on_its_way_out_names_no_machine() {
        use crate::core::graph::TheMachineClockALinksStampsAreTakenOnComponent;
        use crate::core::runtime::mesh::{
            MachineClockARemoteLinkCarriesFrom, MachineClockIdentity,
        };

        for going in [LinkState::Disconnecting, LinkState::Disconnected] {
            let mut local = Link::between(
                OutputLinkPortRef::new("Psrc", "out1"),
                InputLinkPortRef::new("Pdst", "in1"),
            );
            local.insert(LinkStateComponent(going));

            let mut remote = Link::between(
                OutputLinkPortRef::on_another_runtime(
                    crate::core::graph::MeshPortAddress::new(
                        "bench-cam-a1b2",
                        "Camera Source",
                        "video",
                    )
                    .expect("a legal address"),
                ),
                InputLinkPortRef::new("Pdst", "in1"),
            );
            let carries_from = std::sync::Arc::new(MachineClockARemoteLinkCarriesFrom::default());
            carries_from.note_the_machine_a_bag_was_stamped_on(
                MachineClockIdentity::of_the_machine_whose_boot_session_uuid_reads(
                    "8b93a1c2-0000-4d5a-9a11-2c7f0d5e2f1c",
                ),
            );
            remote.insert(LinkStateComponent(going));
            remote.insert_component_without_rendering_it(
                TheMachineClockALinksStampsAreTakenOnComponent(carries_from),
            );

            for link in [&local, &remote] {
                let rendered = serde_json::to_value(LinkOutput::of_a_link_on_the_runtime_named(
                    link,
                    A_RENDERING_RUNTIME,
                ))
                .unwrap();
                assert_eq!(
                    rendered.get("stamp_clock_identity"),
                    None,
                    "a link reading {going:?} carries nothing, so it names no machine"
                );
            }
        }
    }

    /// A link from another runtime renders the machine the mesh is carrying it
    /// from, and renders no key at all until a bag has crossed it — an absent
    /// key is "nobody has said", which is not the same as this machine.
    ///
    /// Fail-without-fix: render this machine's clock for every link and a sink
    /// fed one local track and one remote one is told the two are comparable.
    #[test]
    fn a_link_from_another_runtime_renders_the_machine_the_mesh_is_carrying_from() {
        use crate::core::graph::TheMachineClockALinksStampsAreTakenOnComponent;
        use crate::core::runtime::mesh::{
            MachineClockARemoteLinkCarriesFrom, MachineClockIdentity,
        };

        let mut link = Link::between(
            OutputLinkPortRef::on_another_runtime(
                crate::core::graph::MeshPortAddress::new(
                    "bench-cam-a1b2",
                    "Camera Source",
                    "video",
                )
                .expect("a legal address"),
            ),
            InputLinkPortRef::new("Pdst", "in1"),
        );
        let carries_from = std::sync::Arc::new(MachineClockARemoteLinkCarriesFrom::default());
        link.insert_component_without_rendering_it(TheMachineClockALinksStampsAreTakenOnComponent(
            std::sync::Arc::clone(&carries_from),
        ));
        let rendered = |link: &Link| {
            serde_json::to_value(LinkOutput::of_a_link_on_the_runtime_named(
                link,
                A_RENDERING_RUNTIME,
            ))
            .unwrap()
        };

        assert_eq!(
            rendered(&link).get("stamp_clock_identity"),
            None,
            "nothing has crossed it, so no machine has been named"
        );

        let another_machine = MachineClockIdentity::of_the_machine_whose_boot_session_uuid_reads(
            "8b93a1c2-0000-4d5a-9a11-2c7f0d5e2f1c",
        );
        carries_from.note_the_machine_a_bag_was_stamped_on(another_machine);

        assert_eq!(
            rendered(&link)["stamp_clock_identity"],
            "8b93a1c2-0000-4d5a-9a11-2c7f0d5e2f1c",
            "the rendering reads the ingress's cell, so it follows what arrives"
        );
        assert_ne!(
            rendered(&link)["stamp_clock_identity"],
            MachineClockIdentity::of_this_machine().to_string(),
        );
    }

    /// The field is the state a link was created in; wiring records its
    /// outcome on a component. Rendering reads the component first, so a
    /// `graph` read after a connect says `wired` at the top level rather than
    /// a permanent `pending` beside a `components.state` that disagrees.
    #[test]
    fn a_wired_links_top_level_state_comes_from_its_wiring_component() {
        let mut link = Link::between(
            OutputLinkPortRef::new("Psrc", "out1"),
            InputLinkPortRef::new("Pdst", "in1"),
        );
        let rendered = |link: &Link| {
            serde_json::to_value(LinkOutput::of_a_link_on_the_runtime_named(
                link,
                A_RENDERING_RUNTIME,
            ))
            .unwrap()
        };
        assert_eq!(rendered(&link)["state"], "pending");

        link.insert(LinkStateComponent(LinkState::Wired));
        assert_eq!(rendered(&link)["state"], "wired");
    }

    /// The two places a link renders its state have to say the same thing.
    ///
    /// Fail-without-fix: render `components` untouched and a link its helper
    /// opened comes back `{"state": "wired", "components": {"state":
    /// "Pending"}}` — on the surface the MCP instructions tell an agent to
    /// read.
    #[test]
    fn the_rendered_state_and_the_components_map_never_disagree() {
        use crate::core::graph::OutOfProcessLinkWireRepliesComponent;
        use crate::core::processors::{OutOfProcessLinkWireOutcome, OutOfProcessLinkWireReply};

        let mut link = Link::between(
            OutputLinkPortRef::new("Psrc", "out1"),
            InputLinkPortRef::new("Pdst", "in1"),
        );
        link.insert(LinkStateComponent(LinkState::Pending));
        let helpers_answer = OutOfProcessLinkWireReply::awaiting_the_far_sides_answer();
        link.insert_component_without_rendering_it(OutOfProcessLinkWireRepliesComponent(vec![
            std::sync::Arc::clone(&helpers_answer),
        ]));
        let rendered = |link: &Link| {
            serde_json::to_value(LinkOutput::of_a_link_on_the_runtime_named(
                link,
                A_RENDERING_RUNTIME,
            ))
            .unwrap()
        };

        assert_eq!(rendered(&link)["state"], "pending");
        assert_eq!(rendered(&link)["components"]["state"], "Pending");

        helpers_answer.note_the_far_sides_answer(OutOfProcessLinkWireOutcome::OpenedByTheFarSide);
        assert_eq!(rendered(&link)["state"], "wired");
        assert_eq!(rendered(&link)["components"]["state"], "Wired");
    }

    /// The same agreement on the arm that carries a reason.
    #[test]
    fn a_refused_links_reason_rides_beside_a_state_both_renderings_agree_on() {
        use crate::core::graph::OutOfProcessLinkWireRepliesComponent;
        use crate::core::processors::{OutOfProcessLinkWireOutcome, OutOfProcessLinkWireReply};

        let mut link = Link::between(
            OutputLinkPortRef::new("Psrc", "out1"),
            InputLinkPortRef::new("Pdst", "in1"),
        );
        link.insert(LinkStateComponent(LinkState::Pending));
        let helpers_answer = OutOfProcessLinkWireReply::awaiting_the_far_sides_answer();
        helpers_answer.note_the_far_sides_answer(
            OutOfProcessLinkWireOutcome::RefusedByTheFarSide {
                reason: "BufferSizeExceedsMaxSupportedBufferSizeOfService".to_string(),
            },
        );
        link.insert_component_without_rendering_it(OutOfProcessLinkWireRepliesComponent(vec![
            helpers_answer,
        ]));

        let rendered = serde_json::to_value(LinkOutput::of_a_link_on_the_runtime_named(
            &link,
            A_RENDERING_RUNTIME,
        ))
        .unwrap();
        assert_eq!(rendered["state"], "error");
        assert_eq!(rendered["components"]["state"], "Error");
        assert_eq!(
            rendered["error_reason"],
            "BufferSizeExceedsMaxSupportedBufferSizeOfService"
        );
    }

    /// A link nothing out of process is answering for carries no reason key at
    /// all, so a reader that finds one knows the link is refused.
    #[test]
    fn a_link_that_was_never_refused_renders_no_reason_key() {
        let mut link = Link::between(
            OutputLinkPortRef::new("Psrc", "out1"),
            InputLinkPortRef::new("Pdst", "in1"),
        );
        link.insert(LinkStateComponent(LinkState::Wired));
        let rendered = serde_json::to_value(LinkOutput::of_a_link_on_the_runtime_named(
            &link,
            A_RENDERING_RUNTIME,
        ))
        .unwrap();
        assert!(
            rendered.get("error_reason").is_none(),
            "an ordinary link's shape is unchanged by this key: {rendered}"
        );
    }
}

#[cfg(test)]
mod port_rendering_tests {
    use super::*;

    /// Every key a rendered port may carry. A port that grows a type field
    /// again fails here, whatever the field is named.
    const PORT_INFO_KEYS: [&str; 4] = ["name", "description", "port_kind", "delivery_profile"];
    const PORT_DESCRIPTOR_KEYS: [&str; 4] = ["name", "description", "required", "delivery_profile"];
    const PORT_INFO_WITH_A_CONTRACT_KEYS: [&str; 5] = [
        "name",
        "description",
        "port_kind",
        "delivery_profile",
        "audio_window",
    ];
    const PORT_DESCRIPTOR_WITH_A_CONTRACT_KEYS: [&str; 5] = [
        "name",
        "description",
        "required",
        "delivery_profile",
        "audio_window",
    ];
    const FORBIDDEN_PORT_TYPE_KEYS: [&str; 4] = ["data_type", "schema", "type", "schema_ident"];

    fn assert_renders_exactly(json: &serde_json::Value, expected: &[&str]) {
        let rendered: Vec<&String> = json.as_object().unwrap().keys().collect();
        assert_eq!(
            rendered.len(),
            expected.len(),
            "a rendered port carries exactly {expected:?}; got {rendered:?}"
        );
        for key in expected {
            assert!(json.get(key).is_some(), "missing `{key}` in {json}");
        }
    }

    fn assert_carries_no_type_key(json: &serde_json::Value) {
        for key in FORBIDDEN_PORT_TYPE_KEYS {
            assert!(
                json.get(key).is_none(),
                "port rendering must carry no type key; found `{key}` in {json}"
            );
        }
    }

    /// The contract is opt-in: a port declaring none renders exactly the four
    /// keys it always did, with no `audio_window` present as a null.
    #[test]
    fn port_info_output_renders_exactly_the_declared_keys() {
        let port = crate::core::graph::PortInfo {
            name: "video_in".to_string(),
            description: "Frames to convert".to_string(),
            port_kind: crate::core::graph::PortKind::Data,
            delivery_profile: Some("newest".to_string()),
            audio_window: None,
        };
        let json = serde_json::to_value(PortInfoOutput::from(&port)).unwrap();

        assert_eq!(json["name"], "video_in");
        assert_eq!(json["description"], "Frames to convert");
        assert_eq!(json["port_kind"], "data");
        assert_eq!(json["delivery_profile"], "newest");
        assert_renders_exactly(&json, &PORT_INFO_KEYS);
    }

    /// The type layer is gone, not omitted-when-empty: no spelling of a port
    /// type may appear on the wire, even as an absent optional field.
    #[test]
    fn port_info_output_carries_no_type_key_under_any_spelling() {
        let port = crate::core::graph::PortInfo {
            name: "data".to_string(),
            description: String::new(),
            port_kind: crate::core::graph::PortKind::Data,
            delivery_profile: None,
            audio_window: None,
        };
        let json = serde_json::to_value(PortInfoOutput::from(&port)).unwrap();
        assert_carries_no_type_key(&json);
    }

    #[test]
    fn port_descriptor_output_carries_no_type_key() {
        let pd = crate::core::PortDescriptor::new("video", "Video output", true)
            .with_delivery_profile("ordered");
        let json = serde_json::to_value(PortDescriptorOutput::from(&pd)).unwrap();

        assert_eq!(json["name"], "video");
        assert_eq!(json["description"], "Video output");
        assert_eq!(json["delivery_profile"], "ordered");
        assert_renders_exactly(&json, &PORT_DESCRIPTOR_KEYS);
        assert_carries_no_type_key(&json);
    }

    fn declared_window_contract() -> crate::core::descriptors::AudioWindowContract {
        crate::core::descriptors::AudioWindowContract::Declaration(
            crate::core::descriptors::AudioWindowContractDeclaredValues {
                sample_rate: 16_000,
                channels: Some(1),
                dtype: "f32".to_string(),
                window_size: 512,
                hop: 512,
            },
        )
    }

    #[test]
    fn a_contract_bearing_port_renders_its_contract_beside_the_four() {
        let port = crate::core::graph::PortInfo {
            name: "audio".to_string(),
            description: "Samples to frame".to_string(),
            port_kind: crate::core::graph::PortKind::Data,
            delivery_profile: Some("ordered".to_string()),
            audio_window: Some(declared_window_contract()),
        };
        let json = serde_json::to_value(PortInfoOutput::from(&port)).unwrap();

        assert_eq!(
            json["audio_window"],
            serde_json::json!({
                "resolved_from": "declaration",
                "sample_rate": 16_000,
                "channels": 1,
                "dtype": "f32",
                "window_size": 512,
                "hop": 512,
            })
        );
        assert_renders_exactly(&json, &PORT_INFO_WITH_A_CONTRACT_KEYS);
        assert_carries_no_type_key(&json);
    }

    /// A count the port left to its source is spelled in the rendering rather
    /// than left out of it: a reader learns the count follows the source,
    /// where a missing key would tell it nothing.
    #[test]
    fn a_port_that_declared_no_channel_count_renders_it_as_the_source() {
        let port = crate::core::graph::PortInfo {
            name: "audio".to_string(),
            description: String::new(),
            port_kind: crate::core::graph::PortKind::Data,
            delivery_profile: Some("ordered".to_string()),
            audio_window: Some(crate::core::descriptors::AudioWindowContract::Declaration(
                crate::core::descriptors::AudioWindowContractDeclaredValues {
                    sample_rate: 48_000,
                    channels: None,
                    dtype: "f32".to_string(),
                    window_size: 960,
                    hop: 960,
                },
            )),
        };
        let json = serde_json::to_value(PortInfoOutput::from(&port)).unwrap();

        assert_eq!(
            json["audio_window"],
            serde_json::json!({
                "resolved_from": "declaration",
                "sample_rate": 48_000,
                "channels": "source",
                "dtype": "f32",
                "window_size": 960,
                "hop": 960,
            })
        );
        assert_renders_exactly(&json, &PORT_INFO_WITH_A_CONTRACT_KEYS);
    }

    #[test]
    fn a_port_declaring_the_sentinel_renders_it_as_a_whole_contract() {
        let port = crate::core::graph::PortInfo {
            name: "audio".to_string(),
            description: String::new(),
            port_kind: crate::core::graph::PortKind::Data,
            delivery_profile: Some("ordered".to_string()),
            audio_window: Some(crate::core::descriptors::AudioWindowContract::MatchDevice {}),
        };
        let json = serde_json::to_value(PortInfoOutput::from(&port)).unwrap();

        assert_eq!(
            json["audio_window"],
            serde_json::json!({ "resolved_from": "match_device" })
        );
    }

    /// A node whose input port declares the sentinel, with `settled` optionally
    /// standing in for what its own device gave it.
    fn a_node_declaring_the_sentinel(
        settled: Option<crate::iceoryx2::ResolvedAudioWindowContract>,
    ) -> crate::core::graph::ProcessorNode {
        use crate::core::graph::GraphNodeWithComponents;

        let mut node = crate::core::graph::ProcessorNode::new(
            streamlib_processor_schema::ProcessorClassImportPath::new("tests.SpeakerSink")
                .expect("a legal import path"),
            "SpeakerSink",
            None,
            vec![crate::core::graph::PortInfo {
                name: "audio".to_string(),
                description: "Blocks to play".to_string(),
                port_kind: crate::core::graph::PortKind::Data,
                delivery_profile: Some("ordered".to_string()),
                audio_window: Some(crate::core::descriptors::AudioWindowContract::MatchDevice {}),
            }],
            Vec::new(),
        );

        let contracts = std::sync::Arc::new(
            crate::iceoryx2::DeviceMatchedAudioWindowContractsByInputPort::default(),
        );
        if let Some(contract) = settled {
            contracts.settle_for_input_port("audio", contract);
        }
        node.insert_component_without_rendering_it(
            crate::core::graph::DeviceMatchedAudioWindowContractsComponent(contracts),
        );
        node
    }

    fn a_playback_stream_of(
        sample_rate: u32,
        channels: u32,
    ) -> crate::core::context::AudioStreamFormat {
        crate::core::context::AudioStreamFormat {
            sample_rate,
            channels,
            sample_format: crate::core::context::AudioSampleFormat::F32,
        }
    }

    /// `graph` renders what the device gave rather than the sentinel that asked
    /// for it — machine-dependent because the device format is, which is truer
    /// than a static lie.
    #[test]
    fn a_settled_match_device_port_renders_the_five_values_its_device_gave() {
        let settled = crate::iceoryx2::ResolvedAudioWindowContract::from_a_device_stream_format(
            &crate::iceoryx2::AudioWindowContractMatchingADeviceStream {
                device_stream_format: a_playback_stream_of(44_100, 2),
                window_size_in_per_channel_samples: 441,
                hop_in_per_channel_samples: 441,
            },
        )
        .expect("a device format settles a contract");

        let rendered = serde_json::to_value(ProcessorNodeOutput::from(
            &a_node_declaring_the_sentinel(Some(settled)),
        ))
        .unwrap();

        assert_eq!(
            rendered["ports"]["inputs"][0]["audio_window"],
            serde_json::json!({
                "resolved_from": "device",
                "sample_rate": 44_100,
                "channels": 2,
                "dtype": "f32",
                "window_size": 441,
                "hop": 441,
            })
        );
    }

    /// Before its processor's `setup()` opens a device there is nothing to
    /// render but the declaration, and rendering a guess in its place would be
    /// the static lie the resolved rendering exists instead of.
    #[test]
    fn an_unsettled_match_device_port_still_renders_the_sentinel() {
        let rendered = serde_json::to_value(ProcessorNodeOutput::from(
            &a_node_declaring_the_sentinel(None),
        ))
        .unwrap();

        assert_eq!(
            rendered["ports"]["inputs"][0]["audio_window"],
            serde_json::json!({ "resolved_from": "match_device" })
        );
    }

    /// The settled contracts reach a reader on the port that settled them and
    /// nowhere else: a second rendering under `components` would be one more
    /// copy of the same fact to keep in agreement.
    #[test]
    fn the_settled_contracts_render_on_the_port_and_not_as_a_component_of_their_own() {
        let rendered = serde_json::to_value(ProcessorNodeOutput::from(
            &a_node_declaring_the_sentinel(None),
        ))
        .unwrap();

        assert!(
            rendered["components"]
                .get("device_matched_audio_window_contracts")
                .is_none(),
            "the settled contracts carry no `components` key of their own; got {}",
            rendered["components"]
        );
    }

    /// The contract rides `PortDescriptor` into `PortInfo` untouched — the
    /// carrier the macro and the wheel's declaration bridge both fill.
    #[test]
    fn a_declared_contract_survives_the_descriptor_to_port_info_hop() {
        let descriptor = crate::core::PortDescriptor::new("audio", "Samples to frame", true)
            .with_delivery_profile("ordered")
            .with_audio_window_contract(declared_window_contract());

        let port = crate::core::graph::PortInfo::from(&descriptor);

        assert_eq!(port.audio_window, Some(declared_window_contract()));
    }

    #[test]
    fn a_contract_bearing_descriptor_renders_its_contract_too() {
        let descriptor = crate::core::PortDescriptor::new("audio", "Samples to frame", true)
            .with_delivery_profile("ordered")
            .with_audio_window_contract(
                crate::core::descriptors::AudioWindowContract::MatchDevice {},
            );
        let json = serde_json::to_value(PortDescriptorOutput::from(&descriptor)).unwrap();

        assert_eq!(
            json["audio_window"],
            serde_json::json!({ "resolved_from": "match_device" })
        );
        assert_renders_exactly(&json, &PORT_DESCRIPTOR_WITH_A_CONTRACT_KEYS);
    }
}

#[cfg(test)]
mod capability_extension_and_mesh_rendering_tests {
    //! `extensions` and `mesh` are top-level keys beside `nodes` and `links`,
    //! and a reader that finds either absent cannot tell "nothing loaded" or
    //! "no peers" from "this engine predates the key" — so both are always
    //! present.

    use super::*;
    use crate::core::graph::Graph;
    use crate::core::runtime::{LoadedCapabilityExtension, LoadedCapabilityExtensionRegistry};

    fn an_isolated_mesh() -> RuntimeMeshOutput {
        RuntimeMeshOutput {
            mesh_name: "default".to_string(),
            runtime_name: "rig-desk-a1b2".to_string(),
            session: RuntimeMeshSessionOutput::Open,
            local_only_reason: None,
            peers: Vec::new(),
            egress_ports: Vec::new(),
            link_requests_awaiting_runtime: Vec::new(),
        }
    }

    #[test]
    fn a_graph_with_no_extensions_still_carries_the_key_as_an_empty_list() {
        let rendered =
            serde_json::to_value(Graph::new().to_graph_response(Vec::new(), an_isolated_mesh()))
                .unwrap();

        let keys: Vec<&String> = rendered.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["nodes", "links", "extensions", "mesh"]);
        assert_eq!(rendered["extensions"], serde_json::json!([]));
    }

    /// An isolated runtime renders `open` with no peers, which is how a reader
    /// tells it apart from one whose session never opened.
    #[test]
    fn an_isolated_runtime_renders_an_open_session_with_no_peers_and_no_reason() {
        let rendered =
            serde_json::to_value(Graph::new().to_graph_response(Vec::new(), an_isolated_mesh()))
                .unwrap();

        assert_eq!(
            rendered["mesh"],
            serde_json::json!({
                "mesh_name": "default",
                "runtime_name": "rig-desk-a1b2",
                "session": "open",
                "peers": [],
                "egress_ports": [],
                "link_requests_awaiting_runtime": [],
            })
        );
    }

    /// A port another runtime is reading renders under the display name the
    /// mesh addresses it by, with every reader — so an agent on the sending
    /// node can see who is pulling from it without asking the other end.
    #[test]
    fn a_port_another_runtime_reads_renders_with_the_runtimes_reading_it() {
        let rendered = serde_json::to_value(Graph::new().to_graph_response(
            Vec::new(),
            RuntimeMeshOutput {
                egress_ports: vec![MeshEgressPortOutput {
                    processor_display_name: "CameraSource".to_string(),
                    port_name: "video".to_string(),
                    reader_runtime_names: vec![
                        "bench-fx-c3d4".to_string(),
                        "bench-rec-e5f6".to_string(),
                    ],
                }],
                ..an_isolated_mesh()
            },
        ))
        .unwrap();

        assert_eq!(
            rendered["mesh"]["egress_ports"],
            serde_json::json!([{
                "processor_display_name": "CameraSource",
                "port_name": "video",
                "reader_runtime_names": ["bench-fx-c3d4", "bench-rec-e5f6"],
            }])
        );

        let read_back: GraphResponse =
            serde_json::from_value(rendered).expect("an egress entry deserializes");
        assert_eq!(read_back.mesh.egress_ports.len(), 1);
    }

    /// A peer that has not answered yet renders its name alone, and the whole
    /// response still deserializes — which is what the control plane's prompt
    /// rendering does to every graph a node exports.
    #[test]
    fn a_peer_that_has_not_answered_still_deserializes_beside_one_that_has() {
        let rendered = serde_json::to_value(Graph::new().to_graph_response(
            Vec::new(),
            RuntimeMeshOutput {
                peers: vec![
                    RuntimeMeshPeerOutput {
                        runtime_name: "not-yet-answered".to_string(),
                        runtime_id: None,
                        host_name: None,
                        engine_version: None,
                        control_plane_urls: None,
                    },
                    RuntimeMeshPeerOutput {
                        runtime_name: "answered".to_string(),
                        runtime_id: Some("R7".to_string()),
                        host_name: Some("rig".to_string()),
                        engine_version: Some("0.25.0".to_string()),
                        control_plane_urls: Some(vec!["http://198.51.100.7:9000".to_string()]),
                    },
                ],
                ..an_isolated_mesh()
            },
        ))
        .unwrap();

        assert_eq!(
            rendered["mesh"]["peers"][0],
            serde_json::json!({ "runtime_name": "not-yet-answered" })
        );

        let read_back: GraphResponse =
            serde_json::from_value(rendered).expect("both peer shapes deserialize");
        assert_eq!(read_back.mesh.peers.len(), 2);
        assert!(read_back.mesh.peers[0].runtime_id.is_none());
        assert_eq!(read_back.mesh.peers[1].runtime_id.as_deref(), Some("R7"));
    }

    /// A runtime whose session never opened says so, and says why.
    #[test]
    fn a_local_only_runtime_renders_the_reason_its_session_did_not_open() {
        let rendered = serde_json::to_value(Graph::new().to_graph_response(
            Vec::new(),
            RuntimeMeshOutput {
                session: RuntimeMeshSessionOutput::LocalOnly,
                local_only_reason: Some("the listen endpoint is already taken".to_string()),
                ..an_isolated_mesh()
            },
        ))
        .unwrap();

        assert_eq!(rendered["mesh"]["session"], "local_only");
        assert_eq!(
            rendered["mesh"]["local_only_reason"],
            "the listen endpoint is already taken"
        );
    }

    #[test]
    fn a_registered_extension_renders_its_name_version_and_distribution() {
        let registry = LoadedCapabilityExtensionRegistry::default();
        registry
            .register(LoadedCapabilityExtension {
                name: "webrtc".to_string(),
                version: "0.2.0".to_string(),
                distribution: "streamlib-webrtc".to_string(),
            })
            .expect("the capability registers");
        let extensions: Vec<_> = registry
            .registered()
            .into_iter()
            .map(LoadedCapabilityExtensionOutput::from)
            .collect();

        let rendered =
            serde_json::to_value(Graph::new().to_graph_response(extensions, an_isolated_mesh()))
                .unwrap();

        assert_eq!(
            rendered["extensions"],
            serde_json::json!([{
                "name": "webrtc",
                "version": "0.2.0",
                "distribution": "streamlib-webrtc",
            }])
        );
    }
}

#[cfg(test)]
mod config_schema_rendering_tests {
    use super::*;
    use crate::core::descriptors::{
        ProcessorClassImportPath, ProcessorClassShortName, ProcessorDescriptor,
    };

    fn descriptor_carrying(config_schema: Option<serde_json::Value>) -> ProcessorDescriptor {
        let descriptor = ProcessorDescriptor::new(
            ProcessorClassShortName::new("TestPatternSource").unwrap(),
            ProcessorClassImportPath::new("streamlib_media_builtins::test_pattern_source").unwrap(),
            "a probe",
        );
        match config_schema {
            Some(document) => descriptor.with_config_schema(document),
            None => descriptor,
        }
    }

    /// The control plane serves the descriptor's document, not a summary of
    /// it: a field's type, its description and its default all survive the
    /// hop, because the MCP catalog reads the same rendering.
    #[test]
    fn a_registered_descriptors_config_schema_reaches_the_rendering_unchanged() {
        let document = serde_json::json!({
            "type": "object",
            "properties": {
                "width": { "type": "integer", "description": "Frame width in pixels.", "default": 1280 },
                "height": { "type": "integer", "description": "Frame height in pixels.", "default": 720 },
            },
        });
        let rendered = serde_json::to_value(ProcessorDescriptorOutput::from(&descriptor_carrying(
            Some(document.clone()),
        )))
        .unwrap();
        assert_eq!(rendered["config_schema"], document);
    }

    /// A descriptor built without one — which no declared processor is, in
    /// either language — renders no key at all rather than a null an agent
    /// would have to read as "no config".
    #[test]
    fn a_descriptor_carrying_no_config_schema_renders_no_key_rather_than_a_null() {
        let rendered =
            serde_json::to_value(ProcessorDescriptorOutput::from(&descriptor_carrying(None)))
                .unwrap();
        assert!(rendered.get("config_schema").is_none(), "{rendered}");
    }
}
