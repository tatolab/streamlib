// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::core::graph::{
    OutputPortExposureLevel, OutputPortReaderOutsideItsStream,
    output_port_exposure_allows_the_reader,
};

/// The output ports of this node its stream exposes, each at its level, with
/// the readers from outside the stream registered against it.
///
/// Held on the node so a removed node takes its exposures and their readers
/// with it. Stored but never rendered under the node's `components`: `graph`
/// renders every exposure once, in its top-level `exposed`. An internal port
/// has no entry. Every reader it lets go is handed back to the caller, so a
/// reader's cut is never run or dropped under the graph's lock.
#[derive(Debug, Default)]
pub struct ExposedOutputPortsComponent {
    exposed_ports_by_port_name: BTreeMap<String, ExposedOutputPortOfThisNode>,
}

#[derive(Debug)]
struct ExposedOutputPortOfThisNode {
    level: OutputPortExposureLevel,
    readers_from_outside_the_stream: Vec<ReaderOfAnExposedOutputPort>,
}

/// What cuts one reader off the port it reads. It runs at most once, outside
/// every graph lock.
pub type CutOffAReaderOfAnExposedOutputPort = Box<dyn FnOnce() + Send + Sync>;

/// One reader from outside a port's stream, registered against the port.
pub struct ReaderOfAnExposedOutputPort {
    still_registered: Arc<AtomicBool>,
    location: OutputPortReaderOutsideItsStream,
    cut_off: CutOffAReaderOfAnExposedOutputPort,
}

impl std::fmt::Debug for ReaderOfAnExposedOutputPort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReaderOfAnExposedOutputPort")
            .field("still_registered", &self.is_still_registered())
            .field("location", &self.location)
            .finish_non_exhaustive()
    }
}

impl ReaderOfAnExposedOutputPort {
    /// A reader at `location`, registered while `still_registered` holds.
    pub(crate) fn new(
        still_registered: Arc<AtomicBool>,
        location: OutputPortReaderOutsideItsStream,
        cut_off: CutOffAReaderOfAnExposedOutputPort,
    ) -> Self {
        Self {
            still_registered,
            location,
            cut_off,
        }
    }

    /// Where the reader reads the port from.
    pub fn location(&self) -> OutputPortReaderOutsideItsStream {
        self.location
    }

    /// Whether the reader's registration has not been dropped.
    pub fn is_still_registered(&self) -> bool {
        self.still_registered.load(Ordering::Acquire)
    }

    /// Cut the reader off its port, unless its registration was dropped first.
    pub(crate) fn cut_off_unless_its_registration_was_dropped(self) {
        if self.is_still_registered() {
            (self.cut_off)();
        }
    }
}

impl ExposedOutputPortsComponent {
    /// The level of output port `port_name`: internal unless the stream
    /// exposes it.
    pub fn level_of(&self, port_name: &str) -> OutputPortExposureLevel {
        self.exposed_ports_by_port_name
            .get(port_name)
            .map_or(OutputPortExposureLevel::Internal, |exposed| exposed.level)
    }

    /// Every exposed port of this node with its level, by port name.
    pub fn exposed_ports_and_their_levels(
        &self,
    ) -> impl Iterator<Item = (&str, OutputPortExposureLevel)> {
        self.exposed_ports_by_port_name
            .iter()
            .map(|(port_name, exposed)| (port_name.as_str(), exposed.level))
    }

    /// Put `port_name` at `level`, handing back every reader that leaves the
    /// port: each the new level no longer allows, and each whose registration
    /// was dropped. The caller cuts them off once it has let go of the graph.
    pub(crate) fn set_level(
        &mut self,
        port_name: &str,
        level: OutputPortExposureLevel,
    ) -> Vec<ReaderOfAnExposedOutputPort> {
        if level == OutputPortExposureLevel::Internal {
            return self
                .exposed_ports_by_port_name
                .remove(port_name)
                .map(|exposed| exposed.readers_from_outside_the_stream)
                .unwrap_or_default();
        }
        let exposed = self
            .exposed_ports_by_port_name
            .entry(port_name.to_string())
            .or_insert_with(|| ExposedOutputPortOfThisNode {
                level,
                readers_from_outside_the_stream: Vec::new(),
            });
        exposed.level = level;
        let (staying, leaving) = std::mem::take(&mut exposed.readers_from_outside_the_stream)
            .into_iter()
            .partition(|reader| {
                reader.is_still_registered()
                    && output_port_exposure_allows_the_reader(level, reader.location.location())
            });
        exposed.readers_from_outside_the_stream = staying;
        leaving
    }

