// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Wiring a link that reads an output port of another stream on this machine.
//!
//! One idempotent reconcile runs for every `AddLink` of such a link. It holds
//! one stream's graph lock at a time — the reading stream's, then the source
//! stream's for a bounded wait, then the reading stream's again — and leaves
//! the link `wired`, `pending` its out-of-process ends, `awaiting_remote` with
//! what it waits for, or `error` with its refusal. A link that waits or was
//! refused never fails the commit it is wired in.

use std::sync::Arc;
use std::time::Duration;

use super::open_iceoryx2_service_op::{
    DestinationOfALinkAsItsWiringReadsIt, HowAnOpenedLinkStands, OpenedSourceSideOfALink,
    open_the_destination_side_of_a_link, open_the_source_side_of_a_link,
    read_the_destination_of_a_link, release_the_destination_side_of_a_link,
};
use crate::core::error::Error;
use crate::core::graph::{
    CutOffAReaderOfAnExposedOutputPort, Graph, GraphEdgeWithComponents, GraphNodeWithComponents,
    Iceoryx2ServicesHeldOpenForLinkComponent, Link, LinkReadingAnotherStreamComponent, LinkState,
    LinkStateComponent, LinkUniqueId, OutOfProcessLinkWireRepliesComponent,
    OutOfProcessLinkWiringComponent, OutputLinkPortRef, OutputPortInAnotherStreamOnThisMachine,
    OutputPortReaderOutsideItsStream, PendingDeletionComponent, ProcessorInstanceComponent,
    ReaderOfAnExposedOutputPort, SourceSideOfALinkReadingAnotherStream,
    WiringOfALinkReadingAnotherStream, a_link_reading_another_stream_is_still_on_wiring_attempt,
    a_link_reading_another_stream_is_still_to_be_wired,
    why_a_link_awaits_a_source_stream_not_loaded,
};
use crate::core::runtime::{
    ExposedOutputPortReaderRegistration, LoadedStreamInThisRuntime, LoadedStreamTag,
    ReaderRegisteredOnAGraph, register_a_reader_on_this_graph,
    the_cut_of_a_link_reading_another_stream,
};
use crate::iceoryx2::Iceoryx2Node;

/// How long a reconcile waits for the source stream's graph lock before it
/// reads the link as awaiting that stream. The next trigger retries.
pub(crate) const HOW_LONG_A_LINK_READING_ANOTHER_STREAM_WAITS_FOR_ITS_SOURCES_GRAPH: Duration =
    Duration::from_secs(2);

/// How a reconcile left a link reading another stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HowALinkReadingAnotherStreamWasReconciled {
    /// Wired now, or handed to out-of-process ends that have not answered.
    WiredNow(HowAnOpenedLinkStands),
    /// Already wired against the source stream's current load.
    AlreadyWiredAgainstItsSourcesLoad,
    /// Waiting for its source, for the reason given.
    AwaitingItsSource(String),
    /// Refused, for the reason given. A refusal is final.
    Refused(String),
    /// Not this reconcile's to wire: gone from the graph, on its way out,
    /// refused before, or overtaken by a later attempt.
    NotThisReconcilesToWire,
}

/// Reconcile link `link_id` of `reading_stream` against the stream its source
/// is in: wire it when that stream runs and its port allows the reading
/// stream, unwire a wiring made against a load of that stream that is gone,
/// and otherwise say what the link waits for or why it was refused.
pub(crate) fn reconcile_a_link_reading_another_stream(
    reading_stream: &Arc<LoadedStreamInThisRuntime>,
    link_id: &LinkUniqueId,
) -> HowALinkReadingAnotherStreamWasReconciled {
    let engine = Arc::clone(reading_stream.engine_resources_shared_by_every_stream());

    let begun = reading_stream.compiler.scope(|graph, _tx| {
        begin_a_wiring_attempt(graph, link_id, |source_stream_name| {
            engine.loaded_stream_of_the_cast_name(source_stream_name)
        })
    });
    let BegunWiringAttempt {
        generation,
        source,
        source_stream,
        destination,
        wired_to_this_load_of_its_source_before,
    } = match begun {
        Ok(begun) => begun,
        Err(how_it_was_reconciled) => return how_it_was_reconciled,
    };
    let awaiting_its_source = |reason: String| {
        carry_nothing_for(
            reading_stream,
            link_id,
            generation,
            WhyALinkReadingAnotherStreamCarriesNothing::AwaitingItsSource,
            reason,
        )
    };
    let refused = |reason: String| {
        carry_nothing_for(
            reading_stream,
            link_id,
            generation,
            WhyALinkReadingAnotherStreamCarriesNothing::Refused,
            reason,
        )
    };

    let Some(source_stream) = source_stream else {
        return awaiting_its_source(why_a_link_awaits_a_source_stream_not_loaded(
            source.stream_name(),
        ));
    };
    if let Some(why_the_source_stream_cannot_be_wired_to) =
        source_stream.why_a_link_from_another_stream_cannot_wire_to_it_now()
    {
        return awaiting_its_source(why_the_source_stream_cannot_be_wired_to);
    }

    let the_cut = the_cut_of_a_link_reading_another_stream(
        Arc::downgrade(reading_stream),
        link_id.clone(),
        generation,
        engine.tokio_runtime_for_work_spawned_against_a_stream(),
    );
    let Some((opened_on_the_source, readers_to_drop_once_the_lock_is_released)) =
        source_stream.compiler.scope_within(
            HOW_LONG_A_LINK_READING_ANOTHER_STREAM_WAITS_FOR_ITS_SOURCES_GRAPH,
            |source_graph, _tx| {
                open_the_source_side_on_the_source_stream(
                    source_graph,
                    &source,
                    link_id,
                    &destination,
                    the_cut,
                    &engine.iceoryx2_node,
                )
            },
        )
    else {
        return awaiting_its_source(format!(
            "the stream `{}` did not answer within {}s; the link is tried again when that \
                 stream starts or adds a node, opens a port to other streams, or is unloaded",
            source.stream_name(),
            HOW_LONG_A_LINK_READING_ANOTHER_STREAM_WAITS_FOR_ITS_SOURCES_GRAPH.as_secs_f64()
        ));
    };
    // A reader's cut may own anything, so it is dropped only once the graph lock is released.
    drop(readers_to_drop_once_the_lock_is_released);
    let opened = match opened_on_the_source {
        SourceSideOnTheSourceStream::Opened(opened) => opened,
        SourceSideOnTheSourceStream::NotYet(reason) => {
            return awaiting_its_source(reason);
        }
        SourceSideOnTheSourceStream::ClosedToTheReadingStream(refusal)
            if wired_to_this_load_of_its_source_before =>
        {
            return awaiting_its_source(format!(
                "{refusal}; the link was wired to this load of the stream `{}` before, so it \
                     wires again when the port is opened to it",
                source.stream_name()
            ));
        }
        SourceSideOnTheSourceStream::ClosedToTheReadingStream(reason)
        | SourceSideOnTheSourceStream::Refused(reason) => {
            return refused(reason);
        }
    };

    reading_stream.compiler.scope(|graph, _tx| {
        open_the_destination_side_and_hold_the_wiring(
            graph,
            link_id,
            generation,
            *opened,
            destination,
            source_stream.stream_tag(),
        )
    })
}

