// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Which of this runtime's output ports are being sent over the mesh, and to
//! whom.
//!
//! Written by the egress thread as reader tokens come and go, read by `graph`.
//! No network call ever happens while the table is held, so a `graph` never
//! waits on a peer — the peer table's rule, applied to the sending side.
//!
//! One entry per *live egress*, never per reader: a runtime reading a port
//! this one cannot send has no egress, and `graph` reporting it would say this
//! runtime is sending something it is not.

use std::collections::{BTreeMap, BTreeSet};

use parking_lot::RwLock;

use crate::core::json_schema::MeshEgressPortOutput;
use crate::core::runtime::mesh::output_ports_offered_on_the_mesh::OutputPortOfferedOnTheMesh;

/// The ports this runtime is currently sending, each with its readers.
#[derive(Debug, Default)]
pub(super) struct OutputPortsOtherRuntimesAreReading {
    sending: RwLock<BTreeMap<OutputPortOfferedOnTheMesh, BTreeSet<String>>>,
}

impl OutputPortsOtherRuntimesAreReading {
    /// Replace what is being sent with `sending`.
    ///
    /// Written whole rather than amended per event, because the egress thread
    /// already holds both halves — which ports have an egress and who is
    /// reading each — and deriving the table from them at each event is one
    /// obviously correct statement where two incremental updates are two
    /// chances to drift.
    pub(super) fn record_what_is_being_sent(
        &self,
        sending: BTreeMap<OutputPortOfferedOnTheMesh, BTreeSet<String>>,
    ) {
        *self.sending.write() = sending;
    }

    /// Every port this runtime is sending, as `graph` renders it.
    pub(super) fn render_for_graph(&self) -> Vec<MeshEgressPortOutput> {
        self.sending
            .read()
            .iter()
            .map(|(port, reader_runtime_names)| MeshEgressPortOutput {
                processor_display_name: port.processor_display_name.clone(),
                port_name: port.port_name.clone(),
                reader_runtime_names: reader_runtime_names.iter().cloned().collect(),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a_port(processor_display_name: &str, port_name: &str) -> OutputPortOfferedOnTheMesh {
        OutputPortOfferedOnTheMesh {
            processor_display_name: processor_display_name.to_string(),
            port_name: port_name.to_string(),
        }
    }

    /// A runtime sending nothing renders an empty list, which is how a reader
    /// tells "nobody is pulling from me" from "this engine predates the key".
    #[test]
    fn a_runtime_sending_nothing_renders_no_entries() {
        assert_eq!(
            OutputPortsOtherRuntimesAreReading::default().render_for_graph(),
            Vec::new()
        );
    }

    /// Each entry names its port and every runtime reading it, both in a
    /// stable order so two `graph` reads of one state agree.
    #[test]
    fn each_port_renders_its_readers_in_a_stable_order() {
        let table = OutputPortsOtherRuntimesAreReading::default();
        table.record_what_is_being_sent(BTreeMap::from([
            (
                a_port("CameraSource", "video"),
                BTreeSet::from(["bench-fx-c3d4".to_string(), "bench-rec-e5f6".to_string()]),
            ),
            (
                a_port("MicrophoneSource", "audio"),
                BTreeSet::from(["bench-rec-e5f6".to_string()]),
            ),
        ]));

        let rendered = table.render_for_graph();
        assert_eq!(
            rendered
                .iter()
                .map(|entry| (
                    entry.processor_display_name.as_str(),
                    entry.port_name.as_str()
                ))
                .collect::<Vec<_>>(),
            vec![("CameraSource", "video"), ("MicrophoneSource", "audio")]
        );
        assert_eq!(
            rendered[0].reader_runtime_names,
            vec!["bench-fx-c3d4".to_string(), "bench-rec-e5f6".to_string()]
        );
        assert_eq!(
            rendered[1].reader_runtime_names,
            vec!["bench-rec-e5f6".to_string()]
        );
    }

    /// The last reader of a port leaving takes the whole entry, because the
    /// egress goes with it and `graph` must stop saying this runtime sends it.
    #[test]
    fn a_port_whose_egress_stopped_is_gone_rather_than_rendered_with_no_readers() {
        let table = OutputPortsOtherRuntimesAreReading::default();
        table.record_what_is_being_sent(BTreeMap::from([(
            a_port("CameraSource", "video"),
            BTreeSet::from(["bench-fx-c3d4".to_string()]),
        )]));
        assert_eq!(table.render_for_graph().len(), 1);

        table.record_what_is_being_sent(BTreeMap::new());

        assert_eq!(table.render_for_graph(), Vec::new());
    }
}
