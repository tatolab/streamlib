// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Canonical StreamLib [`Error`] + [`Result`].
//!
//! Shared by `streamlib-engine` (which re-exports it at
//! `core::error`) and the engine-free authoring surface. Every variant
//! is String / std / anyhow based plus the engine-free
//! `ProcessorClassImportPath` and `ExposedNameCastsToNothingError` (from
//! `streamlib-processor-schema`) and, on Linux, the engine-free
//! `ConsumerRhiError` conversion.

use streamlib_processor_schema::{ExposedNameCastsToNothingError, ProcessorClassImportPath};

/// An output port refused a reader from outside its stream: the port's level
/// does not allow a reader where that reader reads from.
#[derive(thiserror::Error, Debug)]
#[error(
    "port `{port}` of node `{node}` in stream `{stream}` is {level}, and {reader} reads only a \
     port that is {levels_the_reader_may_read}"
)]
pub struct OutputPortNotExposedToTheReader {
    /// The stream that holds the port.
    pub stream: String,
    /// The node that owns the port.
    pub node: String,
    /// The output port's name on that node.
    pub port: String,
    /// The port's level: `internal`, `private` or `public`.
    pub level: String,
    /// Where the refused reader reads from.
    pub reader: String,
    /// The levels a reader there may read.
    pub levels_the_reader_may_read: String,
}

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

    #[error("Invalid port address: {0}")]
    InvalidPortAddress(String),

    /// An address names a machine other than this one.
    #[error(
        "the address `{address}` names the machine `{machine}`, and this machine is \
         `{this_machine}`: an address names a port on this machine until the sharing step, \
         which is where links between machines arrive"
    )]
    PortAddressNamesAnotherMachine {
        /// The address as it was written.
        address: String,
        /// The machine its machine part names, cast.
        machine: String,
        /// This machine's name.
        this_machine: String,
    },

    /// A link was made in a stream other than the one that holds its input.
    #[error(
        "a link is made in the stream that holds its input, and this link's input is in the \
         stream `{input_stream}` while it was made in the stream `{acting_stream}`: make it in \
         `{input_stream}`, naming the stream its source is in when that is another one"
    )]
    LinkMadeOutsideTheStreamOfItsInput {
        /// The stream the link was made in.
        acting_stream: String,
        /// The stream that holds the link's input.
        input_stream: String,
    },

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
        "`{node_type}` is a built-in node this runtime does not have on {this_floor}: it runs \
         on {floors_it_runs_on} only"
    )]
    BuiltInNodeTypeAbsentOnThisFloor {
        node_type: ProcessorClassImportPath,
        this_floor: String,
        floors_it_runs_on: String,
    },

    #[error("node `{node_name}` (`{node_type}`) does not take its config: {refusal}")]
    NodeConfigRefused {
        node_name: String,
        node_type: ProcessorClassImportPath,
        refusal: String,
    },

    #[error(
        "processor `{processor_class_import_path}` declares the port `{port_name}`, which is \
         not a cast name — a descriptor carries every port the way `#[processor]` and `@node` \
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

    /// A descriptor declaring a catalog entry on an input port.
    #[error(
        "processor `{processor_class_import_path}` declares a catalog entry on input port \
         `{port_name}` — an entry describes what a producer writes, so only an output port \
         carries one"
    )]
    DescriptorCatalogEntryOnInputPort {
        /// The processor whose descriptor declares it.
        processor_class_import_path: ProcessorClassImportPath,
        /// The input port that carries it.
        port_name: String,
    },

    /// An output port's catalog entry the players' own catalog types do not read.
    #[error(
        "output port `{port_name}` carries a {section} catalog entry hang does not read: {refusal}"
    )]
    CatalogEntryRefused {
        /// The output port the entry describes.
        port_name: String,
        /// The section the entry names.
        section: streamlib_processor_schema::CatalogSection,
        /// hang's own refusal, naming the field.
        refusal: String,
    },

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

    #[error("Config update failed: {0}")]
    Config(String),

    #[error("Operation not supported: {0}")]
    NotSupported(String),

    #[error("Resource not found: {0}")]
    NotFound(String),

    #[error("Runtime error: {0}")]
    Runtime(String),

    #[error(
        "no tappable channel named '{0}' in the running graph — a channel's \
         iceoryx2 data service exists only once a connect() has wired its source \
         output port"
    )]
    TapChannelNotFound(String),

    #[error(
        "the reserved tap slot on channel '{0}' is already occupied — a channel \
         reserves exactly one tap subscriber slot, so only one concurrent tap is \
         allowed; detach the existing tap first"
    )]
    TapSlotOccupied(String),

    #[error(
        "channel '{0}' carries an exposed output port, and the runtime's publisher reads it \
         through the channel's one reserved slot — read the port's track at a relay instead"
    )]
    TapOfAnOutputPortThePublisherReads(String),

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
        "frame on input port '{port}' stamps a payload length of \
         {stamped_payload_bytes} bytes but carries only {available_payload_bytes} \
         after its header — the frame is malformed and was dropped"
    )]
    FrameHeaderPayloadLengthExceedsFrameBytes {
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

/// Direction of a port relative to its processor — `Output` for source-side,
/// `Input` for destination-side. Used by [`Error::ProcessorPortNotFound`] to
/// distinguish "the source processor has no output port named X" from "the
/// target processor has no input port named X."
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortDirection {
    Input,
    Output,
}

impl PortDirection {
    /// The direction's spelling on the helper-process wire, which every SDK
    /// matches on to pick which of its own ports a command names. Changing
    /// either string is a protocol break, so it is spelled here rather than
    /// taken from [`Display`], whose job is prose.
    ///
    /// [`Display`]: std::fmt::Display
    pub fn as_wire_str(&self) -> &'static str {
        match self {
            Self::Input => "input",
            Self::Output => "output",
        }
    }
}

