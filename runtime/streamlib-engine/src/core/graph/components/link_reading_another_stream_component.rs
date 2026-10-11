// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::sync::{Arc, Weak};

use crate::core::graph::{
    GraphEdgeWithComponents, Link, LinkState, LinkStateComponent, LinkUniqueId,
    PendingDeletionComponent,
};
use crate::core::processors::OutOfProcessLinkWiringEnvelope;
use crate::core::runtime::{ExposedOutputPortReaderRegistration, LoadedStreamTag};
use crate::iceoryx2::OutputWriterInner;

/// What a link reading another stream on this machine holds beside its state:
/// why it carries nothing, read by `graph` as the link's
/// `awaiting_remote_reason` or `error_reason`, which wiring attempt is the
/// latest, and the wiring that attempt left on the source's stream.
///
/// Stored but never rendered under the link's `components`: `graph` renders
/// its reason at the top level of the link.
#[derive(Debug, Default)]
pub struct LinkReadingAnotherStreamComponent {
    why_it_carries_nothing: Option<String>,
    wiring_generation: u64,
    wiring: Option<WiringOfALinkReadingAnotherStream>,
}

impl LinkReadingAnotherStreamComponent {
    /// A link that carries nothing for `reason`.
    pub fn carrying_nothing_because(reason: String) -> Self {
        Self {
            why_it_carries_nothing: Some(reason),
            ..Self::default()
        }
    }

    /// Why the link carries nothing, when it says.
    pub fn why_it_carries_nothing(&self) -> Option<&str> {
        self.why_it_carries_nothing.as_deref()
    }

    /// Say the link carries nothing for `reason`.
    pub(crate) fn carry_nothing_because(&mut self, reason: String) {
        self.why_it_carries_nothing = Some(reason);
    }

    /// Say the link carries what its source publishes.
    pub(crate) fn carries_what_its_source_publishes(&mut self) {
        self.why_it_carries_nothing = None;
    }

    /// The latest wiring attempt, which every step of that attempt checks
    /// before it touches the link.
    pub(crate) fn wiring_generation(&self) -> u64 {
        self.wiring_generation
    }

    /// Begin a wiring attempt, superseding every one before it, and hand back
    /// its generation.
    pub(crate) fn begin_a_wiring_attempt(&mut self) -> u64 {
        self.wiring_generation += 1;
        self.wiring_generation
    }

    /// The wiring the latest attempt left on the source's stream, while it
    /// holds one.
    pub(crate) fn wiring(&self) -> Option<&WiringOfALinkReadingAnotherStream> {
        self.wiring.as_ref()
    }

    /// Note that the node publishing the link's source left its stream, so
    /// the wiring it held is stale.
    pub(crate) fn note_its_source_let_go(&mut self) {
        if let Some(wiring) = &mut self.wiring {
            wiring.source_let_go = true;
        }
    }

    /// Hold `wiring` as the link's, handing back the one it replaces.
    #[must_use = "the wiring handed back releases the source's side when dropped"]
    pub(crate) fn hold_wiring(
        &mut self,
        wiring: WiringOfALinkReadingAnotherStream,
    ) -> Option<WiringOfALinkReadingAnotherStream> {
        self.wiring.replace(wiring)
    }

    /// Take the link's wiring off it.
    #[must_use = "the wiring handed back releases the source's side when dropped"]
    pub(crate) fn take_wiring(&mut self) -> Option<WiringOfALinkReadingAnotherStream> {
        self.wiring.take()
    }
}

/// Whether `link`, reading another stream, is still to be wired: neither
/// refused nor on its way out.
pub(crate) fn a_link_reading_another_stream_is_still_to_be_wired(link: &Link) -> bool {
    !link.has::<PendingDeletionComponent>()
        && !link
            .get::<LinkStateComponent>()
            .is_some_and(|state| state.0 == LinkState::Error)
}

/// Whether wiring attempt `generation` is still the latest on `link`, reading
/// another stream, and the link is not on its way out — the check every step
/// of an attempt makes before it touches the link.
pub(crate) fn a_link_reading_another_stream_is_still_on_wiring_attempt(
    link: &Link,
    generation: u64,
) -> bool {
    !link.has::<PendingDeletionComponent>()
        && link
            .get::<LinkReadingAnotherStreamComponent>()
            .is_some_and(|component| component.wiring_generation() == generation)
}

/// Why a link reading the stream `source_stream_name` carries nothing while
/// that stream is not loaded on this machine.
pub(crate) fn why_a_link_awaits_a_source_stream_not_loaded(source_stream_name: &str) -> String {
    format!(
        "the stream `{source_stream_name}` is not loaded on this machine; the link wires when it \
         loads"
    )
}

/// What one wiring of a link reading another stream holds on the source's
/// stream. Dropping it takes the reader off the source's port, then releases
/// the source's side of the channel; neither takes a graph lock, so it is
/// dropped in place under either stream's.
#[derive(Debug)]
pub(crate) struct WiringOfALinkReadingAnotherStream {
    // Fields drop in declaration order: the registration goes first, so a cut
    // not yet begun never begins against a source side already released.
    _exposure_registration: ExposedOutputPortReaderRegistration,
    _source_side: SourceSideOfALinkReadingAnotherStream,
    source_stream_tag: LoadedStreamTag,
    source_let_go: bool,
}

impl WiringOfALinkReadingAnotherStream {
    /// A wiring against the load of the source's stream tagged
    /// `source_stream_tag`.
    pub(crate) fn new(
        exposure_registration: ExposedOutputPortReaderRegistration,
        source_side: SourceSideOfALinkReadingAnotherStream,
        source_stream_tag: LoadedStreamTag,
    ) -> Self {
        Self {
            _exposure_registration: exposure_registration,
            _source_side: source_side,
            source_stream_tag,
            source_let_go: false,
        }
    }

    /// Whether this wiring still stands against the load of the source's
    /// stream tagged `loaded_source_stream_tag`: made against that load, and
    /// its source's node has not left the stream.
    pub(crate) fn still_stands_against(&self, loaded_source_stream_tag: LoadedStreamTag) -> bool {
        !self.source_let_go && self.source_stream_tag == loaded_source_stream_tag
    }
}

/// Where the source's side of a link was wired: the output writer of a
/// processor the engine runs, or the link wiring of a helper process.
#[derive(Clone)]
pub(crate) enum SourceSideOfALinkWiredOn {
    ANativeProcessorsOutputWriter(Arc<OutputWriterInner>),
    AHelperProcessesLinkWiring(Arc<OutOfProcessLinkWiringEnvelope>),
}

/// The source's side of a link reading another stream, released when dropped:
/// the link's notifier comes off the source's output port, taking the port's
/// publisher with it when the link was its last, or a helper source is told to
/// drop what it opened. Held through weak handles, so a source processor that
/// is gone makes the release a no-op and is never kept alive by it.
#[derive(Debug)]
#[must_use = "dropping it releases the source's side of the link at once"]
pub(crate) struct SourceSideOfALinkReadingAnotherStream {
    wired_on: WeakSourceSideOfALinkWiredOn,
    source_port: String,
    link_id: LinkUniqueId,
}

#[derive(Debug)]
enum WeakSourceSideOfALinkWiredOn {
    ANativeProcessorsOutputWriter(Weak<OutputWriterInner>),
    AHelperProcessesLinkWiring(Weak<OutOfProcessLinkWiringEnvelope>),
}

impl SourceSideOfALinkReadingAnotherStream {
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

impl Drop for SourceSideOfALinkReadingAnotherStream {
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
                    "Released the source's side of a link reading another stream"
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
