// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::sync::{Arc, Weak};

use crate::core::graph::LinkUniqueId;
use crate::core::nodes::OutOfProcessLinkWiringEnvelope;
use crate::iceoryx2::OutputWriterInner;

#[derive(Clone)]
pub(crate) enum SourceSideOfALinkWiredOn {
    ANativeNodesOutputWriter(Arc<OutputWriterInner>),
    AHelperProcessesLinkWiring(Arc<OutOfProcessLinkWiringEnvelope>),
}

#[derive(Debug)]
#[must_use = "dropping it releases the source's side of the link at once"]
pub(crate) struct SourceSideHeldOpenForAReaderOutsideItsStream {
    wired_on: WeakSourceSideOfALinkWiredOn,
    source_port: String,
    link_id: LinkUniqueId,
}

#[derive(Debug)]
enum WeakSourceSideOfALinkWiredOn {
    ANativeNodesOutputWriter(Weak<OutputWriterInner>),
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
                SourceSideOfALinkWiredOn::ANativeNodesOutputWriter(output_writer) => {
                    WeakSourceSideOfALinkWiredOn::ANativeNodesOutputWriter(Arc::downgrade(
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
            WeakSourceSideOfALinkWiredOn::ANativeNodesOutputWriter(output_writer) => {
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