/// What a wiring attempt read off the reading stream's graph before it reaches
/// for the source's stream.
struct BegunWiringAttempt {
    generation: u64,
    source: OutputPortInAnotherStreamOnThisMachine,
    source_stream: Option<Arc<LoadedStreamInThisRuntime>>,
    destination: DestinationOfALinkAsItsWiringReadsIt,
    wired_to_this_load_of_its_source_before: bool,
}

/// Begin a wiring attempt on link `link_id` of `reading_graph`, unless the
/// link is not this reconcile's to wire or still wired against the source
/// stream's current load: the attempt supersedes every earlier one, a wiring
/// that no longer stands is unwired and released, and the destination is read
/// — a destination that cannot take the link refuses it.
fn begin_a_wiring_attempt(
    reading_graph: &mut Graph,
    link_id: &LinkUniqueId,
    loaded_stream_of_the_cast_name: impl FnOnce(&str) -> Option<Arc<LoadedStreamInThisRuntime>>,
) -> Result<BegunWiringAttempt, HowALinkReadingAnotherStreamWasReconciled> {
    let not_this_reconciles =
        || Err(HowALinkReadingAnotherStreamWasReconciled::NotThisReconcilesToWire);
    let Some(link) = reading_graph
        .traversal_mut()
        .e(link_id)
        .first_mut()
        .filter(|link| a_link_reading_another_stream_is_still_to_be_wired(link))
    else {
        return not_this_reconciles();
    };
    let Some(source) = link.source().in_another_stream_on_this_machine().cloned() else {
        return not_this_reconciles();
    };
    let to_port = link.to_port().clone();
    if !link.has::<LinkReadingAnotherStreamComponent>() {
        link.insert_component_without_rendering_it(LinkReadingAnotherStreamComponent::default());
    }
    let source_stream = loaded_stream_of_the_cast_name(source.stream_name());
    let Some(component) = link.get_mut::<LinkReadingAnotherStreamComponent>() else {
        return not_this_reconciles();
    };
    if let (Some(wiring), Some(source_stream)) = (component.wiring(), &source_stream)
        && wiring.still_stands_against(source_stream.stream_tag())
    {
        return Err(HowALinkReadingAnotherStreamWasReconciled::AlreadyWiredAgainstItsSourcesLoad);
    }
    let generation = component.begin_a_wiring_attempt();
    let wired_to_this_load_of_its_source_before =
        source_stream.as_ref().is_some_and(|source_stream| {
            component.was_wired_to_this_load_of_its_source(source_stream.stream_tag())
        });
    if let Some(stale_wiring) = component.take_wiring() {
        drop(stale_wiring);
        link.remove::<Iceoryx2ServicesHeldOpenForLinkComponent>();
        link.remove::<OutOfProcessLinkWireRepliesComponent>();
        link.insert(LinkStateComponent(LinkState::Pending));
        release_the_destination_side_of_a_link(
            reading_graph,
            to_port.processor_id(),
            to_port.port_name(),
            link_id,
        );
        tracing::info!(
            %link_id,
            source = %source,
            "[WIRE] unwired a link reading another stream from `{}`, whose wiring no longer stands",
            source.stream_name()
        );
    }

    match read_the_destination_of_a_link(reading_graph, &to_port, link_id) {
        Ok(destination) => Ok(BegunWiringAttempt {
            generation,
            source,
            source_stream,
            destination,
            wired_to_this_load_of_its_source_before,
        }),
        Err(refusal) => {
            let refusal = refusal.to_string();
            stamp_unless_the_attempt_was_overtaken(
                reading_graph,
                link_id,
                generation,
                WhyALinkReadingAnotherStreamCarriesNothing::Refused,
                refusal.clone(),
            );
            Err(HowALinkReadingAnotherStreamWasReconciled::Refused(refusal))
        }
    }
}

/// What opening the source's side on the source stream's graph came to.
enum SourceSideOnTheSourceStream {
    Opened(Box<OpenedSourceSideOnTheSourceStream>),
    NotYet(String),
    /// The source's port is at a level that does not allow the reading
    /// stream, in the words of that refusal.
    ClosedToTheReadingStream(String),
    Refused(String),
}

/// The source's side of a link opened on the source stream's graph, what
/// that stream's port registered for the reader, and the source's side as the
/// link releases it.
struct OpenedSourceSideOnTheSourceStream {
    // Dropped unused, in declaration order: the reader comes off the source's port first, so a
    // cut not begun never begins; then the source's side is released; then the services go.
    exposure_registration: ExposedOutputPortReaderRegistration,
    source_side: SourceSideOfALinkReadingAnotherStream,
    opened_source_side: OpenedSourceSideOfALink,
}

/// On `source_graph`, the graph of the stream `source` is in: register the
/// reading stream as a reader of the source's port — refused when its level
/// does not allow it — then open the source's side of the link when the
/// source's node runs. Hands back, beside the outcome, the readers the caller
/// drops once it has let go of the lock.
fn open_the_source_side_on_the_source_stream(
    source_graph: &mut Graph,
    source: &OutputPortInAnotherStreamOnThisMachine,
    link_id: &LinkUniqueId,
    destination: &DestinationOfALinkAsItsWiringReadsIt,
    the_cut: CutOffAReaderOfAnExposedOutputPort,
    iceoryx2_node: &Iceoryx2Node,
) -> (
    SourceSideOnTheSourceStream,
    Vec<ReaderOfAnExposedOutputPort>,
) {
    let (registered, readers_to_drop) = register_a_reader_on_this_graph(
        source_graph,
        source.stream_name(),
        source.node_name(),
        source.port_name(),
        OutputPortReaderOutsideItsStream::ElsewhereOnThisMachine,
        the_cut,
    );
    let ReaderRegisteredOnAGraph {
        registration: exposure_registration,
        processor_id: source_processor_id,
        port_cast: source_port,
    } = match registered {
        Ok(registered) => registered,
        Err(refusal @ Error::OutputPortNotExposedToTheReader(_)) => {
            return (
                SourceSideOnTheSourceStream::ClosedToTheReadingStream(refusal.to_string()),
                readers_to_drop,
            );
        }
        Err(
            refusal @ (Error::ProcessorNotFound(_)
            | Error::NodeInAStreamHasNoOutputPortNamed { .. }),
        ) => {
            return (
                SourceSideOnTheSourceStream::NotYet(format!(
                    "{refusal}; the link wires when the stream `{}` holds it",
                    source.stream_name()
                )),
                readers_to_drop,
            );
        }
        Err(refusal) => {
            return (
                SourceSideOnTheSourceStream::Refused(refusal.to_string()),
                readers_to_drop,
            );
        }
    };

    let source_node_runs = source_graph
        .traversal()
        .v(&source_processor_id)
        .first()
        .is_some_and(|node| {
            (node.has::<ProcessorInstanceComponent>()
                || node.has::<OutOfProcessLinkWiringComponent>())
                && !node.has::<PendingDeletionComponent>()
        });
    if !source_node_runs {
        return (
            SourceSideOnTheSourceStream::NotYet(format!(
                "node `{}` of the stream `{}` is not running yet; the link wires when it runs",
                source.node_name(),
                source.stream_name()
            )),
            readers_to_drop,
        );
    }

    let opened_source_side = match open_the_source_side_of_a_link(
        source_graph,
        &OutputLinkPortRef::new(source_processor_id, source_port.as_str()),
        link_id,
        destination,
        iceoryx2_node,
    ) {
        Ok(opened_source_side) => opened_source_side,
        Err(refusal) => {
            return (
                SourceSideOnTheSourceStream::Refused(refusal.to_string()),
                readers_to_drop,
            );
        }
    };
    let Some(wired_on) = opened_source_side.wired_on() else {
        return (
            SourceSideOnTheSourceStream::Refused(format!(
                "node `{}` of the stream `{}` holds no output writer, so its port `{}` \
                 publishes nothing",
                source.node_name(),
                source.stream_name(),
                source_port
            )),
            readers_to_drop,
        );
    };
    let source_side = SourceSideOfALinkReadingAnotherStream::released_when_dropped(
        wired_on,
        &source_port,
        link_id,
    );
    (
        SourceSideOnTheSourceStream::Opened(Box::new(OpenedSourceSideOnTheSourceStream {
            opened_source_side,
            exposure_registration,
            source_side,
        })),
        readers_to_drop,
    )
}

