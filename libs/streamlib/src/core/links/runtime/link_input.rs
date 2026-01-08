// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! LinkInput - Input port for a processor.

use std::cell::UnsafeCell;
use std::sync::Arc;
use std::time::Duration;

use super::link_input_data_reader::LinkInputDataReader;
use crate::core::graph::LinkUniqueId;
use crate::core::links::traits::{LinkPortAddress, LinkPortMessage};
use crate::core::{Result, StreamError};

/// Binding between a LinkInput port and an upstream processor via a specific link.
struct LinkInputFromUpstreamProcessor<T: LinkPortMessage> {
    link_id: LinkUniqueId,
    data_reader: LinkInputDataReader<T>,
    #[allow(dead_code)]
    source_address: Option<LinkPortAddress>,
}

impl<T: LinkPortMessage> LinkInputFromUpstreamProcessor<T> {
    fn new(
        link_id: LinkUniqueId,
        data_reader: LinkInputDataReader<T>,
        source_address: Option<LinkPortAddress>,
    ) -> Self {
        Self {
            link_id,
            data_reader,
            source_address,
        }
    }

    fn read(&self) -> Option<T> {
        self.data_reader.read()
    }

    fn wait_read(&self, timeout: Duration) -> Option<T> {
        self.data_reader.wait_read(timeout)
    }

    fn is_connected(&self) -> bool {
        self.data_reader.is_connected()
    }
}

/// Inner state for LinkInput.
struct LinkInputInner<T: LinkPortMessage> {
    port_name: String,
    upstream_processors: UnsafeCell<Vec<LinkInputFromUpstreamProcessor<T>>>,
}

// SAFETY: LinkInputInner is only accessed by the owning processor thread.
unsafe impl<T: LinkPortMessage> Send for LinkInputInner<T> {}
unsafe impl<T: LinkPortMessage> Sync for LinkInputInner<T> {}

/// Input link port for a processor.
///
/// Currently supports a single connection (1-to-1 at input).
/// When no connection exists or connection is dead, reads return None.
pub struct LinkInput<T: LinkPortMessage> {
    inner: Arc<LinkInputInner<T>>,
}

impl<T: LinkPortMessage> Clone for LinkInput<T> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<T: LinkPortMessage> LinkInput<T> {
    /// Create a new input port.
    pub fn new(port_name: &str) -> Self {
        Self {
            inner: Arc::new(LinkInputInner {
                port_name: port_name.to_string(),
                upstream_processors: UnsafeCell::new(Vec::new()),
            }),
        }
    }

    #[inline]
    #[allow(clippy::mut_from_ref)] // UnsafeCell provides interior mutability
    unsafe fn upstream_processors_mut(&self) -> &mut Vec<LinkInputFromUpstreamProcessor<T>> {
        &mut *self.inner.upstream_processors.get()
    }

    #[inline]
    fn upstream_processors(&self) -> &Vec<LinkInputFromUpstreamProcessor<T>> {
        unsafe { &*self.inner.upstream_processors.get() }
    }

    /// Add a data reader from an upstream processor.
    pub fn add_data_reader(
        &self,
        link_id: LinkUniqueId,
        data_reader: LinkInputDataReader<T>,
        source_address: Option<LinkPortAddress>,
    ) -> Result<()> {
        unsafe {
            let upstream = self.upstream_processors_mut();

            if upstream.iter().any(|u| u.link_id == link_id) {
                return Err(StreamError::LinkAlreadyExists(link_id.to_string()));
            }

            upstream.push(LinkInputFromUpstreamProcessor::new(
                link_id,
                data_reader,
                source_address,
            ));
        }
        Ok(())
    }

    /// Remove a data reader by link ID.
    pub fn remove_data_reader(&self, link_id: &LinkUniqueId) -> Result<()> {
        unsafe {
            let upstream = self.upstream_processors_mut();
            let idx = upstream
                .iter()
                .position(|u| &u.link_id == link_id)
                .ok_or_else(|| StreamError::LinkNotFound(link_id.to_string()))?;
            upstream.swap_remove(idx);
        }
        Ok(())
    }

    /// Read from input using the consumption strategy defined by the frame type.
    pub fn read(&self) -> Option<T> {
        unsafe {
            let upstream = self.upstream_processors_mut();
            upstream.retain(|u| u.is_connected());
            upstream.first().and_then(|u| u.read())
        }
    }

    /// Blocking read with timeout.
    ///
    /// First attempts a non-blocking read. If no data is available, waits up to
    /// `timeout` for data to arrive. Returns `None` if timeout expires without data.
    ///
    /// Use at sync points (e.g., display processors) where waiting for the next
    /// frame is preferable to busy-polling or sleeping. Most realtime processors
    /// should use non-blocking `read()` instead.
    pub fn wait_read(&self, timeout: Duration) -> Option<T> {
        unsafe {
            let upstream = self.upstream_processors_mut();
            upstream.retain(|u| u.is_connected());
            upstream.first().and_then(|u| u.wait_read(timeout))
        }
    }

    /// Peek at next item without consuming it.
    pub fn peek(&self) -> Option<T> {
        None
    }

    /// Check if port has any live upstream processors.
    pub fn is_connected(&self) -> bool {
        self.upstream_processors().iter().any(|u| u.is_connected())
    }

    /// Get count of live upstream processors.
    pub fn link_count(&self) -> usize {
        self.upstream_processors()
            .iter()
            .filter(|u| u.is_connected())
            .count()
    }

    /// Get the port name.
    pub fn port_name(&self) -> &str {
        &self.inner.port_name
    }

    /// Check if data is available.
    pub fn has_data(&self) -> bool {
        self.upstream_processors()
            .first()
            .map(|u| u.data_reader.has_data())
            .unwrap_or(false)
    }
}
