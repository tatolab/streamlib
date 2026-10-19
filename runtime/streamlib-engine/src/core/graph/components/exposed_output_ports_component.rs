// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use crate::core::graph::OutputPortExposureLevel;

/// The output ports of this node its stream exposes, each at its level.
///
/// Held on the node so a removed node takes its exposures with it. Stored but
/// never rendered under the node's `components`: `graph` renders every
/// exposure once, in its top-level `exposed`. An internal port has no entry.
#[derive(Debug, Default)]
pub struct ExposedOutputPortsComponent {
    exposed_ports_by_port_name: BTreeMap<String, ExposedOutputPortOfThisNode>,
}

#[derive(Debug)]
struct ExposedOutputPortOfThisNode {
    level: OutputPortExposureLevel,
    level_now: LevelOfAnExposedOutputPortNow,
}

impl Drop for ExposedOutputPortOfThisNode {
    fn drop(&mut self) {
        self.level_now.store(OutputPortExposureLevel::Internal);
    }
}

/// The level one exposure of an output port is at, read without the graph's
/// lock: set as the level is, and internal for good once the port is no longer
/// exposed or its node has left its stream. A port exposed again has a new one.
#[derive(Debug, Clone)]
pub struct LevelOfAnExposedOutputPortNow(Arc<AtomicU8>);

impl LevelOfAnExposedOutputPortNow {
    fn at(level: OutputPortExposureLevel) -> Self {
        let level_now = Self(Arc::new(AtomicU8::new(0)));
        level_now.store(level);
        level_now
    }

    fn store(&self, level: OutputPortExposureLevel) {
        let encoded = match level {
            OutputPortExposureLevel::Internal => 0,
            OutputPortExposureLevel::Private => 1,
            OutputPortExposureLevel::Public => 2,
        };
        self.0.store(encoded, Ordering::Release);
    }

    /// The level now.
    pub fn read(&self) -> OutputPortExposureLevel {
        match self.0.load(Ordering::Acquire) {
            1 => OutputPortExposureLevel::Private,
            2 => OutputPortExposureLevel::Public,
            _ => OutputPortExposureLevel::Internal,
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

    /// The level `port_name`'s exposure is at, read without the graph's lock,
    /// or `None` while the port is internal.
    pub fn level_now_of(&self, port_name: &str) -> Option<LevelOfAnExposedOutputPortNow> {
        self.exposed_ports_by_port_name
            .get(port_name)
            .map(|exposed| exposed.level_now.clone())
    }

    /// Every exposed port of this node with its level, by port name.
    pub fn exposed_ports_and_their_levels(
        &self,
    ) -> impl Iterator<Item = (&str, OutputPortExposureLevel)> {
        self.exposed_ports_by_port_name
            .iter()
            .map(|(port_name, exposed)| (port_name.as_str(), exposed.level))
    }

    /// Put `port_name` at `level`; an internal port leaves the component.
    pub(crate) fn set_level(&mut self, port_name: &str, level: OutputPortExposureLevel) {
        if level == OutputPortExposureLevel::Internal {
            self.exposed_ports_by_port_name.remove(port_name);
            return;
        }
        let exposed = self
            .exposed_ports_by_port_name
            .entry(port_name.to_string())
            .or_insert_with(|| ExposedOutputPortOfThisNode {
                level,
                level_now: LevelOfAnExposedOutputPortNow::at(level),
            });
        exposed.level = level;
        exposed.level_now.store(level);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_port_with_no_entry_is_internal_and_has_no_level_to_read() {
        let exposed_ports = ExposedOutputPortsComponent::default();

        assert_eq!(
            exposed_ports.level_of("video"),
            OutputPortExposureLevel::Internal
        );
        assert!(exposed_ports.level_now_of("video").is_none());
    }

    /// The level read without the lock follows every change, and reads
    /// internal for good once the port is lowered to internal.
    #[test]
    fn the_level_read_without_the_lock_follows_each_change_and_ends_internal() {
        let mut exposed_ports = ExposedOutputPortsComponent::default();
        exposed_ports.set_level("video", OutputPortExposureLevel::Private);
        let level_now = exposed_ports
            .level_now_of("video")
            .expect("a private port has a level to read");
        assert_eq!(level_now.read(), OutputPortExposureLevel::Private);

        exposed_ports.set_level("video", OutputPortExposureLevel::Public);
        assert_eq!(level_now.read(), OutputPortExposureLevel::Public);

        exposed_ports.set_level("video", OutputPortExposureLevel::Internal);
        assert_eq!(level_now.read(), OutputPortExposureLevel::Internal);
        assert_eq!(
            exposed_ports.exposed_ports_and_their_levels().count(),
            0,
            "an internal port has no entry"
        );

        exposed_ports.set_level("video", OutputPortExposureLevel::Private);
        assert_eq!(
            level_now.read(),
            OutputPortExposureLevel::Internal,
            "a port exposed again has a new level to read"
        );
    }

    /// A node leaving its stream drops its exposures, and every level read
    /// without the lock reads internal.
    #[test]
    fn dropping_the_exposures_leaves_every_level_read_without_the_lock_internal() {
        let mut exposed_ports = ExposedOutputPortsComponent::default();
        exposed_ports.set_level("video", OutputPortExposureLevel::Private);
        exposed_ports.set_level("preview", OutputPortExposureLevel::Public);
        let levels_now: Vec<LevelOfAnExposedOutputPortNow> = ["video", "preview"]
            .into_iter()
            .filter_map(|port_name| exposed_ports.level_now_of(port_name))
            .collect();
        assert_eq!(levels_now.len(), 2);

        drop(exposed_ports);

        for level_now in levels_now {
            assert_eq!(level_now.read(), OutputPortExposureLevel::Internal);
        }
    }
}