/// On `reading_graph`, open the destination's side of link `link_id` and hold
/// the wiring on the link — unless the link went, is on its way out, or a
/// later attempt overtook `generation`, when the opened source side is
/// released.
fn open_the_destination_side_and_hold_the_wiring(
    reading_graph: &mut Graph,
    link_id: &LinkUniqueId,
    generation: u64,
    opened: OpenedSourceSideOnTheSourceStream,
    destination: DestinationOfALinkAsItsWiringReadsIt,
    source_stream_tag: LoadedStreamTag,
) -> HowALinkReadingAnotherStreamWasReconciled {
    if the_link_still_on_wiring_attempt(reading_graph, link_id, generation).is_none() {
        drop(opened);
        return HowALinkReadingAnotherStreamWasReconciled::NotThisReconcilesToWire;
    }
    let wire_reply_awaited_from_the_destination = match open_the_destination_side_of_a_link(
        reading_graph,
        link_id,
        &opened.opened_source_side,
        destination,
    ) {
        Ok(wire_reply) => wire_reply,
        Err(refusal) => {
            let refusal = refusal.to_string();
            stamp_unless_the_attempt_was_overtaken(
                reading_graph,
                link_id,
                generation,
                WhyALinkReadingAnotherStreamCarriesNothing::Refused,
                refusal.clone(),
            );
            drop(opened);
            return HowALinkReadingAnotherStreamWasReconciled::Refused(refusal);
        }
    };

    let Some(link) = the_link_still_on_wiring_attempt(reading_graph, link_id, generation) else {
        drop(opened);
        return HowALinkReadingAnotherStreamWasReconciled::NotThisReconcilesToWire;
    };
    let Some(component) = link.get_mut::<LinkReadingAnotherStreamComponent>() else {
        drop(opened);
        return HowALinkReadingAnotherStreamWasReconciled::NotThisReconcilesToWire;
    };
    let OpenedSourceSideOnTheSourceStream {
        exposure_registration,
        source_side,
        opened_source_side,
    } = opened;
    component.carries_what_its_source_publishes();
    drop(
        component.hold_wiring(WiringOfALinkReadingAnotherStream::new(
            exposure_registration,
            source_side,
            source_stream_tag,
        )),
    );
    let how_it_stands = opened_source_side
        .hold_its_services_on_the_link_and_stamp_it(link, wire_reply_awaited_from_the_destination);
    HowALinkReadingAnotherStreamWasReconciled::WiredNow(how_it_stands)
}

/// Link `link_id` of `reading_graph`, while wiring attempt `generation` is
/// still the latest on it and it is not on its way out.
fn the_link_still_on_wiring_attempt<'graph>(
    reading_graph: &'graph mut Graph,
    link_id: &LinkUniqueId,
    generation: u64,
) -> Option<&'graph mut Link> {
    reading_graph
        .traversal_mut()
        .e(link_id)
        .first_mut()
        .filter(|link| a_link_reading_another_stream_is_still_on_wiring_attempt(link, generation))
}

/// Why a reconcile leaves a link reading another stream carrying nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WhyALinkReadingAnotherStreamCarriesNothing {
    /// It waits for its source, and reads `awaiting_remote`.
    AwaitingItsSource,
    /// It was refused for good, and reads `error`.
    Refused,
}

impl WhyALinkReadingAnotherStreamCarriesNothing {
    fn link_state(self) -> LinkState {
        match self {
            Self::AwaitingItsSource => LinkState::AwaitingRemote,
            Self::Refused => LinkState::Error,
        }
    }

    fn how_it_was_reconciled(self, reason: String) -> HowALinkReadingAnotherStreamWasReconciled {
        match self {
            Self::AwaitingItsSource => {
                HowALinkReadingAnotherStreamWasReconciled::AwaitingItsSource(reason)
            }
            Self::Refused => HowALinkReadingAnotherStreamWasReconciled::Refused(reason),
        }
    }
}

/// Stamp link `link_id` as carrying nothing for `why`, in the words of
/// `reason`, unless a later attempt overtook `generation` or the link is on its
/// way out; say whether it was stamped.
fn stamp_unless_the_attempt_was_overtaken(
    reading_graph: &mut Graph,
    link_id: &LinkUniqueId,
    generation: u64,
    why: WhyALinkReadingAnotherStreamCarriesNothing,
    reason: String,
) -> bool {
    let Some(link) = the_link_still_on_wiring_attempt(reading_graph, link_id, generation) else {
        return false;
    };
    link.insert(LinkStateComponent(why.link_state()));
    if let Some(component) = link.get_mut::<LinkReadingAnotherStreamComponent>() {
        component.carry_nothing_because(reason);
    }
    true
}

/// Stamp link `link_id` of `reading_stream` as carrying nothing for `why`, in
/// the words of `reason`, unless a later attempt overtook `generation`, and
/// say how it was reconciled.
fn carry_nothing_for(
    reading_stream: &LoadedStreamInThisRuntime,
    link_id: &LinkUniqueId,
    generation: u64,
    why: WhyALinkReadingAnotherStreamCarriesNothing,
    reason: String,
) -> HowALinkReadingAnotherStreamWasReconciled {
    let state = why.link_state();
    tracing::info!(%link_id, %state, "[WIRE] a link reading another stream: {reason}");
    let stamped = reading_stream.compiler.scope(|graph, _tx| {
        stamp_unless_the_attempt_was_overtaken(graph, link_id, generation, why, reason.clone())
    });
    if stamped {
        why.how_it_was_reconciled(reason)
    } else {
        HowALinkReadingAnotherStreamWasReconciled::NotThisReconcilesToWire
    }
}

#[cfg(test)]
mod tests {
    //! The reconcile against two loaded streams read as running without the
    //! GPU context a real start takes: every node holds the processor
    //! instance a start leaves it with, and nothing compiles.

    use std::time::Duration;

    use parking_lot::Mutex;
    use serde_json::Value;

