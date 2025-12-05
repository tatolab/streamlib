// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! LinkInputDataReader - Weak reference for reading from a LinkInstance.

use std::sync::Weak;

use super::link_instance::LinkInstanceInner;
use crate::core::links::graph::LinkId;
use crate::core::links::traits::LinkPortMessage;

/// Reads data from a LinkInstance for a LinkInput port.
///
/// Uses `Weak` reference so LinkInstance can be dropped independently.
/// When the LinkInstance is dropped, reads return None (graceful degradation).
pub struct LinkInputDataReader<T: LinkPortMessage> {
    inner: Weak<LinkInstanceInner<T>>,
}

impl<T: LinkPortMessage> LinkInputDataReader<T> {
    pub(super) fn new(inner: Weak<LinkInstanceInner<T>>) -> Self {
        Self { inner }
    }

    /// Read from the link using the frame type's consumption strategy.
    ///
    /// Returns `None` if:
    /// - LinkInstance was dropped (graceful degradation)
    /// - No data available
    pub fn read(&self) -> Option<T> {
        self.inner.upgrade().and_then(|inner| inner.read())
    }

    /// Check if the LinkInstance is still alive.
    #[inline]
    pub fn is_connected(&self) -> bool {
        self.inner.strong_count() > 0
    }

    /// Check if data is available (without reading).
    pub fn has_data(&self) -> bool {
        self.inner
            .upgrade()
            .map(|inner| inner.has_data())
            .unwrap_or(false)
    }

    /// Get the link ID if still connected.
    pub fn link_id(&self) -> Option<LinkId> {
        self.inner.upgrade().map(|inner| inner.link_id().clone())
    }
}

impl<T: LinkPortMessage> Clone for LinkInputDataReader<T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}
