// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Why a stream cannot play, typed by what it is and where it applies, and a stream's condition
//! as a listing answers it: its load, playback and health apart, with each diagnostic behind its
//! health.

use std::fmt;

use streamlib_runtime_client_contract::local_api_wire_contract::{
    DiagnosticOnTheWire, DiagnosticPlaceOnTheWire, StreamConditionOnTheWire, StreamHealthOnTheWire,
    StreamLoadOnTheWire, StreamPlaybackOnTheWire,
};

use super::machine_state_directory::StreamFailureKind;
use super::project_stream_action_queue::StreamActionKind;

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
/// its diagnostic first, else degraded while any of `diagnostics_while_loaded`, each of its
/// nodes', holds, else healthy. A failure a node
/// already carries as its own diagnostic is answered by that node's alone.
pub(crate) fn the_condition_of_a_stream(
    load: StreamLoadOnTheWire,
    playback: StreamPlaybackOnTheWire,
    failure: Option<DiagnosticOnTheWire>,
    diagnostics_while_loaded: Vec<DiagnosticOnTheWire>,
) -> StreamConditionOnTheWire {
    let health = if failure.is_some() {
        StreamHealthOnTheWire::Failed
    } else if diagnostics_while_loaded.is_empty() {
        StreamHealthOnTheWire::Healthy
    } else {
        StreamHealthOnTheWire::Degraded
    };
    let failure_a_node_carries = |failure: &DiagnosticOnTheWire| {
        diagnostics_while_loaded
            .iter()
            .any(|of_a_node| of_a_node.kind == failure.kind && of_a_node.place == failure.place)
    };
    let diagnostics = failure
        .filter(|failure| !failure_a_node_carries(failure))
        .into_iter()
        .chain(diagnostics_while_loaded)
        .collect();
    StreamConditionOnTheWire {
        load,
        playback,
        health,
        diagnostics,
    }
}

/// The condition of a stream `listed` so while a stream action of `kind` works on it: loading
/// or unloading, starting or stopping, as the action does. An action that loads, starts or
/// unloads it retries or ends what failed, so it shows healthy; a stop keeps the listed health
/// and diagnostics.
pub(crate) fn the_condition_while_an_action_works_on_it(
    kind: StreamActionKind,
    listed: StreamConditionOnTheWire,
) -> StreamConditionOnTheWire {
    let loaded = listed.load == StreamLoadOnTheWire::Loaded;
    let plays = matches!(
        listed.playback,
        StreamPlaybackOnTheWire::Playing
            | StreamPlaybackOnTheWire::Paused
            | StreamPlaybackOnTheWire::Starting
    );
    match kind {
        StreamActionKind::Open | StreamActionKind::Load | StreamActionKind::ReloadAtStart => {
            StreamConditionOnTheWire::healthy(StreamLoadOnTheWire::Loading, listed.playback)
        }
        StreamActionKind::StartPlayback => StreamConditionOnTheWire::healthy(
            if loaded {
                StreamLoadOnTheWire::Loaded
            } else {
                StreamLoadOnTheWire::Loading
            },
            StreamPlaybackOnTheWire::Starting,
        ),
        StreamActionKind::Stop => StreamConditionOnTheWire {
            playback: StreamPlaybackOnTheWire::Stopping,
            ..listed
        },
        StreamActionKind::Close | StreamActionKind::Remove | StreamActionKind::Delete => {
            StreamConditionOnTheWire::healthy(
                if loaded {
                    StreamLoadOnTheWire::Unloading
                } else {
                    listed.load
                },
                if plays {
                    StreamPlaybackOnTheWire::Stopping
                } else {
                    listed.playback
                },
            )
        }
        StreamActionKind::MakeTheVenvCurrent => listed,
    }
}

#[cfg(test)]
mod tests;