    use super::*;
    use crate::core::graph::{
        ExposedOutputPortsComponent, InputLinkPortRef, OutputPortExposureLevel,
        ProcessorInstanceWithItsOutOfProcessLinkWiring,
    };
    use crate::core::processors::{ProcessorInstance, ProcessorSpec};
    use crate::core::pubsub::{Event, EventListener, PUBSUB, RuntimeEvent, topics};
    use crate::core::runtime::{
        OptionsForLoadingOneStream, Runner, RuntimeOperations, RuntimeStatus,
    };
    use crate::core::test_support::{
        MockInputOnlyProcessor, MockOutputOnlyProcessor, a_temporary_directory_at_owner_only_mode,
        ensure_test_mocks_registered,
    };
    use crate::iceoryx2::{InputMailboxesInner, OutputWriterInner};

    /// How long a test waits for a spawned cut to land on the reading stream.
    const A_CUT_LANDS_WITHIN: Duration = Duration::from_secs(10);

    /// How long releasing a link's source side may take with no lock to wait on.
    const A_RELEASE_RETURNS_WITHIN: Duration = Duration::from_secs(2);

    /// Give node `node_name` of `stream` a live instance of `P`, holding the
    /// iceoryx2 resources its ports call for, as a start leaves it.
    fn run_the_node<P>(
        stream: &LoadedStreamInThisRuntime,
        node_name: &str,
    ) -> (
        Option<Arc<OutputWriterInner>>,
        Option<Arc<InputMailboxesInner>>,
    )
    where
        P: crate::core::GeneratedProcessor
            + crate::core::processors::DynGeneratedProcessor
            + Send
            + 'static,
        P::Config: Default,
    {
        let mut instance = ProcessorInstance::new(Box::new(
            P::from_config(Default::default()).expect("the mock constructs"),
        ));
        instance
            .install_iceoryx2_resources()
            .expect("the mock accepts its iceoryx2 resources");
        let output_writer = instance.iceoryx2_output_writer_inner();
        let input_mailboxes = instance.iceoryx2_input_mailboxes_inner();
        stream.compiler.scope(|graph, _tx| {
            let processor_id = graph
                .traversal()
                .v_with_node_name(node_name)
                .first()
                .expect("the node is in the stream")
                .id
                .clone();
            ProcessorInstanceWithItsOutOfProcessLinkWiring::from(instance).attach_to(
                graph
                    .traversal_mut()
                    .v(&processor_id)
                    .first_mut()
                    .expect("the node is in the stream"),
            );
        });
        (output_writer, input_mailboxes)
    }

    fn read_as_running(stream: &LoadedStreamInThisRuntime) {
        *stream.status.lock() = RuntimeStatus::Started;
    }

    /// `a`, whose `camera` publishes `out1`, and `b`, whose `display` reads
    /// `in1`, both read as running.
    struct TwoStreamsReadAsRunning {
        runner: Arc<Runner>,
        a: Arc<LoadedStreamInThisRuntime>,
        b: Arc<LoadedStreamInThisRuntime>,
        camera_output: Arc<OutputWriterInner>,
        display_input: Arc<InputMailboxesInner>,
        project_directory: tempfile::TempDir,
    }

    impl TwoStreamsReadAsRunning {
        fn load() -> Self {
            ensure_test_mocks_registered();
            let project_directory = a_temporary_directory_at_owner_only_mode().unwrap();
            let runner = Runner::new().expect("the engine builds");
            let b = Self::load_a_stream_holding(
                &runner,
                &project_directory,
                "b",
                MockInputOnlyProcessor::processor_class_import_path(),
                "display",
            );
            let (_, display_input) =
                run_the_node::<MockInputOnlyProcessor::Processor>(&b, "display");
            read_as_running(&b);
            let (a, camera_output) =
                Self::load_a_running_camera_stream(&runner, &project_directory);
            Self {
                runner,
                a,
                b,
                camera_output,
                display_input: display_input.expect("an input-only mock holds input mailboxes"),
                project_directory,
            }
        }

        fn load_a_stream_holding(
            runner: &Runner,
            project_directory: &tempfile::TempDir,
            stream_name: &str,
            node_type: crate::core::descriptors::ProcessorClassImportPath,
            node_name: &str,
        ) -> Arc<LoadedStreamInThisRuntime> {
            let stream = runner
                .load_an_empty_stream(
                    OptionsForLoadingOneStream::in_project_directory(project_directory.path())
                        .named(stream_name),
                )
                .expect("the stream loads");
            stream
                .add_processor(
                    ProcessorSpec::new(node_type, Value::Null)
                        .with_display_name(node_name.to_string()),
                )
                .expect("the node is added");
            stream
        }

        /// Add a node `node_name` of `node_type` to `stream`; its id.
        fn add_a_node_to(
            stream: &LoadedStreamInThisRuntime,
            node_type: crate::core::descriptors::ProcessorClassImportPath,
            node_name: &str,
        ) -> crate::core::graph::ProcessorUniqueId {
            stream
                .add_processor_reporting_its_name(
                    ProcessorSpec::new(node_type, Value::Null)
                        .with_display_name(node_name.to_string()),
                )
                .expect("the node is added")
                .processor_id
        }

        /// A stream `a` whose `camera` runs, its `out1` private.
        fn load_a_running_camera_stream(
            runner: &Runner,
            project_directory: &tempfile::TempDir,
        ) -> (Arc<LoadedStreamInThisRuntime>, Arc<OutputWriterInner>) {
            let a = Self::load_a_stream_holding(
                runner,
                project_directory,
                "a",
                MockOutputOnlyProcessor::processor_class_import_path(),
                "camera",
            );
            let (camera_output, _) =
                run_the_node::<MockOutputOnlyProcessor::Processor>(&a, "camera");
            a.set_output_port_exposure_level("camera", "out1", OutputPortExposureLevel::Private)
                .unwrap();
            read_as_running(&a);
            (
                a,
                camera_output.expect("an output-only mock holds an output writer"),
            )
        }

        /// Add a link in `b` reading `source` into the display, as a load of
        /// `b` does — before `b` runs, so nothing about the source is checked
        /// up front.
        fn link_the_display_from(
            &self,
            source: OutputPortInAnotherStreamOnThisMachine,
        ) -> LinkUniqueId {
            let display = self.b.the_node_named("display").unwrap();
            let status_b_had =
                std::mem::replace(&mut *self.b.status.lock(), RuntimeStatus::Initial);
            let link_id = self.b.connect_from_an_output_port_in_another_stream(
                source,
                InputLinkPortRef::new(display.processor_id, "in1"),
            );
            *self.b.status.lock() = status_b_had;
            link_id.expect("the link is added")
        }

        fn the_link_as_graph_renders_it(&self, link_id: &LinkUniqueId) -> Value {
            self.b.to_json().unwrap()["links"]
                .as_array()
                .and_then(|links| links.iter().find(|link| link["id"] == link_id.as_str()))
                .cloned()
                .unwrap_or(Value::Null)
        }