impl std::fmt::Display for PortDirection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_wire_str())
    }
}

/// Whether a device memory heap is the device's own memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceMemoryHeapLocality {
    /// The heap carries `VK_MEMORY_HEAP_DEVICE_LOCAL_BIT`.
    DeviceLocal,
    /// The heap is system memory the device reaches.
    NotDeviceLocal,
}

impl std::fmt::Display for DeviceMemoryHeapLocality {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DeviceLocal => f.write_str("device-local"),
            Self::NotDeviceLocal => f.write_str("not device-local"),
        }
    }
}

/// Where a device memory heap's budget came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceMemoryHeapBudgetSource {
    /// `VK_EXT_memory_budget`'s `heapBudget`, as the driver reported it.
    ReportedByTheDriver,
    /// The allocator's estimate without the extension: 80% of the heap's size.
    EstimatedAsEightyPercentOfTheHeap,
    /// That estimate, lowered to the heap's usage plus the system's available memory.
    EstimatedAndCappedByTheSystemsAvailableMemory,
}

impl std::fmt::Display for DeviceMemoryHeapBudgetSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ReportedByTheDriver => f.write_str("reported by the driver"),
            Self::EstimatedAsEightyPercentOfTheHeap => {
                f.write_str("estimated as 80% of the heap, the driver reporting no budget")
            }
            Self::EstimatedAndCappedByTheSystemsAvailableMemory => f.write_str(
                "estimated as 80% of the heap, the driver reporting no budget, and capped by \
                 the system's available memory",
            ),
        }
    }
}

/// Trust tier of the iceoryx2 data channel a payload was refused on, named in
/// [`Error::PayloadExceedsChannelCeiling`]. Mirrors the engine's
/// `iceoryx2::ChannelTrustTier` at the error boundary so the ceiling error stays
/// engine-free; the engine maps its own enum onto this at the construction site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelTrustTierLabel {
    /// In-process host-to-host channel.
    Trusted,
    /// Channel with a subprocess (Python / Deno) on either end.
    UntrustedSession,
}

