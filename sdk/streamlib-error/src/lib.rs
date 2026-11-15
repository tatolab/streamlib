// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Canonical StreamLib [`Error`] + [`Result`].
//!
//! Shared by `streamlib-engine` (which re-exports it at
//! `core::error`) and the engine-free authoring surface. Every variant
//! is String / std / anyhow based plus the engine-free
//! `ProcessorClassImportPath` and `ExposedNameCastsToNothingError` (from
//! `streamlib-processor-schema`) and, on Linux, the engine-free
//! `ConsumerRhiError` conversion.

mod channel_trust_tier_label;
#[cfg(target_os = "linux")]
mod consumer_rhi_error_conversion;
mod device_memory_heap_description;
mod node_and_port_refusal_details;
mod privacy_gated_capture_device;

#[cfg(test)]
mod tests;

pub use channel_trust_tier_label::ChannelTrustTierLabel;
pub use device_memory_heap_description::{DeviceMemoryHeapBudgetSource, DeviceMemoryHeapLocality};
use node_and_port_refusal_details::node_types_listed_for_a_refusal;
pub use node_and_port_refusal_details::{
    OutputPortNotExposedToTheReader, PortDirection, WhyANodeOfAStreamIsNotRunning,
};
pub use privacy_gated_capture_device::PrivacyGatedCaptureDevice;
use streamlib_processor_schema::{ExposedNameCastsToNothingError, ProcessorClassImportPath};