        /// Take the camera out of `a` as a removal does, cutting every reader
        /// of its port naming that its node left, and wait for the cut to land
        /// on `b`.
        fn remove_the_camera_cutting_its_readers(&self) {
            let (_listener, changed) = listen_on(&self.b, |event| {
                matches!(event, RuntimeEvent::GraphDidChange)
            });
            let exposures_of_the_camera = self.a.compiler.scope(|graph, _tx| {
                let camera = graph
                    .traversal()
                    .v_with_node_name("camera")
                    .first()
                    .expect("the camera is in `a`")
                    .id
                    .clone();
                let exposures = graph
                    .traversal_mut()
                    .v(&camera)
                    .first_mut()
                    .and_then(|node| node.remove::<ExposedOutputPortsComponent>())
                    .expect("the camera's port is exposed");
                graph.traversal_mut().v(&camera).drop();
                exposures
            });
            for reader in exposures_of_the_camera.into_every_reader() {
                reader.cut_off_unless_its_registration_was_dropped(
                    crate::core::graph::WhyAReaderOfAnExposedOutputPortIsCutOff::ItsPortsNodeLeftItsStream,
                );
            }
            changed
                .recv_timeout(A_CUT_LANDS_WITHIN)
                .expect("the cut lands on the reading stream");
        }

        fn readers_registered_against_the_camera_of(stream: &LoadedStreamInThisRuntime) -> usize {
            stream.compiler.scope(|graph, _tx| {
                graph
                    .traversal()
                    .v_with_node_name("camera")
                    .first()
                    .and_then(|camera| camera.get::<ExposedOutputPortsComponent>())
                    .map_or(0, |exposed| exposed.readers_registered_against("out1"))
            })
        }
    }

    fn camera_out1_of(stream_name: &str) -> OutputPortInAnotherStreamOnThisMachine {
        OutputPortInAnotherStreamOnThisMachine::new(stream_name, "camera", "out1").unwrap()
    }

    /// Hands every event of one stream's topic matching `wanted` to a channel.
    struct ForwardsTheEventsOfOneStream {
        wanted: fn(&RuntimeEvent) -> bool,
        forward: std::sync::mpsc::Sender<RuntimeEvent>,
    }

    impl EventListener for ForwardsTheEventsOfOneStream {
        fn on_event(&mut self, event: &Event) -> crate::core::Result<()> {
            if let Event::OfALoadedStream { event, .. } = event
                && (self.wanted)(event)
            {
                let _ = self.forward.send(event.clone());
            }
            Ok(())
        }
    }

    /// Listen on `stream`'s topic for the events `wanted` picks.
    fn listen_on(
        stream: &LoadedStreamInThisRuntime,
        wanted: fn(&RuntimeEvent) -> bool,
    ) -> (
        Arc<Mutex<dyn EventListener>>,
        std::sync::mpsc::Receiver<RuntimeEvent>,
    ) {
        let (forward, forwarded) = std::sync::mpsc::channel();
        let listener: Arc<Mutex<dyn EventListener>> =
            Arc::new(Mutex::new(ForwardsTheEventsOfOneStream { wanted, forward }));
        PUBSUB
            .subscribe(
                &topics::loaded_stream(stream.loaded_stream_identity()),
                Arc::clone(&listener),
            )
            .unwrap();
        (listener, forwarded)
    }

    /// A link whose source stream runs and whose port allows it is wired on
    /// a channel the source publishes into, carries bags, and is not wired a
    /// second time by the next reconcile.
    #[test]
    fn a_link_wires_once_its_source_stream_runs_and_its_port_allows_it_and_carries_bags() {
        let streams = TwoStreamsReadAsRunning::load();
        let link_id = streams.link_the_display_from(camera_out1_of("a"));

        let reconciled = reconcile_a_link_reading_another_stream(&streams.b, &link_id);

        assert_eq!(
            reconciled,
            HowALinkReadingAnotherStreamWasReconciled::WiredNow(HowAnOpenedLinkStands::Wired)
        );
        let rendered = streams.the_link_as_graph_renders_it(&link_id);
        assert_eq!(rendered["state"], "wired", "{rendered}");
        assert!(
            rendered.get("awaiting_remote_reason").is_none(),
            "{rendered}"
        );
        assert_eq!(
            TwoStreamsReadAsRunning::readers_registered_against_the_camera_of(&streams.a),
            1
        );
        streams
            .camera_output
            .write_raw("out1", b"a bag read from the stream a", 0)
            .expect("the camera publishes");
        streams.display_input.receive_pending();
        assert_eq!(streams.display_input.drain("in1").len(), 1);

        assert_eq!(
            reconcile_a_link_reading_another_stream(&streams.b, &link_id),
            HowALinkReadingAnotherStreamWasReconciled::AlreadyWiredAgainstItsSourcesLoad
        );
        assert_eq!(streams.camera_output.channel_notifier_count("out1"), 1);
    }

    /// A paused source stream keeps the processors its start made, so a link
    /// reading it wires and carries what it writes once it resumes.
    #[test]
    fn a_link_wires_to_a_paused_source_stream() {
        for status_of_the_source in [RuntimeStatus::Pausing, RuntimeStatus::Paused] {
            let streams = TwoStreamsReadAsRunning::load();
            *streams.a.status.lock() = status_of_the_source;
            let link_id = streams.link_the_display_from(camera_out1_of("a"));

            assert_eq!(
                reconcile_a_link_reading_another_stream(&streams.b, &link_id),
                HowALinkReadingAnotherStreamWasReconciled::WiredNow(HowAnOpenedLinkStands::Wired),
                "a source stream {status_of_the_source:?}"
            );
            let rendered = streams.the_link_as_graph_renders_it(&link_id);
            assert_eq!(rendered["state"], "wired", "{rendered}");
            streams
                .camera_output
                .write_raw("out1", b"a bag written once the stream a resumes", 0)
                .expect("the camera publishes");
            streams.display_input.receive_pending();
            assert_eq!(streams.display_input.drain("in1").len(), 1);
        }
    }

    /// Until its source can be wired a link says what it waits for: a
    /// stream not loaded, a stream not started or stopped, a node the stream
    /// does not hold, and a node that does not run yet.
    #[test]
    fn a_link_says_what_it_waits_for_until_its_source_can_be_wired() {
        let streams = TwoStreamsReadAsRunning::load();
        let reason_of = |source: OutputPortInAnotherStreamOnThisMachine| {
            let link_id = streams.link_the_display_from(source);
            let reconciled = reconcile_a_link_reading_another_stream(&streams.b, &link_id);
            let rendered = streams.the_link_as_graph_renders_it(&link_id);
            assert_eq!(rendered["state"], "awaiting_remote", "{rendered}");
            let HowALinkReadingAnotherStreamWasReconciled::AwaitingItsSource(reason) = reconciled
            else {
                panic!("the link is not awaiting its source: {reconciled:?}");
            };
            assert_eq!(rendered["awaiting_remote_reason"], reason.as_str());
            reason
        };

        assert_eq!(
            reason_of(camera_out1_of("nowhere")),
            "the stream `nowhere` is not loaded on this machine; the link wires when it loads"
        );

        let ghost =
            reason_of(OutputPortInAnotherStreamOnThisMachine::new("a", "ghost", "out1").unwrap());
        for named in ["`a`", "\"ghost\"", "camera"] {
            assert!(ghost.contains(named), "{named:?} not in: {ghost}");
        }

        streams
            .a
            .add_processor(
                ProcessorSpec::new(
                    MockOutputOnlyProcessor::processor_class_import_path(),
                    Value::Null,
                )
                .with_display_name("idle".to_string()),
            )
            .unwrap();
        streams
            .a
            .set_output_port_exposure_level("idle", "out1", OutputPortExposureLevel::Private)
            .unwrap();
        assert_eq!(
            reason_of(OutputPortInAnotherStreamOnThisMachine::new("a", "idle", "out1").unwrap()),
            "node `idle` of the stream `a` is not running yet; the link wires when it runs"
        );
        assert_eq!(
            TwoStreamsReadAsRunning::readers_registered_against_the_camera_of(&streams.a),
            0,
            "a link that waits leaves no reader on any port"
        );

        *streams.a.status.lock() = RuntimeStatus::Initial;
        assert_eq!(
            reason_of(camera_out1_of("a")),
            "the stream `a` has not started; the link wires when it starts"
        );

        *streams.a.status.lock() = RuntimeStatus::Stopped;
        assert_eq!(
            reason_of(camera_out1_of("a")),
            "the stream `a` has stopped; the link wires when it starts again"
        );
    }

