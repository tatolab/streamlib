// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A stream's condition as `list_streams` and `graph` answer it, each part apart: whether its
//! graph is loaded, whether it plays, and whether it is healthy; and each problem a stream or a
//! node has, as a typed diagnostic naming its kind, where it applies, the runtime's words and the
//! recovery that applies.

use serde::{Deserialize, Serialize};

/// A stream's load, playback and health, and every diagnostic that explains its health.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct StreamConditionOnTheWire {
    /// Whether its graph is loaded in the runtime.
    pub load: StreamLoadOnTheWire,
    /// Whether it plays.
    pub playback: StreamPlaybackOnTheWire,
    /// Whether it is healthy.
    pub health: StreamHealthOnTheWire,
    /// Each problem behind its health: the stream's own, then each of its nodes'. Empty while it
    /// is `healthy`.
    pub diagnostics: Vec<DiagnosticOnTheWire>,
}

impl StreamConditionOnTheWire {
    /// The condition of a healthy stream with `load` and `playback`.
    pub fn healthy(load: StreamLoadOnTheWire, playback: StreamPlaybackOnTheWire) -> Self {
        Self {
            load,
            playback,
            health: StreamHealthOnTheWire::Healthy,
            diagnostics: Vec::new(),
        }
    }
}

/// Whether a stream's graph is loaded in the runtime. The set grows: a value a client does not
/// know reads as `not_loaded`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "json-schema", schemars(extend("x-may-grow" = true)))]
#[serde(rename_all = "snake_case")]
pub enum StreamLoadOnTheWire {
    /// A stream action of its project loads it, or loads it again.
    Loading,
    /// Loaded: its graph is held in the runtime to read and edit, whatever its playback and
    /// health. A failed stream that is loaded holds its graph from its document.
    Loaded,
    /// A stream action of its project unloads it.
    Unloading,
    /// Not loaded: its project is closed, or it was never loaded or did not load. `graph` reads
    /// it from its saved document.
    // `#[serde(other)]` holds only on the last variant, so a new value goes above this one.
    #[serde(other)]
    NotLoaded,
}

/// Whether a stream plays. The set grows: a value a client does not know reads as `stopped`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "json-schema", schemars(extend("x-may-grow" = true)))]
#[serde(rename_all = "snake_case")]
pub enum StreamPlaybackOnTheWire {
    /// A stream action of its project starts its playback.
    Starting,
    /// Playing; or recorded playing, and played again once it loads at the runtime's start.
    Playing,
    /// Paused: its nodes process nothing until `resume_playback`.
    Paused,
    /// A stream action of its project stops its playback.
    Stopping,
    /// Stopped: its nodes run nothing until `start_playback`.
    // `#[serde(other)]` holds only on the last variant, so a new value goes above this one.
    #[serde(other)]
    Stopped,
}

/// Whether a stream is healthy. The set grows: a value a client does not know reads as
/// `failed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "json-schema", schemars(extend("x-may-grow" = true)))]
#[serde(rename_all = "snake_case")]
pub enum StreamHealthOnTheWire {
    /// Nothing is wrong with it.
    Healthy,
    /// A node of it failed, keeps raising or is not ready, or its graph document holds an edit its
    /// running graph did not take, whatever its playback.
    Degraded,
    /// It cannot play until the recovery its diagnostic names is made.
    // `#[serde(other)]` holds only on the last variant, so a new value goes above this one.
    #[serde(other)]
    Failed,
}

/// Whether a node is healthy, as `graph` answers it. The set grows: a value a client does not
/// know reads as `failed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "json-schema", schemars(extend("x-may-grow" = true)))]
#[serde(rename_all = "snake_case")]
pub enum NodeHealthOnTheWire {
    /// Nothing is wrong with it.
    Healthy,
    /// It runs, and says it is not ready yet.
    NotReady,
    /// It runs, and its `process()` or function body keeps raising.
    Raising,
    /// It failed, or runs nothing.
    // `#[serde(other)]` holds only on the last variant, so a new value goes above this one.
    #[serde(other)]
    Failed,
}

/// One problem a stream or a node has.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct DiagnosticOnTheWire {
    /// What the problem is.
    pub kind: DiagnosticKindOnTheWire,
    /// Where it applies.
    pub place: DiagnosticPlaceOnTheWire,
    /// The runtime's words for it, never rewritten.
    pub words: String,
    /// What recovers from it.
    pub recovery: DiagnosticRecoveryOnTheWire,
}

impl DiagnosticOnTheWire {
    /// A diagnostic of `kind` at `place` in the runtime's `words`, with the recovery its kind
    /// takes.
    pub fn of(
        kind: DiagnosticKindOnTheWire,
        place: DiagnosticPlaceOnTheWire,
        words: String,
    ) -> Self {
        Self {
            kind,
            place,
            words,
            recovery: kind.recovery(),
        }
    }
}