    /// Register `reader` against `port_name`, handing back every reader whose
    /// registration was dropped since, for the caller to drop once it has let
    /// go of the graph; or hand `reader` back with the port's level when that
    /// level does not allow a reader where it reads from.
    pub(crate) fn register_reader(
        &mut self,
        port_name: &str,
        reader: ReaderOfAnExposedOutputPort,
    ) -> std::result::Result<
        Vec<ReaderOfAnExposedOutputPort>,
        (ReaderOfAnExposedOutputPort, OutputPortExposureLevel),
    > {
        let level = self.level_of(port_name);
        let Some(exposed) = self
            .exposed_ports_by_port_name
            .get_mut(port_name)
            .filter(|_| output_port_exposure_allows_the_reader(level, reader.location.location()))
        else {
            return Err((reader, level));
        };
        let (still_registered, registration_dropped) =
            std::mem::take(&mut exposed.readers_from_outside_the_stream)
                .into_iter()
                .partition(ReaderOfAnExposedOutputPort::is_still_registered);
        exposed.readers_from_outside_the_stream = still_registered;
        exposed.readers_from_outside_the_stream.push(reader);
        Ok(registration_dropped)
    }

    /// How many readers from outside the stream are registered against
    /// `port_name`.
    pub fn readers_registered_against(&self, port_name: &str) -> usize {
        self.exposed_ports_by_port_name
            .get(port_name)
            .map_or(0, |exposed| {
                exposed
                    .readers_from_outside_the_stream
                    .iter()
                    .filter(|reader| reader.is_still_registered())
                    .count()
            })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;

    use super::*;

    fn a_reader_counting_its_cuts_into(
        location: OutputPortReaderOutsideItsStream,
        cuts: &Arc<AtomicUsize>,
    ) -> (ReaderOfAnExposedOutputPort, Arc<AtomicBool>) {
        let still_registered = Arc::new(AtomicBool::new(true));
        let cuts = Arc::clone(cuts);
        let reader = ReaderOfAnExposedOutputPort::new(
            Arc::clone(&still_registered),
            location,
            Box::new(move || {
                cuts.fetch_add(1, Ordering::SeqCst);
            }),
        );
        (reader, still_registered)
    }

    fn cut_off_every(readers: Vec<ReaderOfAnExposedOutputPort>) {
        readers
            .into_iter()
            .for_each(ReaderOfAnExposedOutputPort::cut_off_unless_its_registration_was_dropped);
    }

    #[test]
    fn a_port_with_no_entry_is_internal_and_refuses_a_reader_from_outside_the_stream() {
        let mut exposed_ports = ExposedOutputPortsComponent::default();
        let cuts = Arc::new(AtomicUsize::new(0));
        let (reader, _) = a_reader_counting_its_cuts_into(
            OutputPortReaderOutsideItsStream::ElsewhereOnThisMachine,
            &cuts,
        );

        let refused = exposed_ports.register_reader("video", reader);

        assert_eq!(
            exposed_ports.level_of("video"),
            OutputPortExposureLevel::Internal
        );
        assert!(matches!(
            refused,
            Err((_, OutputPortExposureLevel::Internal))
        ));
        assert_eq!(
            cuts.load(Ordering::SeqCst),
            0,
            "a refused reader is never cut"
        );
    }

    #[test]
    fn lowering_a_level_hands_back_exactly_the_readers_it_no_longer_allows() {
        let mut exposed_ports = ExposedOutputPortsComponent::default();
        exposed_ports.set_level("video", OutputPortExposureLevel::Public);
        let on_this_machine = Arc::new(AtomicUsize::new(0));
        let off_this_machine = Arc::new(AtomicUsize::new(0));
        for (location, cuts) in [
            (
                OutputPortReaderOutsideItsStream::ElsewhereOnThisMachine,
                &on_this_machine,
            ),
            (
                OutputPortReaderOutsideItsStream::OnAnotherMachine,
                &off_this_machine,
            ),
        ] {
            let (reader, _) = a_reader_counting_its_cuts_into(location, cuts);
            assert!(exposed_ports.register_reader("video", reader).is_ok());
        }

        let leaving_at_private = exposed_ports.set_level("video", OutputPortExposureLevel::Private);
        assert_eq!(leaving_at_private.len(), 1);
        cut_off_every(leaving_at_private);
        assert_eq!(off_this_machine.load(Ordering::SeqCst), 1);
        assert_eq!(on_this_machine.load(Ordering::SeqCst), 0);
        assert_eq!(exposed_ports.readers_registered_against("video"), 1);

        cut_off_every(exposed_ports.set_level("video", OutputPortExposureLevel::Internal));
        assert_eq!(on_this_machine.load(Ordering::SeqCst), 1);
        assert_eq!(off_this_machine.load(Ordering::SeqCst), 1);
        assert_eq!(
            exposed_ports.exposed_ports_and_their_levels().count(),
            0,
            "an internal port has no entry"
        );
    }

    #[test]
    fn raising_a_level_cuts_no_reader() {
        let mut exposed_ports = ExposedOutputPortsComponent::default();
        exposed_ports.set_level("video", OutputPortExposureLevel::Private);
        let cuts = Arc::new(AtomicUsize::new(0));
        let (reader, _) = a_reader_counting_its_cuts_into(
            OutputPortReaderOutsideItsStream::ElsewhereOnThisMachine,
            &cuts,
        );
        assert!(exposed_ports.register_reader("video", reader).is_ok());

        assert!(
            exposed_ports
                .set_level("video", OutputPortExposureLevel::Public)
                .is_empty()
        );
        assert_eq!(exposed_ports.readers_registered_against("video"), 1);
    }

    #[test]
    fn a_reader_whose_registration_was_dropped_is_never_cut_and_leaves_at_the_next_change() {
        let mut exposed_ports = ExposedOutputPortsComponent::default();
        exposed_ports.set_level("video", OutputPortExposureLevel::Private);
        let cuts = Arc::new(AtomicUsize::new(0));
        let (reader, still_registered) = a_reader_counting_its_cuts_into(
            OutputPortReaderOutsideItsStream::ElsewhereOnThisMachine,
            &cuts,
        );
        assert!(exposed_ports.register_reader("video", reader).is_ok());

        still_registered.store(false, Ordering::Release);
        assert_eq!(exposed_ports.readers_registered_against("video"), 0);
        let leaving = exposed_ports.set_level("video", OutputPortExposureLevel::Public);
        assert_eq!(
            leaving.len(),
            1,
            "a dropped registration's reader leaves the port"
        );
        cut_off_every(leaving);

        assert_eq!(cuts.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_registration_hands_back_the_readers_whose_registrations_were_dropped() {
        let mut exposed_ports = ExposedOutputPortsComponent::default();
        exposed_ports.set_level("video", OutputPortExposureLevel::Private);
        let cuts = Arc::new(AtomicUsize::new(0));
        let (first, first_still_registered) = a_reader_counting_its_cuts_into(
            OutputPortReaderOutsideItsStream::ElsewhereOnThisMachine,
            &cuts,
        );
        assert!(exposed_ports.register_reader("video", first).is_ok());
        first_still_registered.store(false, Ordering::Release);

        let (second, _) = a_reader_counting_its_cuts_into(
            OutputPortReaderOutsideItsStream::ElsewhereOnThisMachine,
            &cuts,
        );
        let registration_dropped = exposed_ports
            .register_reader("video", second)
            .expect("a private port takes a reader on this machine");

        assert_eq!(registration_dropped.len(), 1);
        assert_eq!(exposed_ports.readers_registered_against("video"), 1);
    }
}
