// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Canonical StreamLib [`Error`] + [`Result`].
//!
//! Shared by `streamlib-engine` (which re-exports it at
//! `core::error`) and the engine-free authoring surface. Every variant
//! is String / std / anyhow based plus the engine-free
//! `ProcessorClassImportPath` (from `streamlib-processor-schema`) and, on
//! Linux, the engine-free `ConsumerRhiError` conversion.

use streamlib_processor_schema::ProcessorClassImportPath;

/// The StreamLib error type.
#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("GPU operation failed: {0}")]
    GpuError(String),

    #[error("No display surface available: {0}")]
    DisplaySurfaceUnavailable(String),

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

    #[error("channel name is empty")]
    EmptyChannelName,

    #[error(
        "channel `{name}` contains invalid character `{character}` (allowed: a-z, 0-9, hyphen, underscore, must start with a-z)"
    )]
    InvalidChannelNameCharacter { name: String, character: char },

    #[error("channel `{0}` must start with a-z")]
    ChannelNameMustStartWithLowercase(String),

    #[error("channel `{name}` is {len} bytes, exceeding the {max}-byte wire capacity")]
    ChannelNameTooLong {
        name: String,
        len: usize,
        max: usize,
    },

    #[error("Invalid graph: {0}")]
    InvalidGraph(String),

    #[error("Processor not found: {0}")]
    ProcessorNotFound(String),

    #[error("Unknown processor type: {ident} (not registered)")]
    UnknownProcessorType { ident: ProcessorClassImportPath },

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
        "capability `{capability}` is already registered by `{already_registered_by}`, so \
         `{also_registered_by}` cannot register it too — uninstall one of the two \
         distributions"
    )]
    CapabilityExtensionNameAlreadyRegistered {
        capability: String,
        already_registered_by: String,
        also_registered_by: String,
    },

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
        "payload of {payload_bytes} bytes on channel '{channel}' exceeds the \
         per-channel ceiling of {ceiling_bytes} bytes ({tier} tier) — the sample \
         was refused and counted, the stream continues; raise the node's \
         max_payload_bytes_per_channel for this tier or split the payload"
    )]
    PayloadExceedsChannelCeiling {
        channel: String,
        payload_bytes: usize,
        ceiling_bytes: usize,
        tier: ChannelTrustTierLabel,
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
        assert!(msg.contains("not registered"), "message: {msg}");
        assert!(
            msg.contains("my_app.filters:BlurProcessor"),
            "the requested path must reach the user unaltered: {msg}"
        );
        assert!(
            !msg.contains("streamlib add"),
            "no install fix-it survives the module-system removal: {msg}"
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
}