impl std::fmt::Display for ChannelTrustTierLabel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Trusted => f.write_str("trusted"),
            Self::UntrustedSession => f.write_str("untrusted-session"),
        }
    }
}

/// Node types as a refusal names them: each in backticks, comma-separated.
fn node_types_listed_for_a_refusal(node_types: &[ProcessorClassImportPath]) -> String {
    node_types
        .iter()
        .map(|node_type| format!("`{node_type}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(target_os = "linux")]
impl From<streamlib_consumer_rhi::ConsumerRhiError> for Error {
    fn from(e: streamlib_consumer_rhi::ConsumerRhiError) -> Self {
        match e {
            streamlib_consumer_rhi::ConsumerRhiError::Gpu(s) => Error::GpuError(s),
            streamlib_consumer_rhi::ConsumerRhiError::Configuration(s) => Error::Configuration(s),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The message carries the requested import path verbatim — the caller
    /// asked for a class by that name, and matching their spelling is what
    /// makes a typo findable. The install fix-it died with the module system.
    #[test]
    fn unknown_processor_type_names_the_class_that_is_not_registered() {
        let msg = Error::UnknownProcessorType {
            ident: ProcessorClassImportPath::new("my_app.filters:BlurProcessor").unwrap(),
        }
        .to_string();
        assert!(
            msg.contains("my_app.filters:BlurProcessor"),
            "the requested path must reach the user unaltered: {msg}"
        );
        assert!(
            !msg.contains("streamlib add"),
            "no install fix-it survives the module-system removal: {msg}"
        );
    }

    #[test]
    fn a_describe_refusal_names_every_type_it_refused_and_why() {
        let msg = Error::NodeTypesNotDescribed {
            node_types: vec![
                ProcessorClassImportPath::new("my_app.filters:Blur").unwrap(),
                ProcessorClassImportPath::new("my_app.filters:Sharpen").unwrap(),
            ],
            refusal: "the interpreter printed no describe document".to_string(),
        }
        .to_string();
        assert_eq!(
            msg,
            "cannot describe `my_app.filters:Blur`, `my_app.filters:Sharpen`: the interpreter \
             printed no describe document"
        );
    }

    /// A stream names no runtime version, so the refusal of a type this runtime
    /// lacks names the runtime's own, to compare with the stream's library.
    #[test]
    fn an_unknown_node_type_names_this_runtimes_own_version() {
        let msg = Error::UnknownProcessorType {
            ident: ProcessorClassImportPath::new("tatolab.stream:NewThing").unwrap(),
        }
        .to_string();
        assert_eq!(
            msg,
            format!(
                "this runtime ({}) has no node type `tatolab.stream:NewThing`",
                env!("CARGO_PKG_VERSION")
            )
        );
    }

    /// These two words cross the helper-process boundary: the engine writes
    /// one into the `unwire_link` command and the child branches on it to pick
    /// which of its own ports to release. A drift here is silent on both sides
    /// — the child logs an unknown direction and the port stays leaked — so
    /// the spelling is pinned literally rather than derived from the variant.
    #[test]
    fn port_direction_wire_spelling_is_pinned() {
        assert_eq!(PortDirection::Input.as_wire_str(), "input");
        assert_eq!(PortDirection::Output.as_wire_str(), "output");
        // Display is the same words, so a log and a frame never disagree.
        assert_eq!(PortDirection::Input.to_string(), "input");
        assert_eq!(PortDirection::Output.to_string(), "output");
    }

    #[test]
    fn a_frame_allocation_refused_past_a_heap_budget_reads_its_heap_budget_usage_and_pool_bytes() {
        let msg = Error::FrameAllocationRefusedPastTheDeviceMemoryHeapBudget {
            heap_index: 1,
            heap_locality: DeviceMemoryHeapLocality::NotDeviceLocal,
            heap_budget_bytes: 8_000_000_000,
            heap_usage_bytes: 7_990_000_000,
            bytes_this_runtimes_allocator_pools_hold_on_the_heap: 1_200_000_000,
            requested_bytes: 3_686_400,
            headroom_bytes: 67_108_864,
            heap_budget_source:
                DeviceMemoryHeapBudgetSource::EstimatedAndCappedByTheSystemsAvailableMemory,
            frame_allocation_site: "HostVulkanBuffer::new".to_string(),
        }
        .to_string();
        assert_eq!(
            msg,
            "HostVulkanBuffer::new refused a 3686400-byte frame allocation before allocating: \
             device memory heap 1 (not device-local) has a budget of 8000000000 bytes (estimated \
             as 80% of the heap, the driver reporting no budget, and capped by the system's \
             available memory) and uses 7990000000 bytes, of which this runtime's allocator pools \
             hold 1200000000 bytes; the allocation would leave less than the 67108864-byte headroom"
        );
    }

    #[test]
    fn a_refused_one_off_acquisition_reads_its_shape_and_the_count() {
        let msg =
            Error::EveryOneOffAcquisitionOfThisShapeTheNodeMayHoldIsHeld {
                acquisition_shape:
                    "1280x720 rgba8_unorm texture (usage copy_src|copy_dst|texture_binding)"
                        .to_string(),
                one_off_acquisitions_one_node_may_hold_of_one_shape: 16,
            }
            .to_string();
        assert_eq!(
            msg,
            "this node already holds 16 one-off acquisitions of 1280x720 rgba8_unorm texture \
             (usage copy_src|copy_dst|texture_binding), the most one node may hold of one shape at \
             once, so this one is refused before anything is allocated; release one it holds, or \
             publish per-frame output from a node output pool"
        );
    }

    #[test]
    fn a_frame_allocation_refused_near_the_allocation_limit_reads_the_count_and_the_limit() {
        let msg = Error::FrameAllocationRefusedNearTheDeviceMemoryAllocationCountLimit {
            live_device_memory_allocation_count: 3_970,
            max_memory_allocation_count: 4_096,
            allocations_kept_in_reserve: 128,
            frame_allocation_site: "HostVulkanTexture::new".to_string(),
        }
        .to_string();
        assert_eq!(
            msg,
            "HostVulkanTexture::new refused a frame allocation before allocating: this process \
             holds 3970 device memory allocations of the 4096 the device allows, fewer than 128 \
             short of its limit"
        );
    }

    #[test]
    fn a_frame_allocation_refused_past_the_metal_working_set_reads_the_working_set_and_its_usage() {
        let msg = Error::FrameAllocationRefusedPastTheMetalWorkingSetBudget {
            recommended_max_working_set_bytes: 11_453_251_584,
            metal_current_allocated_bytes: 11_400_000_000,
            requested_bytes: 4_194_304,
            headroom_bytes: 67_108_864,
            frame_allocation_site: "HostVulkanTexture::new_iosurface_backed".to_string(),
        }
        .to_string();
        assert_eq!(
            msg,
            "HostVulkanTexture::new_iosurface_backed refused a 4194304-byte frame allocation \
             before allocating: Metal's recommended working set is 11453251584 bytes and the \
             Metal device has allocated 11400000000 bytes; the allocation would leave less than \
             the 67108864-byte headroom"
        );
    }

    #[test]
    fn a_device_memory_heap_reads_its_locality_and_where_its_budget_came_from() {
        assert_eq!(
            DeviceMemoryHeapLocality::DeviceLocal.to_string(),
            "device-local"
        );
        assert_eq!(
            DeviceMemoryHeapLocality::NotDeviceLocal.to_string(),
            "not device-local"
        );
        assert_eq!(
            DeviceMemoryHeapBudgetSource::ReportedByTheDriver.to_string(),
            "reported by the driver"
        );
        assert_eq!(
            DeviceMemoryHeapBudgetSource::EstimatedAsEightyPercentOfTheHeap.to_string(),
            "estimated as 80% of the heap, the driver reporting no budget"
        );
    }
}
