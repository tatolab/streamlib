// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Why a stream cannot play, typed by what it is and where it applies, and a stream's condition
//! as a listing answers it: its load, playback and health apart, with each diagnostic behind its
//! health.

use std::fmt;

use streamlib_runtime_client_contract::local_api_wire_contract::{
    DiagnosticKindOnTheWire, DiagnosticOnTheWire, DiagnosticPlaceOnTheWire,
    StreamConditionOnTheWire, StreamHealthOnTheWire, StreamLoadOnTheWire, StreamPlaybackOnTheWire,
};

use super::machine_state_directory::StreamFailureKind;
use super::project_stream_action_queue::StreamActionKind;

impl StreamFailureKind {
    /// The diagnostic kind a failure of this kind is answered as.
    pub fn on_the_wire(self) -> DiagnosticKindOnTheWire {
        match self {
            Self::ProjectVenvNotMade => DiagnosticKindOnTheWire::ProjectVenvNotMade,
            Self::LoadRefused => DiagnosticKindOnTheWire::LoadRefused,
            Self::StartRefused => DiagnosticKindOnTheWire::StartRefused,
            Self::NodeTypeNotDescribed => DiagnosticKindOnTheWire::NodeTypeNotDescribed,
            Self::LinkRefused => DiagnosticKindOnTheWire::LinkRefused,
            Self::KeepsCrashingTheRuntime => DiagnosticKindOnTheWire::KeepsCrashingTheRuntime,
        }
    }
}

/// Why a stream cannot play: what its failure is, where it applies, in the runtime's words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamFailure {
    /// What the failure is.
    pub kind: StreamFailureKind,
    /// Where it applies.
    pub place: DiagnosticPlaceOnTheWire,
    /// The runtime's words for it.
    pub words: String,
}

impl StreamFailure {
    /// A failure of `kind` of the stream as a whole, in `words`.
    pub fn of_the_stream(kind: StreamFailureKind, words: String) -> Self {
        Self {
            kind,
            place: DiagnosticPlaceOnTheWire::Stream,
            words,
        }
    }

    /// The failure as a diagnostic answers it.
    pub fn diagnostic(&self) -> DiagnosticOnTheWire {
        DiagnosticOnTheWire::of(
            self.kind.on_the_wire(),
            self.place.clone(),
            self.words.clone(),
        )
    }
}

impl fmt::Display for StreamFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.words)
    }
}

/// The condition of a stream with `load` and `playback`: failed for `failure` when it has one,
/// its diagnostic first, else degraded while any of `node_diagnostics` holds, else healthy. A
/// failure a node already carries as its own diagnostic is answered by that node's alone.
pub(crate) fn the_condition_of_a_stream(
    load: StreamLoadOnTheWire,
    playback: StreamPlaybackOnTheWire,
    failure: Option<DiagnosticOnTheWire>,
    node_diagnostics: Vec<DiagnosticOnTheWire>,
) -> StreamConditionOnTheWire {
    let health = if failure.is_some() {
        StreamHealthOnTheWire::Failed
    } else if node_diagnostics.is_empty() {
        StreamHealthOnTheWire::Healthy
    } else {
        StreamHealthOnTheWire::Degraded
    };
    let failure_a_node_carries = |failure: &DiagnosticOnTheWire| {
        node_diagnostics
            .iter()
            .any(|of_a_node| of_a_node.kind == failure.kind && of_a_node.place == failure.place)
    };
    let diagnostics = failure
        .filter(|failure| !failure_a_node_carries(failure))
        .into_iter()
        .chain(node_diagnostics)
        .collect();
    StreamConditionOnTheWire {
        load,
        playback,
        health,
        diagnostics,
    }
}

/// The condition of a stream whose `condition` was `as_listed` while a stream action of `kind`
/// works on it: loading or unloading, starting or stopping, as the action does, with no
/// diagnostic, since the action retries or ends what failed.
pub(crate) fn the_condition_while_an_action_works_on_it(
    kind: StreamActionKind,
    as_listed: &StreamConditionOnTheWire,
) -> StreamConditionOnTheWire {
    let loaded = as_listed.load == StreamLoadOnTheWire::Loaded;
    let plays = matches!(
        as_listed.playback,
        StreamPlaybackOnTheWire::Playing
            | StreamPlaybackOnTheWire::Paused
            | StreamPlaybackOnTheWire::Starting
    );
    let (load, playback) = match kind {
        StreamActionKind::Open | StreamActionKind::Load | StreamActionKind::ReloadAtStart => {
            (StreamLoadOnTheWire::Loading, as_listed.playback)
        }
        StreamActionKind::StartPlayback => (
            if loaded {
                StreamLoadOnTheWire::Loaded
            } else {
                StreamLoadOnTheWire::Loading
            },
            StreamPlaybackOnTheWire::Starting,
        ),
        StreamActionKind::Stop => (as_listed.load, StreamPlaybackOnTheWire::Stopping),
        StreamActionKind::Close | StreamActionKind::Remove | StreamActionKind::Delete => (
            if loaded {
                StreamLoadOnTheWire::Unloading
            } else {
                as_listed.load
            },
            if plays {
                StreamPlaybackOnTheWire::Stopping
            } else {
                as_listed.playback
            },
        ),
        StreamActionKind::MakeTheVenvCurrent => (as_listed.load, as_listed.playback),
    };
    StreamConditionOnTheWire {
        load,
        playback,
        health: StreamHealthOnTheWire::Healthy,
        diagnostics: Vec::new(),
    }
}

#[cfg(test)]
mod tests;