    /// A port at `internal` refuses the link naming the port and its level,
    /// and a refusal is final: raising the level later wires nothing.
    #[test]
    fn a_link_to_an_internal_port_is_refused_and_stays_refused() {
        let streams = TwoStreamsReadAsRunning::load();
        let link_id = streams.link_the_display_from(camera_out1_of("a"));
        streams
            .a
            .set_output_port_exposure_level("camera", "out1", OutputPortExposureLevel::Internal)
            .unwrap();

        let reconciled = reconcile_a_link_reading_another_stream(&streams.b, &link_id);

        let HowALinkReadingAnotherStreamWasReconciled::Refused(refusal) = reconciled else {
            panic!("an internal port is not refused: {reconciled:?}");
        };
        for named in ["`a`", "`camera`", "`out1`", "internal"] {
            assert!(refusal.contains(named), "{named:?} not in: {refusal}");
        }
        let rendered = streams.the_link_as_graph_renders_it(&link_id);
        assert_eq!(rendered["state"], "error", "{rendered}");
        assert_eq!(rendered["error_reason"], refusal.as_str());

        streams
            .a
            .set_output_port_exposure_level("camera", "out1", OutputPortExposureLevel::Private)
            .unwrap();
        assert_eq!(
            reconcile_a_link_reading_another_stream(&streams.b, &link_id),
            HowALinkReadingAnotherStreamWasReconciled::NotThisReconcilesToWire
        );
        assert_eq!(streams.camera_output.channel_notifier_count("out1"), 0);
    }

    /// A wiring made against a load of the source stream that is gone is
    /// unwired — the old source's notifier and publisher released, its
    /// reader taken off — and the link waits; a new load of the stream is
    /// wired on the channel of its own source.
    #[test]
    fn a_reconcile_against_a_stale_load_unwires_then_rewires() {
        let streams = TwoStreamsReadAsRunning::load();
        let link_id = streams.link_the_display_from(camera_out1_of("a"));
        reconcile_a_link_reading_another_stream(&streams.b, &link_id);
        assert_eq!(streams.camera_output.channel_notifier_count("out1"), 1);

        streams
            .runner
            .unload_stream("a")
            .expect("the stream unloads");
        let reconciled = reconcile_a_link_reading_another_stream(&streams.b, &link_id);

        assert_eq!(
            reconciled,
            HowALinkReadingAnotherStreamWasReconciled::AwaitingItsSource(
                "the stream `a` is not loaded on this machine; the link wires when it loads"
                    .to_string()
            )
        );
        assert_eq!(streams.camera_output.channel_notifier_count("out1"), 0);
        assert!(!streams.camera_output.has_channel_publisher("out1"));
        assert_eq!(
            TwoStreamsReadAsRunning::readers_registered_against_the_camera_of(&streams.a),
            0
        );
        assert!(
            streams.display_input.inbound_link_names("in1").is_empty(),
            "the reading side of the old wiring is released"
        );

        let (reloaded_a, reloaded_camera_output) =
            TwoStreamsReadAsRunning::load_a_running_camera_stream(
                &streams.runner,
                &streams.project_directory,
            );
        assert_eq!(
            reconcile_a_link_reading_another_stream(&streams.b, &link_id),
            HowALinkReadingAnotherStreamWasReconciled::WiredNow(HowAnOpenedLinkStands::Wired)
        );
        assert_eq!(reloaded_camera_output.channel_notifier_count("out1"), 1);
        assert_eq!(
            TwoStreamsReadAsRunning::readers_registered_against_the_camera_of(&reloaded_a),
            1
        );
        reloaded_camera_output
            .write_raw("out1", b"a bag from the reloaded stream", 0)
            .unwrap();
        streams.display_input.receive_pending();
        assert_eq!(streams.display_input.drain("in1").len(), 1);
    }

    /// A link wired to one load of its source stream is refused, naming the
    /// port and its level, when the stream loads again with that port
    /// `internal` — whether or not it was reconciled while the stream was
    /// gone.
    ///
    /// Fail-without-fix: let any earlier wiring hold the link waiting and a
    /// reload whose port is `internal` leaves it `awaiting_remote` for good.
    #[test]
    fn a_link_wired_before_is_refused_when_its_source_stream_reloads_internal() {
        for reconciled_while_the_source_stream_was_unloaded in [true, false] {
            let streams = TwoStreamsReadAsRunning::load();
            let link_id = streams.link_the_display_from(camera_out1_of("a"));
            assert_eq!(
                reconcile_a_link_reading_another_stream(&streams.b, &link_id),
                HowALinkReadingAnotherStreamWasReconciled::WiredNow(HowAnOpenedLinkStands::Wired)
            );
            streams
                .runner
                .unload_stream("a")
                .expect("the stream unloads");
            if reconciled_while_the_source_stream_was_unloaded {
                reconcile_a_link_reading_another_stream(&streams.b, &link_id);
            }

            let reloaded_a = TwoStreamsReadAsRunning::load_a_stream_holding(
                &streams.runner,
                &streams.project_directory,
                "a",
                MockOutputOnlyProcessor::processor_class_import_path(),
                "camera",
            );
            let (reloaded_camera_output, _) =
                run_the_node::<MockOutputOnlyProcessor::Processor>(&reloaded_a, "camera");
            read_as_running(&reloaded_a);
            let reconciled = reconcile_a_link_reading_another_stream(&streams.b, &link_id);

            let HowALinkReadingAnotherStreamWasReconciled::Refused(refusal) = reconciled else {
                panic!(
                    "a reload at internal is not refused (reconciled while unloaded: \
                     {reconciled_while_the_source_stream_was_unloaded}): {reconciled:?}"
                );
            };
            for named in ["`a`", "`camera`", "`out1`", "internal"] {
                assert!(refusal.contains(named), "{named:?} not in: {refusal}");
            }
            let rendered = streams.the_link_as_graph_renders_it(&link_id);
            assert_eq!(rendered["state"], "error", "{rendered}");
            assert_eq!(rendered["error_reason"], refusal.as_str());
            assert_eq!(
                reloaded_camera_output
                    .expect("an output-only mock holds an output writer")
                    .channel_notifier_count("out1"),
                0
            );
        }
    }

