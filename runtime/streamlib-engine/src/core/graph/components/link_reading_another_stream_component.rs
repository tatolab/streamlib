// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

/// What a link reading another stream on this machine holds beside its state:
/// why it carries nothing, read by `graph` as the link's
/// `awaiting_remote_reason` or `error_reason`.
///
/// Stored but never rendered under the link's `components`: `graph` renders
/// its reason at the top level of the link.
#[derive(Debug, Default)]
pub struct LinkReadingAnotherStreamComponent {
    why_it_carries_nothing: Option<String>,
}

impl LinkReadingAnotherStreamComponent {
    /// A link that carries nothing for `reason`.
    pub fn carrying_nothing_because(reason: String) -> Self {
        Self {
            why_it_carries_nothing: Some(reason),
        }
    }

    /// Why the link carries nothing, when it says.
    pub fn why_it_carries_nothing(&self) -> Option<&str> {
        self.why_it_carries_nothing.as_deref()
    }
}