/// What a problem is. The set grows: a kind a client does not know reads as `other`, shown by
/// its `words` and recovered by its `recovery`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "json-schema", schemars(extend("x-may-grow" = true)))]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticKindOnTheWire {
    /// The project's venv could not be made from its `pyproject.toml`.
    ProjectVenvNotMade,
    /// The stream's code gave no graph, or its load was refused.
    LoadRefused,
    /// The stream loaded, and its start was refused.
    StartRefused,
    /// A node's type is not one its project's node code describes, so the node is held as its
    /// document writes it and runs nothing.
    NodeTypeNotDescribed,
    /// A link the graph's validation refused is held as drawn and never wired.
    LinkRefused,
    /// The stream was implicated in the runtime's crashes too many times in a row.
    KeepsCrashingTheRuntime,
    /// An earlier runtime recorded the stream in a form this one does not load.
    RecordedByAnEarlierRuntime,
    /// A node failed.
    NodeFailed,
    /// A node's `process()` or function body keeps raising.
    NodeRaising,
    /// A running node says it is not ready yet.
    NodeNotReady,
    /// An edit the stream's graph document holds that its running graph did not take: `graph`
    /// shows the document's graph, each part the running graph does not hold yet carrying
    /// `live: false`, and the stream takes the edit when its playback stops.
    EditNotLive,
    /// A link the running graph holds that could not be wired, so its reader reads nothing
    /// from it: the words say why, in the words of the end that refused it.
    LinkNotWired,
    /// A problem of a kind this client does not know; the runtime never sends it. Its `words`
    /// say what it is, and its `recovery` what recovers from it.
    // `#[serde(other)]` holds only on the last variant, so a new kind goes above this one.
    #[serde(other)]
    Other,
}

impl DiagnosticKindOnTheWire {
    /// The recovery a problem of this kind takes.
    pub const fn recovery(self) -> DiagnosticRecoveryOnTheWire {
        match self {
            Self::ProjectVenvNotMade => DiagnosticRecoveryOnTheWire::FixTheDependencies,
            Self::LoadRefused | Self::NodeTypeNotDescribed | Self::NodeRaising => {
                DiagnosticRecoveryOnTheWire::FixTheCode
            }
            Self::StartRefused | Self::NodeFailed | Self::EditNotLive | Self::LinkNotWired => {
                DiagnosticRecoveryOnTheWire::RestartThePlayback
            }
            Self::LinkRefused => DiagnosticRecoveryOnTheWire::FixTheLink,
            Self::KeepsCrashingTheRuntime | Self::Other => {
                DiagnosticRecoveryOnTheWire::RetryTheLoad
            }
            Self::RecordedByAnEarlierRuntime => DiagnosticRecoveryOnTheWire::RemoveTheStream,
            Self::NodeNotReady => DiagnosticRecoveryOnTheWire::WaitForTheNode,
        }
    }
}

/// What recovers from a problem. The set grows: a recovery a client does not know reads as
/// `retry_the_load`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "json-schema", schemars(extend("x-may-grow" = true)))]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticRecoveryOnTheWire {
    /// Fix the code the words name; a saved node file reloads it, and `reload_project` retries
    /// the stream.
    FixTheCode,
    /// Fix the project's dependencies in its `pyproject.toml`, then `reload_project`.
    FixTheDependencies,
    /// Remove or reconnect the link the place names.
    FixTheLink,
    /// Stop the stream's playback and start it again.
    RestartThePlayback,
    /// Forget the stream with `remove_project` of the project directory its record names; this
    /// runtime never loads it. A record that names no project directory is not reached that way.
    RemoveTheStream,
    /// Nothing: the node reports itself ready once it is.
    WaitForTheNode,
    /// Load the stream again with `reload_project` of its project; a stream whose graph is
    /// loaded is retried by `start_playback` too.
    // `#[serde(other)]` holds only on the last variant, so a new recovery goes above this one.
    #[serde(other)]
    RetryTheLoad,
}

/// Where a problem applies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(tag = "on", rename_all = "snake_case")]
pub enum DiagnosticPlaceOnTheWire {
    /// The stream as a whole.
    Stream,
    /// One node of the stream.
    Node {
        /// The node's id, as `graph` lists it.
        node_id: String,
        /// The node's name: its label, not its identity.
        node_name: String,
    },
    /// One link of the stream's graph, by the node names and ports its document draws it
    /// between.
    Link {
        /// The name of the node the link leaves.
        from_node: String,
        /// The output port the link leaves.
        from_port: String,
        /// The name of the node the link enters.
        to_node: String,
        /// The input port the link enters.
        to_port: String,
    },
}
