// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A node that counts each setup and teardown it runs, records each `level` it reads, and holds
//! a [`StubExclusiveDevice`] from its setup to its teardown, one device and one count for each
//! stream it runs in.

use std::collections::BTreeMap;

use parking_lot::Mutex;
use streamlib::sdk::context::RuntimeContextLimitedAccess;
use streamlib::sdk::descriptors::PinType;
use streamlib::sdk::error::Error;
use streamlib::sdk::nodes::ContinuousNode;
use streamlib::sdk::pin_value_packing::{PackedPinValueRef, unpack_pin_value};

use super::stub_exclusive_device::{StubExclusiveDevice, StubExclusiveDeviceLease};
use super::*;

/// The Float input of [`NodeCountingItsBuilds`] a test sets while the stream plays.
pub(super) const THE_COUNTING_NODES_LEVEL: &str = "level";

/// The setups and teardowns [`NodeCountingItsBuilds`] ran in one stream, the last `level` it
/// read, and that stream's device.
#[derive(Debug, Default)]
pub(super) struct BuildsCountedInOneStream {
    setups: usize,
    teardowns: usize,
    last_level_read: Option<f64>,
    device: Arc<StubExclusiveDevice>,
}

impl BuildsCountedInOneStream {
    pub(super) fn setups(&self) -> usize {
        self.setups
    }

    pub(super) fn teardowns(&self) -> usize {
        self.teardowns
    }

    pub(super) fn last_level_read(&self) -> Option<f64> {
        self.last_level_read
    }

    pub(super) fn device(&self) -> &StubExclusiveDevice {
        &self.device
    }
}

/// What each stream's [`NodeCountingItsBuilds`] ran, by stream id.
static BUILDS_COUNTED_BY_STREAM: Mutex<BTreeMap<String, BuildsCountedInOneStream>> =
    Mutex::new(BTreeMap::new());

fn count_in<T>(stream_id: &str, count: impl FnOnce(&mut BuildsCountedInOneStream) -> T) -> T {
    count(
        BUILDS_COUNTED_BY_STREAM
            .lock()
            .entry(stream_id.to_owned())
            .or_default(),
    )
}

/// Read what the [`NodeCountingItsBuilds`] of the stream `stream_id` ran; fails the test when
/// none ran in it.
pub(super) fn the_builds_counted_in<T>(
    stream_id: &str,
    read: impl FnOnce(&BuildsCountedInOneStream) -> T,
) -> T {
    read(
        BUILDS_COUNTED_BY_STREAM
            .lock()
            .get(stream_id)
            .unwrap_or_else(|| {
                panic!("no `NodeCountingItsBuilds` ran in the stream `{stream_id}`")
            }),
    )
}

/// A native node reading its Float `level` input that counts its setups and teardowns and holds
/// its stream's stub exclusive device open from setup to teardown.
#[streamlib::sdk::node(
    execution = continuous(interval_ms = 5),
    input("level", delivery_profile = "newest", pin_type = Float),
)]
pub(super) struct NodeCountingItsBuilds {
    lease: Option<StubExclusiveDeviceLease>,
}

impl ContinuousNode for NodeCountingItsBuilds::Node {
    fn setup(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        let device = count_in(ctx.stream_id(), |counted| {
            counted.setups += 1;
            Arc::clone(&counted.device)
        });
        self.lease = Some(
            device
                .open()
                .map_err(|already_held| Error::Configuration(already_held.to_string()))?,
        );
        Ok(())
    }

    fn teardown(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        count_in(ctx.stream_id(), |counted| counted.teardowns += 1);
        self.lease = None;
        Ok(())
    }

    fn process(&mut self, ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        if let Some(message) = self.inputs.read_raw(THE_COUNTING_NODES_LEVEL)? {
            let level = unpack_pin_value(
                &PinType::Float { exact_size: None },
                PackedPinValueRef {
                    payload_descriptor: &message.payload_descriptor,
                    payload: message.payload.bytes(),
                },
            )
            .expect("a Float message unpacks")
            .as_f64()
            .expect("a Float unpacks to a number");
            count_in(ctx.stream_id(), |counted| {
                counted.last_level_read = Some(level)
            });
        }
        Ok(())
    }
}

/// Register [`NodeCountingItsBuilds`] with the node registry, once per test binary.
pub(super) fn register_the_node_counting_its_builds_once() {
    static REGISTERED: std::sync::Once = std::sync::Once::new();
    REGISTERED.call_once(|| NODE_REGISTRY.register::<NodeCountingItsBuilds::Node>());
}
