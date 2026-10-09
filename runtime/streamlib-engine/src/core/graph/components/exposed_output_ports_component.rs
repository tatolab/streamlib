// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::collections::BTreeMap;

use crate::core::graph::{
    OutputPortExposureLevel, OutputPortReaderLocation, output_port_exposure_allows_the_reader,
};

/// The output ports of this node its stream exposes, each at its level, with
/// the readers from outside the stream registered against it.
///
/// Held on the node so a removed node takes its exposures and their readers
/// with it. Stored but never rendered under the node's `components`: `graph`
/// renders every exposure once, in its top-level `exposed`. An internal port
/// has no entry.
#[derive(Default)]
pub struct ExposedOutputPortsComponent {
    exposed_ports_by_port_name: BTreeMap<String, ExposedOutputPortOfThisNode>,
}

struct ExposedOutputPortOfThisNode {
    level: OutputPortExposureLevel,
    readers_from_outside_the_stream: Vec<ReaderOfAnExposedOutputPort>,
}

/// What cuts one reader off the port it reads. It runs once, after the port's
/// stream has let go of its graph, and is dropped unrun when the port's node
/// is removed.
pub type CutOffAReaderOfAnExposedOutputPort = Box<dyn FnOnce() + Send + Sync>;

/// One reader from outside a port's stream, registered against the port.
pub struct ReaderOfAnExposedOutputPort {
    registration_id: u64,
    location: OutputPortReaderLocation,
    cut_off: CutOffAReaderOfAnExposedOutputPort,
}

impl ReaderOfAnExposedOutputPort {
    pub(crate) fn new(
        registration_id: u64,
        location: OutputPortReaderLocation,
        cut_off: CutOffAReaderOfAnExposedOutputPort,
    ) -> Self {
        Self {
            registration_id,
            location,
            cut_off,
        }
    }

    /// Where the reader reads the port from.
    pub fn location(&self) -> OutputPortReaderLocation {
        self.location
    }

    /// Cut the reader off its port.
    pub(crate) fn cut_off(self) {
        (self.cut_off)();
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

    /// Put `port_name` at `level`, handing back every registered reader the
    /// new level no longer allows, for the caller to cut off once it has let
    /// go of the graph.
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
        let (still_allowed, no_longer_allowed) =
            std::mem::take(&mut exposed.readers_from_outside_the_stream)
                .into_iter()
                .partition(|reader| output_port_exposure_allows_the_reader(level, reader.location));
        exposed.readers_from_outside_the_stream = still_allowed;
        no_longer_allowed
    }

    /// Register `reader` against `port_name`, or hand it back with the port's
    /// level when that level does not allow a reader where it reads from.
    pub(crate) fn register_reader(
        &mut self,
        port_name: &str,
        reader: ReaderOfAnExposedOutputPort,
    ) -> std::result::Result<(), (ReaderOfAnExposedOutputPort, OutputPortExposureLevel)> {
        let level = self.level_of(port_name);
        match self.exposed_ports_by_port_name.get_mut(port_name) {
            Some(exposed) if output_port_exposure_allows_the_reader(level, reader.location) => {
                exposed.readers_from_outside_the_stream.push(reader);
                Ok(())
            }
            _ => Err((reader, level)),
        }
    }

    /// Take the reader registered under `registration_id` off `port_name`,
    /// if it is still there.
    pub(crate) fn forget_reader(
        &mut self,
        port_name: &str,
        registration_id: u64,
    ) -> Option<ReaderOfAnExposedOutputPort> {
        let readers = &mut self
            .exposed_ports_by_port_name
            .get_mut(port_name)?
            .readers_from_outside_the_stream;
        let position = readers
            .iter()
            .position(|reader| reader.registration_id == registration_id)?;
        Some(readers.remove(position))
    }

    /// How many readers from outside the stream are registered against
    /// `port_name`.
    pub fn readers_registered_against(&self, port_name: &str) -> usize {
        self.exposed_ports_by_port_name
            .get(port_name)
            .map_or(0, |exposed| exposed.readers_from_outside_the_stream.len())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    fn a_reader_counting_its_cuts_into(
        registration_id: u64,
        location: OutputPortReaderLocation,
        cuts: &Arc<AtomicUsize>,
    ) -> ReaderOfAnExposedOutputPort {
        let cuts = Arc::clone(cuts);
        ReaderOfAnExposedOutputPort::new(
            registration_id,
            location,
            Box::new(move || {
                cuts.fetch_add(1, Ordering::SeqCst);
            }),
        )
    }

    #[test]
    fn a_port_with_no_entry_is_internal_and_refuses_a_reader_from_outside_the_stream() {
        let mut exposed_ports = ExposedOutputPortsComponent::default();
        let cuts = Arc::new(AtomicUsize::new(0));

        let refused = exposed_ports.register_reader(
            "video",
            a_reader_counting_its_cuts_into(
                1,
                OutputPortReaderLocation::ElsewhereOnThisMachine,
                &cuts,
            ),
        );

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
        for (registration_id, location, cuts) in [
            (
                1,
                OutputPortReaderLocation::ElsewhereOnThisMachine,
                &on_this_machine,
            ),
            (
                2,
                OutputPortReaderLocation::OnAnotherMachine,
                &off_this_machine,
            ),
        ] {
            assert!(
                exposed_ports
                    .register_reader(
                        "video",
                        a_reader_counting_its_cuts_into(registration_id, location, cuts)
                    )
                    .is_ok()
            );
        }

        let cut_at_private = exposed_ports.set_level("video", OutputPortExposureLevel::Private);
        assert_eq!(cut_at_private.len(), 1);
        cut_at_private
            .into_iter()
            .for_each(ReaderOfAnExposedOutputPort::cut_off);
        assert_eq!(off_this_machine.load(Ordering::SeqCst), 1);
        assert_eq!(on_this_machine.load(Ordering::SeqCst), 0);
        assert_eq!(exposed_ports.readers_registered_against("video"), 1);

        let cut_at_internal = exposed_ports.set_level("video", OutputPortExposureLevel::Internal);
        cut_at_internal
            .into_iter()
            .for_each(ReaderOfAnExposedOutputPort::cut_off);
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
        assert!(
            exposed_ports
                .register_reader(
                    "video",
                    a_reader_counting_its_cuts_into(
                        1,
                        OutputPortReaderLocation::ElsewhereOnThisMachine,
                        &cuts
                    )
                )
                .is_ok()
        );

        assert!(
            exposed_ports
                .set_level("video", OutputPortExposureLevel::Public)
                .is_empty()
        );
        assert_eq!(exposed_ports.readers_registered_against("video"), 1);
    }

    #[test]
    fn a_forgotten_reader_is_never_cut() {
        let mut exposed_ports = ExposedOutputPortsComponent::default();
        exposed_ports.set_level("video", OutputPortExposureLevel::Private);
        let cuts = Arc::new(AtomicUsize::new(0));
        assert!(
            exposed_ports
                .register_reader(
                    "video",
                    a_reader_counting_its_cuts_into(
                        7,
                        OutputPortReaderLocation::ElsewhereOnThisMachine,
                        &cuts
                    )
                )
                .is_ok()
        );

        assert!(exposed_ports.forget_reader("video", 7).is_some());
        assert!(
            exposed_ports
                .set_level("video", OutputPortExposureLevel::Internal)
                .is_empty()
        );
        assert_eq!(cuts.load(Ordering::SeqCst), 0);
    }
}