    /// A link naming an output port its source node does not have waits for
    /// the stream to hold it, its reason naming the port and the node's
    /// output ports.
    #[test]
    fn a_link_naming_an_output_port_its_source_node_lacks_waits_naming_it() {
        let streams = TwoStreamsReadAsRunning::load();
        let link_id = streams.link_the_display_from(
            OutputPortInAnotherStreamOnThisMachine::new("a", "camera", "out9").unwrap(),
        );

        let reconciled = reconcile_a_link_reading_another_stream(&streams.b, &link_id);

        let HowALinkReadingAnotherStreamWasReconciled::AwaitingItsSource(reason) = reconciled
        else {
            panic!("a missing output port is not waited for: {reconciled:?}");
        };
        for named in ["`a`", "`camera`", "no output port `out9`", "out1"] {
            assert!(reason.contains(named), "{named:?} not in: {reason}");
        }
    }

    /// A commit's WIRE phase over a link that waits, a link that is refused
    /// and a link in the reading stream succeeds: the one in the stream is
    /// wired and carries bags, and the other two say why they carry nothing.
    ///
    /// Fail-without-fix: hand a waiting or refused link's outcome back as an
    /// error and the phase fails before the link in the stream is wired.
    #[test]
    fn an_awaiting_or_refused_link_never_fails_the_commit_it_is_wired_in() {
        let streams = TwoStreamsReadAsRunning::load();
        let in_stream_source_id = TwoStreamsReadAsRunning::add_a_node_to(
            &streams.b,
            MockOutputOnlyProcessor::processor_class_import_path(),
            "pattern",
        );
        let (pattern_output, _) =
            run_the_node::<MockOutputOnlyProcessor::Processor>(&streams.b, "pattern");
        let pattern_output = pattern_output.expect("an output-only mock holds an output writer");
        streams
            .a
            .set_output_port_exposure_level("camera", "out1", OutputPortExposureLevel::Internal)
            .unwrap();
        let refused = streams.link_the_display_from(camera_out1_of("a"));
        let awaiting = streams.link_the_display_from(camera_out1_of("nowhere"));
        let display = streams.b.the_node_named("display").unwrap();
        let in_stream = streams
            .b
            .connect(
                OutputLinkPortRef::new(in_stream_source_id, "out1"),
                InputLinkPortRef::new(display.processor_id, "in1"),
            )
            .expect("a link in the stream is added");

        let links_wired = streams.b.compiler.wire_every_link_a_plan_adds_for_a_test(
            &[refused.clone(), awaiting.clone(), in_stream.clone()],
            &streams.b,
        );

        assert_eq!(
            links_wired.expect("a link that waits or is refused never fails the commit"),
            1
        );
        let refused = streams.the_link_as_graph_renders_it(&refused);
        assert_eq!(refused["state"], "error", "{refused}");
        assert!(
            refused["error_reason"]
                .as_str()
                .is_some_and(|reason| reason.contains("internal")),
            "{refused}"
        );
        let awaiting = streams.the_link_as_graph_renders_it(&awaiting);
        assert_eq!(awaiting["state"], "awaiting_remote", "{awaiting}");
        assert_eq!(
            awaiting["awaiting_remote_reason"],
            "the stream `nowhere` is not loaded on this machine; the link wires when it loads"
        );
        let in_stream = streams.the_link_as_graph_renders_it(&in_stream);
        assert_eq!(in_stream["state"], "wired", "{in_stream}");
        pattern_output
            .write_raw("out1", b"a bag of the reading stream's own", 0)
            .unwrap();
        streams.display_input.receive_pending();
        assert_eq!(streams.display_input.drain("in1").len(), 1);
    }

    /// Closing a link reading another stream releases the source's side —
    /// the link's notifier and the publisher on the source's port — while the
    /// source stream's graph lock is held elsewhere.
    ///
    /// Fail-without-fix: release through the source's processor in the
    /// source stream's graph, as a close of a link in one stream does, and the
    /// close waits on that lock past the bound.
    #[test]
    fn releasing_the_source_side_takes_no_lock_of_the_source_graph() {
        let streams = TwoStreamsReadAsRunning::load();
        let link_id = streams.link_the_display_from(camera_out1_of("a"));
        assert_eq!(
            reconcile_a_link_reading_another_stream(&streams.b, &link_id),
            HowALinkReadingAnotherStreamWasReconciled::WiredNow(HowAnOpenedLinkStands::Wired)
        );
        assert_eq!(streams.camera_output.channel_notifier_count("out1"), 1);

        let (closed_while_the_source_graph_was_held, notifiers_while_it_was_held) =
            std::thread::scope(|scope| {
                let (held_tx, held_rx) = std::sync::mpsc::channel();
                let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
                let source_stream = &streams.a;
                scope.spawn(move || {
                    source_stream.compiler.scope(|_graph, _tx| {
                        let _ = held_tx.send(());
                        let _ = release_rx.recv();
                    })
                });
                held_rx.recv().expect("the source's graph is held");
                let (closed_tx, closed_rx) = std::sync::mpsc::channel();
                let reading_stream = &streams.b;
                let link_id = &link_id;
                scope.spawn(move || {
                    reading_stream
                        .compiler
                        .scope(|graph, _tx| {
                            crate::core::compiler::compiler_ops::close_iceoryx2_service(
                                graph, link_id,
                            )
                        })
                        .expect("the link closes");
                    let _ = closed_tx.send(());
                });
                let closed = closed_rx.recv_timeout(A_RELEASE_RETURNS_WITHIN).is_ok();
                let notifiers = streams.camera_output.channel_notifier_count("out1");
                let _ = release_tx.send(());
                (closed, notifiers)
            });

        assert!(
            closed_while_the_source_graph_was_held,
            "the close waited on the source stream's graph lock"
        );
        assert_eq!(notifiers_while_it_was_held, 0);
        assert!(!streams.camera_output.has_channel_publisher("out1"));
        assert_eq!(
            TwoStreamsReadAsRunning::readers_registered_against_the_camera_of(&streams.a),
            0
        );
    }

    /// A source stream whose graph is held past the bound leaves the link
    /// awaiting it rather than holding up the reading stream's commit.
    #[test]
    fn a_source_stream_that_does_not_answer_leaves_the_link_awaiting_it() {
        let streams = TwoStreamsReadAsRunning::load();
        let link_id = streams.link_the_display_from(camera_out1_of("a"));

        let reconciled = std::thread::scope(|scope| {
            let (held_tx, held_rx) = std::sync::mpsc::channel();
            let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
            let source_stream = &streams.a;
            scope.spawn(move || {
                source_stream.compiler.scope(|_graph, _tx| {
                    let _ = held_tx.send(());
                    let _ = release_rx.recv();
                })
            });
            held_rx.recv().expect("the source's graph is held");
            let reconciled = reconcile_a_link_reading_another_stream(&streams.b, &link_id);
            let _ = release_tx.send(());
            reconciled
        });

        let HowALinkReadingAnotherStreamWasReconciled::AwaitingItsSource(reason) = reconciled
        else {
            panic!("the link is not awaiting its source: {reconciled:?}");
        };
        assert!(
            reason.starts_with("the stream `a` did not answer within"),
            "{reason}"
        );
        assert_eq!(
            TwoStreamsReadAsRunning::readers_registered_against_the_camera_of(&streams.a),
            0
        );
    }

