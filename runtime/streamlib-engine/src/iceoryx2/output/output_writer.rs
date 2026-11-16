// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::ffi::c_void;
use std::sync::Arc;

use serde::Serialize;

use super::OutputWriterInner;
use crate::core::descriptors::PortStreamFacts;
use crate::core::error::{Error, Result};
use crate::core::graph::cast_exposed_name_to_url_safe;
use crate::core::media_clock::MediaClock;
use crate::iceoryx2::{DataChannelPayloadDescriptor, MessageGroupBoundary};

// =============================================================================
// OutputWriter
// =============================================================================

pub struct OutputWriter {
    pub(crate) handle: *const c_void,
}

// SAFETY: `handle` points at an `Arc<OutputWriterInner>` whose
// interior is Send+Sync (OutputWriterInner declares both above).
unsafe impl Send for OutputWriter {}
unsafe impl Sync for OutputWriter {}

impl OutputWriter {
    pub fn from_inner_arc(inner: Arc<OutputWriterInner>) -> Self {
        let handle = Arc::into_raw(inner) as *const c_void;
        Self { handle }
    }

    /// Engine-internal borrow of the `OutputWriterInner`, or `None`
    /// when unwired.
    fn host_inner(&self) -> Option<&OutputWriterInner> {
        if self.handle.is_null() {
            return None;
        }
        // SAFETY: `handle` is `Arc::into_raw(Arc<OutputWriterInner>)`.
        Some(unsafe { &*(self.handle as *const OutputWriterInner) })
    }

    fn wired_inner_for_port(&self, port: &str) -> Result<&OutputWriterInner> {
        self.host_inner().ok_or_else(|| {
            Error::Link(format!(
                "OutputWriter not wired (port='{port}'): host has not yet installed iceoryx2 \
                 resources on this node instance"
            ))
        })
    }

    pub fn empty() -> Self {
        Self {
            handle: std::ptr::null(),
        }
    }

    /// Returns true iff this has been wired to a real inner.
    pub fn is_configured(&self) -> bool {
        !self.handle.is_null()
    }

    /// Borrow the `Arc<OutputWriterInner>` this handle points at. Returns
    /// `None` for unwired handles. Bumps the strong count; the returned
    /// Arc balances with one Drop on the inner.
    ///
    /// Engine-only (used by the macro-emitted
    /// `iceoryx2_output_writer_inner` trait method to expose the
    /// wiring path to compiler ops).
    pub fn inner_arc(&self) -> Option<Arc<OutputWriterInner>> {
        if !self.is_configured() {
            return None;
        }
        // SAFETY: `handle` is `Arc::into_raw(Arc<OutputWriterInner>)`; bump
        // the strong count and reconstruct an owning `Arc` from the raw handle.
        unsafe {
            Arc::increment_strong_count(self.handle as *const OutputWriterInner);
            Some(Arc::from_raw(self.handle as *const OutputWriterInner))
        }
    }

    pub fn stream_time_now_ns(&self) -> i64 {
        self.host_inner().map_or_else(
            || MediaClock::now().as_nanos() as i64,
            OutputWriterInner::implicit_stamp_now_ns,
        )
    }

    /// Publish `value` as a msgpack named map on `port`, stamped with stream time now, as a message
    /// that starts a group.
    pub fn write_msgpack<T: Serialize>(&self, port: &str, value: &T) -> Result<()> {
        let timestamp_ns = self.wired_inner_for_port(port)?.implicit_stamp_now_ns();
        self.write_msgpack_with_timestamp(port, value, timestamp_ns)
    }

    /// Publish `value` as a msgpack named map on `port`, stamped `timestamp_ns`, as
    /// a message that starts a group.
    pub fn write_msgpack_with_timestamp<T: Serialize>(
        &self,
        port: &str,
        value: &T,
        timestamp_ns: i64,
    ) -> Result<()> {
        let payload_bytes = rmp_serde::to_vec_named(value)
            .map_err(|e| Error::Link(format!("Failed to serialize frame: {}", e)))?;
        self.write_bytes_with_timestamp(
            port,
            &payload_bytes,
            timestamp_ns,
            MessageGroupBoundary::StartsGroup,
        )
    }

    /// Publish `payload_bytes` as given on `port`, stamped `timestamp_ns` and
    /// `group_boundary` in the message's header.
    pub fn write_bytes_with_timestamp(
        &self,
        port: &str,
        payload_bytes: &[u8],
        timestamp_ns: i64,
        group_boundary: MessageGroupBoundary,
    ) -> Result<()> {
        self.write_bytes_described_by(
            port,
            payload_bytes,
            timestamp_ns,
            group_boundary,
            DataChannelPayloadDescriptor::bytes(),
        )
    }

    /// [`Self::write_bytes_with_timestamp`], the message's header carrying
    /// `payload_descriptor` — a pooled GPU frame's, for one.
    pub fn write_bytes_described_by(
        &self,
        port: &str,
        payload_bytes: &[u8],
        timestamp_ns: i64,
        group_boundary: MessageGroupBoundary,
        payload_descriptor: DataChannelPayloadDescriptor,
    ) -> Result<()> {
        self.wired_inner_for_port(port)?.write_bytes_described_by(
            &cast_exposed_name_to_url_safe(port)?,
            payload_bytes,
            timestamp_ns,
            group_boundary,
            payload_descriptor,
        )
    }

    /// Declare `given_up_group_count` groups given up upstream of `port`, told
    /// to its `ordered` readers with the next message it sends. For
    /// engine-internal writers only.
    #[doc(hidden)]
    pub fn declare_groups_given_up_upstream_before_the_next_write(
        &self,
        port: &str,
        given_up_group_count: u64,
    ) -> Result<()> {
        self.wired_inner_for_port(port)?
            .declare_groups_given_up_upstream_before_the_next_write(
                &cast_exposed_name_to_url_safe(port)?,
                given_up_group_count,
            )
    }

    /// Check if a port is configured.
    pub fn has_port(&self, port: &str) -> bool {
        match (self.host_inner(), cast_exposed_name_to_url_safe(port)) {
            (Some(inner), Ok(port)) => inner.has_port(&port),
            _ => false,
        }
    }

    /// Replace `port`'s stream facts — what `graph` renders under the port and
    /// a downstream reader reads off its inbound link. Refused when they are
    /// another family's than the port's.
    pub fn set_stream_facts(
        &self,
        port: &str,
        stream_facts: impl Into<PortStreamFacts>,
    ) -> Result<()> {
        self.wired_inner_for_port(port)?
            .set_stream_facts(port, stream_facts.into())
    }
}

impl Default for OutputWriter {
    fn default() -> Self {
        Self::empty()
    }
}

impl Clone for OutputWriter {
    fn clone(&self) -> Self {
        if !self.is_configured() {
            return Self::empty();
        }
        // SAFETY: `handle` is `Arc::into_raw(Arc<OutputWriterInner>)`; bump
        // the strong count so both handles own one reference.
        unsafe {
            Arc::increment_strong_count(self.handle as *const OutputWriterInner);
        }
        Self {
            handle: self.handle,
        }
    }
}

impl Drop for OutputWriter {
    fn drop(&mut self) {
        if !self.is_configured() {
            return;
        }
        // SAFETY: `handle` is `Arc::into_raw(Arc<OutputWriterInner>)`.
        unsafe {
            drop(Arc::from_raw(self.handle as *const OutputWriterInner));
        }
        self.handle = std::ptr::null();
    }
}
