// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::collections::HashMap;
use std::sync::Arc;

use crate::iceoryx2::function_node_island_input_port::{
    FunctionNodeIslandEvaluatedAtEachRead, InputPortFedByAFunctionNodeIsland,
    PinValueReadByAnUnlinkedInputPort,
};

use super::InputMailboxesInner;

impl InputMailboxesInner {
    /// Feed `port` from `island`, evaluated at each read of the port in place
    /// of a mailbox; an island already feeding `port` is replaced, and the
    /// port's sample positions carry on from it.
    pub fn feed_an_input_port_from_a_function_node_island(
        &self,
        port: &str,
        island: Arc<dyn FunctionNodeIslandEvaluatedAtEachRead>,
    ) {
        let mut input_ports_fed_by_function_node_islands =
            self.input_ports_fed_by_function_node_islands.write();
        let island_port = match input_ports_fed_by_function_node_islands.get(port) {
            Some(island_port_it_replaces) => {
                InputPortFedByAFunctionNodeIsland::evaluated_by_in_place_of(
                    island,
                    island_port_it_replaces,
                )
            }
            None => InputPortFedByAFunctionNodeIsland::evaluated_by(island),
        };
        input_ports_fed_by_function_node_islands.insert(port.to_string(), Arc::new(island_port));
    }

    /// Stop feeding `port` from the island feeding it; whether one did.
    pub fn stop_feeding_an_input_port_from_a_function_node_island(&self, port: &str) -> bool {
        self.input_ports_fed_by_function_node_islands
            .write()
            .remove(port)
            .is_some()
    }

    /// The function node the island feeding `port` runs as Python, which a
    /// read without the Python interpreter's lock cannot call; `None` when no
    /// island feeds `port` or its every node runs as native code.
    pub fn the_function_node_run_as_python_by_the_island_feeding(
        &self,
        port: &str,
    ) -> Option<String> {
        self.function_node_island_feeding(port)
            .and_then(|island_port| {
                island_port
                    .the_first_function_node_it_runs_as_python()
                    .map(str::to_string)
            })
    }

    /// The state each stateful node of the island feeding `port` last
    /// returned, by its breadcrumb number; empty when no island feeds `port`.
    pub fn the_state_each_stateful_function_node_of_the_island_feeding_last_returned(
        &self,
        port: &str,
    ) -> Vec<(u32, Vec<f64>)> {
        self.function_node_island_feeding(port)
            .map(|island_port| island_port.the_state_each_stateful_function_node_last_returned())
            .unwrap_or_default()
    }

    pub(super) fn function_node_island_feeding(
        &self,
        port: &str,
    ) -> Option<Arc<InputPortFedByAFunctionNodeIsland>> {
        self.input_ports_fed_by_function_node_islands
            .read()
            .get(port)
            .cloned()
    }

    /// Read each port of `pin_values` as its pin value while no link feeds
    /// it, at the time `stream_clock_now_ns` gives, in place of the pin values
    /// read before; a port left out reads only its links again. A port kept
    /// counts its reads on from where it was.
    pub fn read_each_unlinked_input_port_as_its_pin_value(
        &self,
        pin_values: HashMap<String, f64>,
        stream_clock_now_ns: Arc<dyn Fn() -> i64 + Send + Sync>,
    ) {
        let mut input_ports_read_as_their_pin_value = self
            .input_ports_read_as_their_pin_value_while_unlinked
            .write();
        let pinned_ports = pin_values
            .into_iter()
            .map(|(port, pin_value)| {
                let pin_value: Arc<dyn FunctionNodeIslandEvaluatedAtEachRead> =
                    Arc::new(PinValueReadByAnUnlinkedInputPort::read_at_the_time_of(
                        pin_value,
                        Arc::clone(&stream_clock_now_ns),
                    ));
                let pinned_port = match input_ports_read_as_their_pin_value.get(&port) {
                    Some(pinned_port_it_replaces) => {
                        InputPortFedByAFunctionNodeIsland::evaluated_by_in_place_of(
                            pin_value,
                            pinned_port_it_replaces,
                        )
                    }
                    None => InputPortFedByAFunctionNodeIsland::evaluated_by(pin_value),
                };
                (port, Arc::new(pinned_port))
            })
            .collect();
        *input_ports_read_as_their_pin_value = pinned_ports;
    }

    /// Read each input of `input_values` that holds a number or a bool as that number, 1 or 0
    /// for a bool, while no link feeds it, stamped with stream time at each read, in place of
    /// the values read before; an input left out, or holding a value of another kind, reads
    /// only its links again.
    pub fn read_each_unlinked_input_as_its_value(
        &self,
        input_values: &serde_json::Map<String, serde_json::Value>,
    ) {
        let numbers = input_values
            .iter()
            .filter_map(|(input, value)| {
                let number = match value {
                    serde_json::Value::Bool(on) => f64::from(u8::from(*on)),
                    serde_json::Value::Number(number) => number.as_f64()?,
                    _ => return None,
                };
                Some((input.clone(), number))
            })
            .collect();
        let stream_clock = self.stream_clock.get().cloned();
        self.read_each_unlinked_input_port_as_its_pin_value(
            numbers,
            Arc::new(move || match &stream_clock {
                Some(stream_clock) => stream_clock.now_ns(),
                None => crate::core::media_clock::MediaClock::now().as_nanos() as i64,
            }),
        );
    }

    /// Whether a link feeds `port`.
    fn is_fed_by_a_link(&self, port: &str) -> bool {
        self.inbound_link_subscribers_and_listener
            .lock()
            .bound_to_local_port(port)
            .next()
            .is_some()
    }

    /// The pin value `port` reads, when one is set on it and no link feeds it.
    pub(super) fn pin_value_read_by_an_unlinked(
        &self,
        port: &str,
    ) -> Option<Arc<InputPortFedByAFunctionNodeIsland>> {
        let pinned_port = self
            .input_ports_read_as_their_pin_value_while_unlinked
            .read()
            .get(port)
            .cloned()?;
        (!self.is_fed_by_a_link(port)).then_some(pinned_port)
    }
}