    /// Lowering the source port to `internal` cuts the link: its stream
    /// disconnects it, and the reader leaves the port.
    #[test]
    fn lowering_the_source_port_to_internal_disconnects_the_link() {
        let streams = TwoStreamsReadAsRunning::load();
        let link_id = streams.link_the_display_from(camera_out1_of("a"));
        reconcile_a_link_reading_another_stream(&streams.b, &link_id);
        let (_listener, disconnected) = listen_on(&streams.b, |event| {
            matches!(event, RuntimeEvent::RuntimeDidDisconnect { .. })
        });

        streams
            .a
            .set_output_port_exposure_level("camera", "out1", OutputPortExposureLevel::Internal)
            .unwrap();

        let RuntimeEvent::RuntimeDidDisconnect {
            link_id: disconnected_link_id,
            ..
        } = disconnected
            .recv_timeout(A_CUT_LANDS_WITHIN)
            .expect("the cut disconnects the link")
        else {
            unreachable!("the listener forwards disconnects alone");
        };
        assert_eq!(disconnected_link_id, link_id.as_str());
        assert!(
            streams
                .b
                .compiler
                .logged_pending_operations()
                .iter()
                .any(|operation| matches!(
                    operation,
                    crate::core::compiler::PendingOperation::RemoveLink(removed)
                        if *removed == link_id
                )),
            "the reading stream logs the link's removal"
        );
        assert_eq!(
            TwoStreamsReadAsRunning::readers_registered_against_the_camera_of(&streams.a),
            0
        );
    }

    /// A source node that left its stream cuts the link back to waiting: the
    /// next reconcile unwires it and names the node it waits for.
    #[test]
    fn a_source_node_leaving_its_stream_returns_the_link_to_awaiting_it() {
        let streams = TwoStreamsReadAsRunning::load();
        let link_id = streams.link_the_display_from(camera_out1_of("a"));
        reconcile_a_link_reading_another_stream(&streams.b, &link_id);
        streams.remove_the_camera_cutting_its_readers();

        let reconciled = reconcile_a_link_reading_another_stream(&streams.b, &link_id);

        let HowALinkReadingAnotherStreamWasReconciled::AwaitingItsSource(reason) = reconciled
        else {
            panic!("the link is not awaiting its source: {reconciled:?}");
        };
        assert!(reason.contains("\"camera\""), "{reason}");
        assert_eq!(streams.camera_output.channel_notifier_count("out1"), 0);
        assert!(streams.display_input.inbound_link_names("in1").is_empty());
    }

    /// A source node that left its stream and came back starts with its port
    /// `internal`: the link it was wired to waits for the port to open again,
    /// naming the port and its level, rather than being refused, and opening
    /// the port re-plans it onto the new node's channel.
    ///
    /// Fail-without-fix: map every `internal` port to a refusal and the
    /// re-added node's link reads `error` for good; leave the level change
    /// without a re-plan and the waiting link is never reconciled again.
    #[test]
    fn a_source_node_re_added_internal_holds_the_link_awaiting_its_port_until_it_opens() {
        let streams = TwoStreamsReadAsRunning::load();
        let link_id = streams.link_the_display_from(camera_out1_of("a"));
        reconcile_a_link_reading_another_stream(&streams.b, &link_id);
        streams.remove_the_camera_cutting_its_readers();
        reconcile_a_link_reading_another_stream(&streams.b, &link_id);

        TwoStreamsReadAsRunning::add_a_node_to(
            &streams.a,
            MockOutputOnlyProcessor::processor_class_import_path(),
            "camera",
        );
        let (re_added_camera_output, _) =
            run_the_node::<MockOutputOnlyProcessor::Processor>(&streams.a, "camera");
        let re_added_camera_output =
            re_added_camera_output.expect("an output-only mock holds an output writer");
        let reconciled = reconcile_a_link_reading_another_stream(&streams.b, &link_id);

        let HowALinkReadingAnotherStreamWasReconciled::AwaitingItsSource(reason) = reconciled
        else {
            panic!("the link is not awaiting the re-added node's port: {reconciled:?}");
        };
        for named in ["`a`", "`camera`", "`out1`", "internal"] {
            assert!(reason.contains(named), "{named:?} not in: {reason}");
        }
        let rendered = streams.the_link_as_graph_renders_it(&link_id);
        assert_eq!(rendered["state"], "awaiting_remote", "{rendered}");
        assert_eq!(rendered["awaiting_remote_reason"], reason.as_str());
        assert_eq!(
            TwoStreamsReadAsRunning::readers_registered_against_the_camera_of(&streams.a),
            0
        );

        let (_listener, changed) = listen_on(&streams.b, |event| {
            matches!(event, RuntimeEvent::GraphDidChange)
        });
        streams
            .a
            .set_output_port_exposure_level("camera", "out1", OutputPortExposureLevel::Private)
            .unwrap();
        changed
            .recv_timeout(A_CUT_LANDS_WITHIN)
            .expect("opening the port re-plans the links reading `a`");
        assert!(
            streams
                .b
                .compiler
                .logged_pending_operations()
                .iter()
                .any(|operation| matches!(
                    operation,
                    crate::core::compiler::PendingOperation::AddLink(added) if *added == link_id
                )),
            "opening the port queues the waiting link to be wired"
        );

        assert_eq!(
            reconcile_a_link_reading_another_stream(&streams.b, &link_id),
            HowALinkReadingAnotherStreamWasReconciled::WiredNow(HowAnOpenedLinkStands::Wired)
        );
        assert_eq!(re_added_camera_output.channel_notifier_count("out1"), 1);
        re_added_camera_output
            .write_raw("out1", b"a bag from the re-added camera", 0)
            .unwrap();
        streams.display_input.receive_pending();
        assert_eq!(streams.display_input.drain("in1").len(), 1);
    }

    /// Opening a port re-plans the links reading its stream only when the
    /// port was closed to them: a port already open queues nothing.
    #[test]
    fn opening_a_port_already_open_to_other_streams_re_plans_nothing() {
        let streams = TwoStreamsReadAsRunning::load();
        let link_id = streams.link_the_display_from(camera_out1_of("a"));
        reconcile_a_link_reading_another_stream(&streams.b, &link_id);
        let logged_before = streams.b.compiler.logged_pending_operations().len();
        let (_listener, changed) = listen_on(&streams.b, |event| {
            matches!(event, RuntimeEvent::GraphDidChange)
        });

        streams
            .a
            .set_output_port_exposure_level("camera", "out1", OutputPortExposureLevel::Public)
            .unwrap();

        assert!(
            changed.recv_timeout(Duration::from_millis(500)).is_err(),
            "a port already open re-planned the links reading its stream"
        );
        assert_eq!(
            streams.b.compiler.logged_pending_operations().len(),
            logged_before
        );
    }
}
