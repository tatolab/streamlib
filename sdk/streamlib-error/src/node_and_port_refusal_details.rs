// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! What a refusal names about a node or a port: the reader an output port
//! refused, why a node is not running, and a port's direction.

use streamlib_processor_schema::ProcessorClassImportPath;

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

/// Why a node of a stream does not run, as a refusal names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WhyANodeOfAStreamIsNotRunning {
    /// Its project's playback is stopped.
    PlaybackStopped,
    /// Its project's playback is paused, and the node was added since.
    PlaybackPaused,
    /// Its stream is starting, or a compile has not reached the node yet.
    Preparing,
    /// The node failed.
    NodeFailed,
}

impl WhyANodeOfAStreamIsNotRunning {
    /// The name the local API's wire spells it with.
    pub fn wire_name(self) -> &'static str {
        match self {
            Self::PlaybackStopped => "playback_stopped",
            Self::PlaybackPaused => "playback_paused",
            Self::Preparing => "preparing",
            Self::NodeFailed => "node_failed",
        }
    }
}

/// Direction of a port relative to its processor — `Output` for source-side,
/// `Input` for destination-side. Used by
/// [`Error::ProcessorPortNotFound`](crate::Error::ProcessorPortNotFound) to
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

/// Node types as a refusal names them: each in backticks, comma-separated.
pub(crate) fn node_types_listed_for_a_refusal(node_types: &[ProcessorClassImportPath]) -> String {
    node_types
        .iter()
        .map(|node_type| format!("`{node_type}`"))
        .collect::<Vec<_>>()
        .join(", ")
}
