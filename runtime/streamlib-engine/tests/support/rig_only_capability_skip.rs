// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The one way a hardware test skips: naming what this machine lacks from the
//! closed list a test may stay on the rig for.
//!
//! The Linux lane runs every `hardware-tests` suite on the runtime unit's
//! bundled software device and fails on any skip line that does not match
//! [`HARDWARE_TEST_SKIP_LINE_PREFIX`] followed by a [`RigOnlyCapability`]
//! reason. The reasons are the Python `rig_only_capability(reason=…)` closed
//! list, word for word.

// Each test binary compiles its own copy and uses part of it.
#![allow(dead_code)]

/// What the bundled software device or a CI runner cannot give a test.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RigOnlyCapability {
    VulkanVideo,
    DmaBufModifiers,
    Cuda,
    Camera,
    AudioDevice,
}

impl RigOnlyCapability {
    /// Every capability, in the order the closed list states them.
    pub const EVERY_RIG_ONLY_CAPABILITY: [RigOnlyCapability; 5] = [
        RigOnlyCapability::VulkanVideo,
        RigOnlyCapability::DmaBufModifiers,
        RigOnlyCapability::Cuda,
        RigOnlyCapability::Camera,
        RigOnlyCapability::AudioDevice,
    ];

    /// The closed-list reason, spelled as `rig_only_capability(reason=…)` spells it.
    pub const fn closed_list_reason(self) -> &'static str {
        match self {
            RigOnlyCapability::VulkanVideo => "Vulkan Video",
            RigOnlyCapability::DmaBufModifiers => "DMA-BUF modifiers",
            RigOnlyCapability::Cuda => "CUDA",
            RigOnlyCapability::Camera => "a camera",
            RigOnlyCapability::AudioDevice => "an audio device",
        }
    }
}

/// What every skip line starts with; the lane's skip-line check matches it
/// followed by a closed-list reason.
pub const HARDWARE_TEST_SKIP_LINE_PREFIX: &str = "hardware test skipped, needing ";

/// Prints the skip line naming `capability` and returns `true` when this
/// machine lacks it, so the caller returns; returns `false` otherwise.
#[must_use = "the caller returns when this is true"]
pub fn skip_unless_this_machine_has(
    capability: RigOnlyCapability,
    this_machine_has_it: bool,
) -> bool {
    if !this_machine_has_it {
        skip_because_this_machine_lacks(capability);
    }
    !this_machine_has_it
}

/// Prints the skip line naming `capability`, for a caller that has already
/// found it absent and returns next.
pub fn skip_because_this_machine_lacks(capability: RigOnlyCapability) {
    print_the_skip_line(capability, None);
}

/// Prints the skip line naming `capability` and, after it, the refusal that
/// showed it absent, for a caller that returns next.
pub fn skip_because_this_machine_lacks_naming_the_refusal(
    capability: RigOnlyCapability,
    refusal: impl std::fmt::Display,
) {
    print_the_skip_line(capability, Some(&refusal));
}

fn print_the_skip_line(capability: RigOnlyCapability, refusal: Option<&dyn std::fmt::Display>) {
    let test_name = std::thread::current()
        .name()
        .unwrap_or("an unnamed test thread")
        .to_owned();
    // `println!`, not `tracing`: a test binary that has started no runtime has
    // no subscriber, and the lane reads this line from the test's own stdout.
    match refusal {
        Some(refusal) => println!(
            "{HARDWARE_TEST_SKIP_LINE_PREFIX}{}: {test_name}: {refusal}",
            capability.closed_list_reason(),
        ),
        None => println!(
            "{HARDWARE_TEST_SKIP_LINE_PREFIX}{}: {test_name}",
            capability.closed_list_reason(),
        ),
    }
}
