// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! How far a link whose source is on another runtime has got.
//!
//! `connect` never waits on the mesh: it returns with the link waiting, and the
//! outcome lands here afterwards — when the source runtime appears, when it
//! says which ports it offers, and when it leaves again. The mesh writes this
//! cell on its own thread and `graph` reads it under no graph lock, the shape
//! [`OutOfProcessLinkWireRepliesComponent`] already uses for an answer that
//! arrives after the wiring op returned.
//!
//! [`OutOfProcessLinkWireRepliesComponent`]: super::OutOfProcessLinkWireRepliesComponent

use std::sync::Arc;

use parking_lot::Mutex;

/// Where a link whose source is on another runtime has got to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteLinkResolution {
    /// Nothing is carrying yet, and the reason names what is missing — the
    /// runtime, or the port on a runtime that is here.
    AwaitingRemote {
        /// Why nothing carries yet, in terms a reader can act on.
        reason: String,
    },
    /// The ingress is open and the link carries.
    Wired,
    /// The link cannot be made, and will not be retried. The reason is the
    /// refusing runtime's own words.
    Refused {
        /// Why the link was refused.
        reason: String,
    },
}

/// The resolution cell one link whose source is on another runtime carries.
///
/// Cloned rather than borrowed: the mesh holds its own handle on the cell for
/// the link's life, so a resolution never needs the graph lock to land.
pub struct RemoteLinkResolutionComponent(pub Arc<Mutex<RemoteLinkResolution>>);

impl RemoteLinkResolutionComponent {
    /// A link that has just been connected and is waiting on `reason`.
    pub fn awaiting_remote(reason: impl Into<String>) -> Self {
        Self(Arc::new(Mutex::new(RemoteLinkResolution::AwaitingRemote {
            reason: reason.into(),
        })))
    }

    /// The mesh's own handle on this cell.
    pub fn its_cell(&self) -> Arc<Mutex<RemoteLinkResolution>> {
        Arc::clone(&self.0)
    }

    /// Where the link has got to right now.
    pub fn how_far_it_has_got(&self) -> RemoteLinkResolution {
        self.0.lock().clone()
    }
}
