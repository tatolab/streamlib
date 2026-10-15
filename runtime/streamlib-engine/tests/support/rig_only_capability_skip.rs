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

/// What the bundled software device or a CI runner cannot give a test.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)] // Each including test binary names only the capabilities it needs.
pub enum RigOnlyCapability {
    VulkanVideo,
    DmaBufModifiers,
    Cuda,
    Camera,
    Display,
    AudioDevice,
}

#[allow(dead_code)]
impl RigOnlyCapability {
    /// Every capability, in the order the closed list states them.
    pub const EVERY_RIG_ONLY_CAPABILITY: [RigOnlyCapability; 6] = [
        RigOnlyCapability::VulkanVideo,
        RigOnlyCapability::DmaBufModifiers,
        RigOnlyCapability::Cuda,
        RigOnlyCapability::Camera,
        RigOnlyCapability::Display,
        RigOnlyCapability::AudioDevice,
    ];

    /// The closed-list reason, spelled as `rig_only_capability(reason=…)` spells it.
    pub const fn closed_list_reason(self) -> &'static str {
        match self {
            RigOnlyCapability::VulkanVideo => "Vulkan Video",
            RigOnlyCapability::DmaBufModifiers => "DMA-BUF modifiers",
            RigOnlyCapability::Cuda => "CUDA",
            RigOnlyCapability::Camera => "a camera",
            RigOnlyCapability::Display => "a display",
            RigOnlyCapability::AudioDevice => "an audio device",
        }
    }
}

/// What every skip line starts with; the lane's skip-line check matches it
/// followed by a closed-list reason.
#[allow(dead_code)]
pub const HARDWARE_TEST_SKIP_LINE_PREFIX: &str = "hardware test skipped, needing ";

/// Prints the skip line naming `capability` and returns `true` when this
/// machine lacks it, so the caller returns; returns `false` otherwise.
#[allow(dead_code)]
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
#[allow(dead_code)]
pub fn skip_because_this_machine_lacks(capability: RigOnlyCapability) {
    // `println!`, not `tracing`: a test binary that has started no runtime has
    // no subscriber, and the lane reads this line from the test's own stdout.
    println!(
        "{HARDWARE_TEST_SKIP_LINE_PREFIX}{}: {}",
        capability.closed_list_reason(),
        std::thread::current()
            .name()
            .unwrap_or("an unnamed test thread"),
    );
}

/// Whether this process can reach a display server: the window server on
/// macOS, one named in the environment as winit finds it elsewhere.
#[allow(dead_code)]
pub fn this_process_can_reach_a_display_server() -> bool {
    cfg!(target_os = "macos")
        || ["DISPLAY", "WAYLAND_DISPLAY", "WAYLAND_SOCKET"]
            .iter()
            .any(|name| std::env::var_os(name).is_some_and(|value| !value.is_empty()))
}
