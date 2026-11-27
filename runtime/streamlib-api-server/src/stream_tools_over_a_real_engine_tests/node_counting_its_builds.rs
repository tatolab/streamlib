// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A node written as one function that counts each setup and teardown it runs, records each
//! `level` it reads, and holds a [`StubExclusiveDevice`] from its setup to its teardown, one
//! device and one count for each stream it runs in.

use std::collections::BTreeMap;

use parking_lot::Mutex;
use streamlib::sdk::context::RuntimeContextLimitedAccess;
use streamlib::sdk::descriptors::PinType;
use streamlib::sdk::iceoryx2::InputMailboxes;
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

/// What a [`NodeCountingItsBuilds`] holds from its setup to its teardown: its stream's stub
/// exclusive device, counting the teardown as it drops.
struct HeldFromSetupToTeardown {
    stream_id: String,
    _lease: StubExclusiveDeviceLease,
}

impl Drop for HeldFromSetupToTeardown {
    fn drop(&mut self) {
        count_in(&self.stream_id, |counted| counted.teardowns += 1);
    }
}

/// A native node written as one function reading its Float `level` input, that counts each run
/// of its body as a setup and each drop of what its body made as a teardown, and holds its
/// stream's stub exclusive device open from one to the other.
#[streamlib::sdk::node(
    execution = continuous(interval_ms = 5),
    input("level", delivery_profile = "newest", pin_type = Float),
)]
pub(super) fn node_counting_its_builds(
    inputs: InputMailboxes,
    ctx: &RuntimeContextFullAccess<'_>,
) -> Result<impl FnMut(&RuntimeContextLimitedAccess<'_>) -> Result<()>> {
    let device = count_in(ctx.stream_id(), |counted| {
        counted.setups += 1;
        Arc::clone(&counted.device)
    });
    let held = HeldFromSetupToTeardown {
        stream_id: ctx.stream_id().to_owned(),
        _lease: device.open(),
    };
    Ok(move |ctx: &RuntimeContextLimitedAccess<'_>| {
        // Named so the closure owns `held`: its drop, when the node's frame function goes, is
        // what counts the teardown.
        let _held_until_the_frames_end = &held;
        if let Some(message) = inputs.read_raw(THE_COUNTING_NODES_LEVEL)? {
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
    })
}

/// Register [`NodeCountingItsBuilds`] with the node registry, once per test binary.
pub(super) fn register_the_node_counting_its_builds_once() {
    static REGISTERED: std::sync::Once = std::sync::Once::new();
    REGISTERED.call_once(|| NODE_REGISTRY.register::<NodeCountingItsBuilds::Node>());
}