/// The StreamLib error type.
#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("GPU operation failed: {0}")]
    GpuError(String),

    /// The engine could not give a starting stream what every stream's start
    /// needs — the GPU context — a refusal no stream owns.
    #[error("the engine could not give the stream its GPU context: {0}")]
    EngineResourceRefusedAtAStreamsStart(String),

    #[error("Shader compilation failed: {0}")]
    ShaderCompilation(String),

    #[error("Texture operation failed: {0}")]
    TextureError(String),

    #[error("Stream graph error: {0}")]
    GraphError(String),

    #[error("Port error: {0}")]
    PortError(String),

    #[error("Link error: {0}")]
    Link(String),

    #[error("Link already exists: {0}")]
    LinkAlreadyExists(String),

    #[error("Link not found: {0}")]
    LinkNotFound(String),

    #[error("Link not wired: {0}")]
    LinkNotWired(String),

    #[error("Link already disconnected: {0}")]
    LinkAlreadyDisconnected(String),

    #[error("Invalid link: {0}")]
    InvalidLink(String),

    /// A chunk of a channel name is not one address chunk.
    #[error(
        "channel `{channel}` has the chunk `{chunk}`, which is not one address chunk: {reason}"
    )]
    ChannelNameChunkIsNotOneAddressChunk {
        /// The channel name as it was given.
        channel: String,
        /// The chunk the address grammar refuses.
        chunk: String,
        /// Why it is not one, with the grammar it breaks.
        reason: String,
    },

    #[error("channel `{name}` is {len} bytes, exceeding the {max}-byte wire capacity")]
    ChannelNameTooLong {
        name: String,
        len: usize,
        max: usize,
    },

    #[error(transparent)]
    ExposedNameCastsToNothing(#[from] ExposedNameCastsToNothingError),

    #[error(
        "the node name `{name}` casts to `{cast}`, which node `{cast}` in this graph already \
         has — a name the author gives is an address, so it is never suffixed. Give one of \
         them another name, or leave the name out to take the class's short name with the \
         next free `-2`, `-3` …"
    )]
    NodeNameTaken { name: String, cast: String },

    #[error("{0}")]
    OutputPortNotExposedToTheReader(Box<OutputPortNotExposedToTheReader>),

    #[error("Invalid graph: {0}")]
    InvalidGraph(String),

    #[error("Processor not found: {0}")]
    ProcessorNotFound(String),

    #[error(
        "this runtime ({runtime_version}) has no node type `{ident}`",
        runtime_version = env!("CARGO_PKG_VERSION")
    )]
    UnknownProcessorType { ident: ProcessorClassImportPath },

    #[error(
        "cannot describe {}: {refusal}",
        node_types_listed_for_a_refusal(node_types)
    )]
    NodeTypesNotDescribed {
        node_types: Vec<ProcessorClassImportPath>,
        refusal: String,
    },

    #[error(
        "`{node_type}` is no longer a built-in node: it is `{module_node_type}`, a node \
         `pip install {module_distribution}` gives the stream"
    )]
    BuiltInNodeTypeMovedToAModule {
        node_type: ProcessorClassImportPath,
        module_node_type: ProcessorClassImportPath,
        module_distribution: String,
    },

    #[error("node `{node_name}` (`{node_type}`) does not take its config: {refusal}")]
    NodeConfigRefused {
        node_name: String,
        node_type: ProcessorClassImportPath,
        refusal: String,
    },

    #[error(
        "processor `{processor_class_import_path}` declares the port `{port_name}`, which is \
         not a cast name — a descriptor carries every port the way `#[processor]` and `@node.process` \
         declare it: lowercase a-z 0-9 - . _ ~, no leading, trailing or doubled '-', at most \
         {max} characters",
        max = streamlib_processor_schema::EXPOSED_NAME_MAXIMUM_LENGTH
    )]
    DescriptorPortNameNotCast {
        processor_class_import_path: ProcessorClassImportPath,
        port_name: String,
    },

    #[error(
        "processor `{processor_class_import_path}` declares the port `{port_name}` more than \
         once — every port, input or output, needs its own name"
    )]
    DescriptorPortNameDeclaredTwice {
        processor_class_import_path: ProcessorClassImportPath,
        port_name: String,
    },

    /// Stream facts stated for an output port of another family.
    #[error(transparent)]
    StreamFactsOfAnotherFamily(#[from] streamlib_processor_schema::StreamFactsOfAnotherFamily),

    /// A node named in a stream has no output port of the name given.
    #[error(
        "node `{node}` in stream `{stream}` has no output port `{port}`. Its output ports are: \
         {output_ports_the_node_has}"
    )]
    NodeInAStreamHasNoOutputPortNamed {
        /// The stream that holds the node.
        stream: String,
        /// The node's name.
        node: String,
        /// The output port name as it was given.
        port: String,
        /// The node's output ports, comma-joined, or `none`.
        output_ports_the_node_has: String,
    },

    /// A node of a stream that a caller reached for does not run.
    #[error("{runtime_words}")]
    NodeOfAStreamIsNotRunning {
        /// Why the node does not run.
        not_running_because: WhyANodeOfAStreamIsNotRunning,
        /// The runtime's words for it, never rewritten.
        runtime_words: String,
    },

    #[error("Processor '{processor_id}' has no {direction} port named '{port_name}'")]
    ProcessorPortNotFound {
        processor_id: String,
        port_name: String,
        direction: PortDirection,
    },

    #[error("Buffer operation failed: {0}")]
    BufferError(String),

    #[error(
        "surface '{surface_id}' names a recycled frame: its pool slot has been rehanded to \
         the producer since (this id published generation {published_generation}, the slot \
         is on generation {current_generation}), so the frame this id named no longer \
         exists — a frame must be claimed at read time to outlive the pool's cycling"
    )]
    SurfaceFrameRecycled {
        surface_id: String,
        published_generation: u64,
        current_generation: u64,
    },

    #[error(
        "every one of the {pool_capacity} slots in processor output pool '{pool_key}' is held \
         by a consumer, so the producer drops this frame rather than overwriting one something \
         is still reading"
    )]
    EverySlotInTheProcessorOutputPoolIsInUse {
        pool_key: String,
        pool_capacity: usize,
    },

    #[error(
        "GPU pool '{pool}' already has {claimant_column_limit} interpreters claiming its frames,          the most its claim table holds, so this interpreter's link to it is refused"
    )]
    GpuPoolClaimantColumnsExhausted {
        pool: String,
        claimant_column_limit: usize,
    },

    /// A node's one-off acquisition refused because the node already holds
    /// as many of that shape as one node may.
    #[error(
        "this node already holds {one_off_acquisitions_one_node_may_hold_of_one_shape} one-off \
         acquisitions of {acquisition_shape}, the most one node may hold of one shape at once, so \
         this one is refused before anything is allocated; release one it holds, or publish \
         per-frame output from a node output pool"
    )]
    EveryOneOffAcquisitionOfThisShapeTheNodeMayHoldIsHeld {
        acquisition_shape: String,
        one_off_acquisitions_one_node_may_hold_of_one_shape: usize,
    },

    /// A frame allocation the device memory heap it would come from has no
    /// room for, refused before anything was allocated.
    #[error(
        "{frame_allocation_site} refused a {requested_bytes}-byte frame allocation before \
         allocating: device memory heap {heap_index} ({heap_locality}) has a budget of \
         {heap_budget_bytes} bytes ({heap_budget_source}) and uses {heap_usage_bytes} bytes, of \
         which this runtime's allocator pools hold \
         {bytes_this_runtimes_allocator_pools_hold_on_the_heap} bytes; the allocation would leave \
         less than the {headroom_bytes}-byte headroom"
    )]
    FrameAllocationRefusedPastTheDeviceMemoryHeapBudget {
        heap_index: u32,
        heap_locality: DeviceMemoryHeapLocality,
        heap_budget_bytes: u64,
        heap_usage_bytes: u64,
        bytes_this_runtimes_allocator_pools_hold_on_the_heap: u64,
        requested_bytes: u64,
        headroom_bytes: u64,
        heap_budget_source: DeviceMemoryHeapBudgetSource,
        frame_allocation_site: String,
    },

    /// A frame allocation refused because the process holds nearly as many
    /// device memory allocations as the device allows.
    #[error(
        "{frame_allocation_site} refused a frame allocation before allocating: this process \
         holds {live_device_memory_allocation_count} device memory allocations of the \
         {max_memory_allocation_count} the device allows, fewer than \
         {allocations_kept_in_reserve} short of its limit"
    )]
    FrameAllocationRefusedNearTheDeviceMemoryAllocationCountLimit {
        live_device_memory_allocation_count: u64,
        max_memory_allocation_count: u32,
        allocations_kept_in_reserve: u32,
        frame_allocation_site: String,
    },

    /// A frame allocation Metal's recommended working set has no room for,
    /// refused before anything was allocated.
    #[error(
        "{frame_allocation_site} refused a {requested_bytes}-byte frame allocation before \
         allocating: Metal's recommended working set is {recommended_max_working_set_bytes} \
         bytes and the Metal device has allocated {metal_current_allocated_bytes} bytes; the \
         allocation would leave less than the {headroom_bytes}-byte headroom"
    )]
    FrameAllocationRefusedPastTheMetalWorkingSetBudget {
        recommended_max_working_set_bytes: u64,
        metal_current_allocated_bytes: u64,
        requested_bytes: u64,
        headroom_bytes: u64,
        frame_allocation_site: String,
    },

    #[error("Clock synchronization error: {0}")]
    ClockError(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Invalid configuration: {0}")]
    Configuration(String),

    /// A node refused a privacy-gated capture device whose access nobody has
    /// asked the user for, because nobody acted on its stream.
    #[error("{refusal}")]
    CaptureDeviceAccessNeverAskedFor {
        device: PrivacyGatedCaptureDevice,
        refusal: String,
    },

    #[error("Config update failed: {0}")]
    Config(String),

    #[error("Operation not supported: {0}")]
    NotSupported(String),

    #[error("Resource not found: {0}")]
    NotFound(String),

    #[error("Runtime error: {0}")]
    Runtime(String),

    #[error(
        "the reserved subscriber slot on channel '{0}' is already held — a channel reserves \
         exactly one slot beyond its destinations, and the runtime's publisher reads it"
    )]
    ReservedPublisherSubscriberSlotOccupied(String),

    #[error("Bag key '{key}' is not present")]
    BagKeyMissing { key: String },

    #[error("Bag key '{key}' could not be read as `{expected_type}`: {detail}")]
    BagTypeMismatch {
        key: String,
        expected_type: String,
        detail: String,
    },

    #[error("Bag msgpack decode failed: {0}")]
    BagDecodeFailed(String),

    #[error("Bag msgpack encode failed: {0}")]
    BagEncodeFailed(String),

    #[error(
        "payload of {payload_bytes} bytes on channel '{channel}' is past the \
         {largest_admitted_frame_bytes} bytes its {chunk_ceiling_bytes}-byte \
         per-channel ceiling admits ({tier} tier) — the sample was refused and \
         counted, the stream continues; split the payload, or raise this tier's \
         max_payload_bytes_per_channel above {chunk_ceiling_bytes}"
    )]
    PayloadExceedsChannelCeiling {
        channel: String,
        payload_bytes: usize,
        /// The most bytes one frame may carry on this channel — the per-channel
        /// shared-memory chunk ceiling less what iceoryx2 lays out ahead of a
        /// frame in the sample carrying it. What a producer must fit inside.
        largest_admitted_frame_bytes: usize,
        /// The per-channel shared-memory chunk ceiling itself: the tier default
        /// or its operator override, and so the knob to raise.
        chunk_ceiling_bytes: usize,
        tier: ChannelTrustTierLabel,
        /// The refusing output port's refused-bag total, this refusal included.
        refused_bags_on_the_output_port: u64,
    },

    #[error(
        "a sample on input port '{port}' stamps a payload length of \
         {stamped_payload_bytes} bytes but carries only {available_payload_bytes} — \
         the sample is malformed and was dropped"
    )]
    ChannelSampleStampsAPayloadPastTheSample {
        port: String,
        stamped_payload_bytes: usize,
        available_payload_bytes: usize,
    },

    #[error(
        "a bag on input port '{port}' cannot be read as an audio block, which its \
         `audio_window` contract makes the engine's job to do: {refusal}"
    )]
    AudioWindowStageCannotReadTheBag { port: String, refusal: String },

    #[error(
        "input port '{port}' declares an `audio_window` contract of {contract_channels} \
         channels and received a block of {source_channels} — the stage converts N to 1 by \
         averaging and 1 to N by duplicating, and refuses every other pair rather than \
         inventing a mix"
    )]
    AudioWindowStageChannelConversionRefused {
        port: String,
        source_channels: u32,
        contract_channels: u32,
    },

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// StreamLib result alias.
pub type Result<T> = std::result::Result<T, Error>;
