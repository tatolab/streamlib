// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! LinkOutputDataWriter - Weak reference for writing to a LinkInstance.

use std::sync::Weak;

use super::link_instance::LinkInstanceInner;
use crate::core::links::graph::LinkId;
use crate::core::links::traits::LinkPortMessage;

/// Writes data to a LinkInstance from a LinkOutput port.
///
/// Uses `Weak` reference so LinkInstance can be dropped independently.
/// When the LinkInstance is dropped, writes silently drop data (graceful degradation).
pub struct LinkOutputDataWriter<T: LinkPortMessage> {
    inner: Weak<LinkInstanceInner<T>>,
}

impl<T: LinkPortMessage> LinkOutputDataWriter<T> {
    pub(super) fn new(inner: Weak<LinkInstanceInner<T>>) -> Self {
        Self { inner }
    }

    /// Write to the link.
    ///
    /// Returns `true` if written successfully, `false` if:
    /// - LinkInstance was dropped (graceful degradation)
    /// - Buffer is full (data dropped)
    pub fn write(&self, value: T) -> bool {
        if let Some(inner) = self.inner.upgrade() {
            inner.push(value)
        } else {
            false
        }
    }

    /// Check if the LinkInstance is still alive.
    #[inline]
    pub fn is_connected(&self) -> bool {
        self.inner.strong_count() > 0
    }

    /// Get the link ID if still connected.
    pub fn link_id(&self) -> Option<LinkId> {
        self.inner.upgrade().map(|inner| inner.link_id().clone())
    }
}

impl<T: LinkPortMessage> Clone for LinkOutputDataWriter<T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}
