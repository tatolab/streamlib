// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The refusal of a reach for a node of a stream that does not run, naming why.

use super::LoadedStreamInThisRuntime;
use crate::core::error::{Error, WhyANodeOfAStreamIsNotRunning};
use crate::core::graph::{GraphNodeWithComponents, ProcessorNode, ProcessorState, StateComponent};
use crate::core::runtime::RuntimeStatus;

impl LoadedStreamInThisRuntime {
    /// Why a node of this stream that does not run, and has not failed, does not run now.
    pub(crate) fn why_a_node_of_this_stream_that_has_not_failed_does_not_run(
        &self,
    ) -> WhyANodeOfAStreamIsNotRunning {
        if self.is_held_stopped() {
            WhyANodeOfAStreamIsNotRunning::PlaybackStopped
        } else if matches!(
            self.status(),
            RuntimeStatus::Paused | RuntimeStatus::Pausing
        ) {
            WhyANodeOfAStreamIsNotRunning::PlaybackPaused
        } else {
            WhyANodeOfAStreamIsNotRunning::Preparing
        }
    }
}

/// The refusal of a reach for `node`, which does not run: `node_failed` when it failed, and
/// `why_short_of_its_own_failure` otherwise, `runtime_words` led by what was refused.
pub(crate) fn the_refusal_of_a_reach_for_a_node_not_running(
    node: &ProcessorNode,
    why_short_of_its_own_failure: WhyANodeOfAStreamIsNotRunning,
    runtime_words: String,
) -> Error {
    let not_running_because = if node
        .get::<StateComponent>()
        .is_some_and(|state| state.current() == ProcessorState::Error)
    {
        WhyANodeOfAStreamIsNotRunning::NodeFailed
    } else {
        why_short_of_its_own_failure
    };
    Error::NodeOfAStreamIsNotRunning {
        not_running_because,
        runtime_words: format!(
            "{runtime_words}: {}",
            what_a_node_not_running_waits_on(not_running_because)
        ),
    }
}

/// What a node that does not run for `not_running_because` waits on, in a refusal's words.
fn what_a_node_not_running_waits_on(
    not_running_because: WhyANodeOfAStreamIsNotRunning,
) -> &'static str {
    match not_running_because {
        WhyANodeOfAStreamIsNotRunning::PlaybackStopped => {
            "its project's playback is stopped, and `start_playback` plays it"
        }
        WhyANodeOfAStreamIsNotRunning::PlaybackPaused => {
            "its project is paused, and `resume_playback` starts the nodes added meanwhile"
        }
        WhyANodeOfAStreamIsNotRunning::Preparing => {
            "its stream is starting, or has not compiled the node yet"
        }
        WhyANodeOfAStreamIsNotRunning::NodeFailed => {
            "it failed, and `graph` carries why in its `components.failed_because`"
        }
    }
}
