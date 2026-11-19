// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The input value changes a connect waits for while it judges and makes its link.

use std::sync::Arc;

use super::{ALinkConnected, LiveGraphChangeOnOneStream, LoadedStreamInThisRuntime, connect_impl};
use crate::core::graph::LinkUniqueId;
use crate::core::{InputLinkPortRef, OutputLinkPortRef, Result};

/// The input value changes a connect waits for, so a type choice a set puts back never meets a
/// link made under the choice it replaced.
pub(in crate::core::runtime) enum InputValueChangesAConnectWaitsFor {
    /// The stream's one input value change at a time, which the connect holds while it judges
    /// and makes its link.
    TheStreamsOne(Arc<parking_lot::Mutex<()>>),
    /// None: the connect's caller already holds the stream's one input value change at a time.
    NoneItsCallerHoldsThem,
}

impl InputValueChangesAConnectWaitsFor {
    /// Wait for any input value change being made, and hold off the next until the answer drops.
    pub(in crate::core::runtime) fn held_while_a_link_is_made(
        &self,
    ) -> Option<parking_lot::MutexGuard<'_, ()>> {
        match self {
            Self::TheStreamsOne(one_at_a_time) => Some(one_at_a_time.lock()),
            Self::NoneItsCallerHoldsThem => None,
        }
    }
}

impl Default for InputValueChangesAConnectWaitsFor {
    fn default() -> Self {
        Self::TheStreamsOne(Arc::default())
    }
}

impl LoadedStreamInThisRuntime {
    /// Connect a link `from` to `to` made now, as `connect` does, from code that already holds
    /// this stream's one input value change at a time.
    pub(in crate::core::runtime) fn connect_while_no_input_value_changes(
        &self,
        from: OutputLinkPortRef,
        to: InputLinkPortRef,
    ) -> Result<LinkUniqueId> {
        self.block_on_the_engines_tokio_runtime(connect_impl(
            LiveGraphChangeOnOneStream {
                one_input_value_change_at_a_time:
                    InputValueChangesAConnectWaitsFor::NoneItsCallerHoldsThem,
                ..self.live_graph_change()
            },
            from,
            to,
            ALinkConnected::MadeNow,
        ))
    }
}
