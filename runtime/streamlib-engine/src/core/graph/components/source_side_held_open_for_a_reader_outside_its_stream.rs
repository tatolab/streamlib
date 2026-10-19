// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::sync::{Arc, Weak};

use crate::core::graph::LinkUniqueId;
use crate::core::processors::OutOfProcessLinkWiringEnvelope;
use crate::iceoryx2::OutputWriterInner;

/// Where the source's side of a link was wired: the output writer of a
/// processor the engine runs, or the link wiring of a helper process.
#[derive(Clone)]
pub(crate) enum SourceSideOfALinkWiredOn {
    ANativeProcessorsOutputWriter(Arc<OutputWriterInner>),
    AHelperProcessesLinkWiring(Arc<OutOfProcessLinkWiringEnvelope>),
}

/// The source's side of a channel held open for the runtime's publisher, a
/// reader outside the source's stream, released when dropped: the reader's
/// notifier comes off the source's output port, taking the port's publisher
/// with it when it was the last, or a helper source is told to drop what it
/// opened. Held through weak handles, so a source processor that is gone makes
/// the release a no-op and is never kept alive by it.
#[derive(Debug)]
#[must_use = "dropping it releases the source's side of the link at once"]
pub(crate) struct SourceSideHeldOpenForAReaderOutsideItsStream {
    wired_on: WeakSourceSideOfALinkWiredOn,
    source_port: String,
    link_id: LinkUniqueId,
}

#[derive(Debug)]
enum WeakSourceSideOfALinkWiredOn {
    ANativeProcessorsOutputWriter(Weak<OutputWriterInner>),
    AHelperProcessesLinkWiring(Weak<OutOfProcessLinkWiringEnvelope>),
}

impl SourceSideHeldOpenForAReaderOutsideItsStream {
    /// The source's side of link `link_id` out of `source_port`, wired on
    /// `wired_on`, released when dropped.
    pub(crate) fn released_when_dropped(
        wired_on: &SourceSideOfALinkWiredOn,
        source_port: &str,
        link_id: &LinkUniqueId,
    ) -> Self {
        Self {
            wired_on: match wired_on {
                SourceSideOfALinkWiredOn::ANativeProcessorsOutputWriter(output_writer) => {
                    WeakSourceSideOfALinkWiredOn::ANativeProcessorsOutputWriter(Arc::downgrade(
                        output_writer,
                    ))
                }
                SourceSideOfALinkWiredOn::AHelperProcessesLinkWiring(link_wiring) => {
                    WeakSourceSideOfALinkWiredOn::AHelperProcessesLinkWiring(Arc::downgrade(
                        link_wiring,
                    ))
                }
            },
            source_port: source_port.to_string(),
            link_id: link_id.clone(),
        }
    }
}

impl Drop for SourceSideHeldOpenForAReaderOutsideItsStream {
    fn drop(&mut self) {
        match &self.wired_on {
            WeakSourceSideOfALinkWiredOn::ANativeProcessorsOutputWriter(output_writer) => {
                let Some(output_writer) = output_writer.upgrade() else {
                    return;
                };
                let publisher_released =
                    output_writer.remove_channel_link(&self.source_port, self.link_id.as_str());
                tracing::debug!(
                    link_id = %self.link_id,
                    port = %self.source_port,
                    publisher_released,
                    "Released the source's side of a channel held open for a reader outside its stream"
                );
            }
            WeakSourceSideOfALinkWiredOn::AHelperProcessesLinkWiring(link_wiring) => {
                let Some(link_wiring) = link_wiring.upgrade() else {
                    return;
                };
                link_wiring.unwire_a_link_reporting_a_far_side_that_kept_its_port(
                    crate::core::PortDirection::Output,
                    &self.source_port,
                    self.link_id.as_str(),
                );
            }
        }
    }
}
