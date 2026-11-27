// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A node written as one function that paces itself, as a camera does: its one computed value
//! is its stream's [`StubExclusiveDevice`] opened at its `width`, opened again by each new width
//! once the lease the value it replaces holds is closed, as each shipped device node closes the
//! device it has before opening the next. It counts each run of its body, one count and one
//! device for each stream it runs in.

use std::collections::BTreeMap;

use parking_lot::Mutex;
use streamlib::sdk::nodes::{Input, computed};

use super::stub_exclusive_device::{StubExclusiveDevice, StubExclusiveDeviceLease};
use super::*;

/// The Int input of [`NodeHoldingTheStubDevice`] its device is opened at.
pub(super) const THE_DEVICE_HOLDING_NODES_WIDTH: &str = "width";

/// The bodies [`NodeHoldingTheStubDevice`] ran in one stream, and that stream's device.
#[derive(Debug, Default)]
pub(super) struct DeviceHeldInOneStream {
    bodies_run: usize,
    device: Arc<StubExclusiveDevice>,
}

impl DeviceHeldInOneStream {
    pub(super) fn bodies_run(&self) -> usize {
        self.bodies_run
    }

    pub(super) fn device(&self) -> &StubExclusiveDevice {
        &self.device
    }
}

static DEVICE_HELD_BY_STREAM: Mutex<BTreeMap<String, DeviceHeldInOneStream>> =
    Mutex::new(BTreeMap::new());

fn held_in<T>(stream_id: &str, read: impl FnOnce(&mut DeviceHeldInOneStream) -> T) -> T {
    read(
        DEVICE_HELD_BY_STREAM
            .lock()
            .entry(stream_id.to_owned())
            .or_default(),
    )
}

/// Read what the [`NodeHoldingTheStubDevice`] of the stream `stream_id` ran and opened.
pub(super) fn the_device_held_in<T>(
    stream_id: &str,
    read: impl FnOnce(&DeviceHeldInOneStream) -> T,
) -> T {
    held_in(stream_id, |held| read(held))
}

/// The stub device, open at one width.
#[derive(Debug, PartialEq)]
struct StubDeviceOpenAtAWidth {
    _lease: Arc<StubExclusiveDeviceLease>,
    width: i32,
}

#[streamlib::sdk::node(
    execution = manual,
    input("width", description = "The width its device is opened at.", value = i32, default = 640),
)]
pub(super) fn node_holding_the_stub_device(width: Input<i32>, ctx: &RuntimeContextFullAccess<'_>) {
    held_in(ctx.stream_id(), |held| held.bodies_run += 1);
    let open_now: Arc<Mutex<Option<Arc<StubExclusiveDeviceLease>>>> = Arc::default();
    computed(move |build_ctx| {
        let width = *width.get();
        let device = held_in(build_ctx.stream_id(), |held| Arc::clone(&held.device));
        if let Some(replaced) = open_now.lock().take() {
            replaced.close();
        }
        let lease = Arc::new(device.open());
        *open_now.lock() = Some(Arc::clone(&lease));
        Ok::<_, String>(StubDeviceOpenAtAWidth {
            _lease: lease,
            width,
        })
    });
}

/// Register [`NodeHoldingTheStubDevice`] with the node registry, once per test binary.
pub(super) fn register_the_node_holding_the_stub_device_once() {
    static REGISTERED: std::sync::Once = std::sync::Once::new();
    REGISTERED.call_once(|| NODE_REGISTRY.register::<NodeHoldingTheStubDevice::Node>());
}
