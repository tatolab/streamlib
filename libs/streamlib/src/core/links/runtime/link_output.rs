//! LinkOutput - Output port for a processor.

use std::cell::UnsafeCell;
use std::sync::Arc;

use crossbeam_channel::Sender;

use super::link_output_data_writer::LinkOutputDataWriter;
use super::link_output_to_processor_message::LinkOutputToProcessorMessage;
use crate::core::links::graph::LinkId;
use crate::core::links::traits::LinkPortMessage;
use crate::core::{Result, StreamError};

/// Binding between a LinkOutput port and a downstream processor via a specific link.
struct LinkOutputToDownstreamProcessor<T: LinkPortMessage> {
    link_id: LinkId,
    data_writer: LinkOutputDataWriter<T>,
    message_writer: Option<Sender<LinkOutputToProcessorMessage>>,
}

impl<T: LinkPortMessage> LinkOutputToDownstreamProcessor<T> {
    fn new(link_id: LinkId, data_writer: LinkOutputDataWriter<T>) -> Self {
        Self {
            link_id,
            data_writer,
            message_writer: None,
        }
    }

    fn write(&self, value: T) -> bool {
        let written = self.data_writer.write(value);
        if written {
            if let Some(writer) = &self.message_writer {
                let _ = writer.send(LinkOutputToProcessorMessage::InvokeProcessingNow);
            }
        }
        written
    }

    fn is_connected(&self) -> bool {
        self.data_writer.is_connected()
    }
}

/// Inner state for LinkOutput.
struct LinkOutputInner<T: LinkPortMessage> {
    port_name: String,
    downstream_processors: UnsafeCell<Vec<LinkOutputToDownstreamProcessor<T>>>,
}

// SAFETY: LinkOutputInner is only accessed by the owning processor thread.
unsafe impl<T: LinkPortMessage> Send for LinkOutputInner<T> {}
unsafe impl<T: LinkPortMessage> Sync for LinkOutputInner<T> {}

/// Output link port for a processor.
///
/// Supports fan-out: one output can connect to multiple inputs.
/// When no connections exist or all connections are dead, writes are silently dropped.
pub struct LinkOutput<T: LinkPortMessage> {
    inner: Arc<LinkOutputInner<T>>,
}

impl<T: LinkPortMessage> Clone for LinkOutput<T> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<T: LinkPortMessage> LinkOutput<T> {
    /// Create a new output port.
    pub fn new(port_name: &str) -> Self {
        Self {
            inner: Arc::new(LinkOutputInner {
                port_name: port_name.to_string(),
                downstream_processors: UnsafeCell::new(Vec::new()),
            }),
        }
    }

    #[inline]
    #[allow(clippy::mut_from_ref)] // UnsafeCell provides interior mutability
    unsafe fn downstream_processors_mut(&self) -> &mut Vec<LinkOutputToDownstreamProcessor<T>> {
        &mut *self.inner.downstream_processors.get()
    }

    #[inline]
    fn downstream_processors(&self) -> &Vec<LinkOutputToDownstreamProcessor<T>> {
        unsafe { &*self.inner.downstream_processors.get() }
    }

    /// Add a data writer for a downstream processor.
    pub fn add_data_writer(
        &self,
        link_id: LinkId,
        data_writer: LinkOutputDataWriter<T>,
    ) -> Result<()> {
        unsafe {
            let downstream = self.downstream_processors_mut();

            if downstream.iter().any(|d| d.link_id == link_id) {
                return Err(StreamError::LinkAlreadyExists(link_id.to_string()));
            }

            downstream.push(LinkOutputToDownstreamProcessor::new(link_id, data_writer));
        }
        Ok(())
    }

    /// Remove a data writer by link ID.
    pub fn remove_data_writer(&self, link_id: &LinkId) -> Result<()> {
        unsafe {
            let downstream = self.downstream_processors_mut();
            let idx = downstream
                .iter()
                .position(|d| &d.link_id == link_id)
                .ok_or_else(|| StreamError::LinkNotFound(link_id.to_string()))?;
            downstream.swap_remove(idx);
        }
        Ok(())
    }

    /// Write data to all downstream processors (fan-out).
    pub fn write(&self, value: T) {
        unsafe {
            let downstream = self.downstream_processors_mut();

            if downstream.is_empty() {
                return;
            }

            downstream.retain(|d| d.is_connected());

            if downstream.is_empty() {
                return;
            }

            let len = downstream.len();

            for d in &downstream[..len - 1] {
                d.write(value.clone());
            }

            if let Some(last) = downstream.last() {
                last.write(value);
            }
        }
    }

    /// Alias for write().
    #[inline]
    pub fn push(&self, value: T) {
        self.write(value);
    }

    /// Check if port has any live downstream processors.
    pub fn is_connected(&self) -> bool {
        self.downstream_processors()
            .iter()
            .any(|d| d.is_connected())
    }

    /// Get count of live downstream processors.
    pub fn link_count(&self) -> usize {
        self.downstream_processors()
            .iter()
            .filter(|d| d.is_connected())
            .count()
    }

    /// Set the message writer for all downstream processors.
    pub fn set_link_output_to_processor_message_writer(
        &self,
        message_writer: Sender<LinkOutputToProcessorMessage>,
    ) {
        unsafe {
            let downstream = self.downstream_processors_mut();
            for d in downstream.iter_mut() {
                d.message_writer = Some(message_writer.clone());
            }
        }
    }

    /// Get the port name.
    pub fn port_name(&self) -> &str {
        &self.inner.port_name
    }
}
