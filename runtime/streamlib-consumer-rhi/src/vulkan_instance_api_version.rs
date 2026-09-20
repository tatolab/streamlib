// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The Vulkan API version every StreamLib instance requests.

use vulkanalia::vk;

/// The Vulkan API version requested at instance creation, host and consumer
/// alike.
///
/// This request — not any device query — is what makes the entry points
/// promoted into core 1.3 (`cmd_pipeline_barrier2`, `cmd_begin_rendering`,
/// `queue_submit2`, `wait_semaphores`) resolve at load time. A device's
/// reported `apiVersion` is not a capability report: MoltenVK clamps it to
/// whatever the instance asked for, so it answers 1.0.x to an instance that
/// asked for 1.0 and 1.4.x to one that asked for 1.4, on the same hardware.
/// `cargo xtask check-no-device-api-version-branch` keeps the tree off that
/// probe.
///
/// It lives in this crate because a host and a subprocess consumer that
/// disagree on the floor resolve different entry points across the same IPC
/// seam. `cargo xtask check-no-device-api-version-branch` cannot catch that —
/// it bans reads of a device's report, not divergent requests — so the two
/// share the constant instead.
pub const REQUESTED_VULKAN_INSTANCE_API_VERSION: u32 = vk::make_version(1, 4, 0);
