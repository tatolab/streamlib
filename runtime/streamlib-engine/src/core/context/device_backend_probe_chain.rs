// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The walk every device seam's probe takes: an ordered chain of backend arms,
//! the first that actually opens chosen, each demotion reported with the
//! reason behind it.

/// Why an arm of a chain cannot serve, in the words the demotion log line
/// carries.
///
/// Not a core [`crate::core::Error`]: nothing failed that a caller must handle.
/// The chain has another arm, and this is what tells a reader whether the
/// library was absent, the daemon was, or the device was.
#[derive(Debug)]
pub struct DeviceBackendArmUnavailableReason(String);

impl DeviceBackendArmUnavailableReason {
    /// State why this arm cannot serve.
    pub fn of(reason: impl Into<String>) -> Self {
        Self(reason.into())
    }
}

impl std::fmt::Display for DeviceBackendArmUnavailableReason {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// What opening one arm of a chain yields.
type DeviceBackendArmOpenOutcome<SharedBackend> =
    std::result::Result<SharedBackend, DeviceBackendArmUnavailableReason>;

/// One arm of a chain: its name, and the attempt to open it.
///
/// The attempt is held unrun so a chain's *order* can be read — and asserted —
/// without loading a device library or touching a device. Crate-private:
/// nothing outside the engine chooses an arm, because no dial selects a
/// backend.
pub(crate) struct DeviceBackendArm<SharedBackend> {
    pub(crate) backend_name: &'static str,
    open: Box<dyn FnOnce() -> DeviceBackendArmOpenOutcome<SharedBackend>>,
}

impl<SharedBackend> DeviceBackendArm<SharedBackend> {
    /// The only way to build one, so an arm's name always comes from the same
    /// place as the attempt that opens it.
    // A platform with no real arm for a device class has none to construct.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub(crate) fn named(
        backend_name: &'static str,
        open: impl FnOnce() -> DeviceBackendArmOpenOutcome<SharedBackend> + 'static,
    ) -> Self {
        Self {
            backend_name,
            open: Box::new(open),
        }
    }
}

/// Take the first arm that opens, handing every arm that declines before it to
/// `report_demotion` with the reason it gave.
///
/// An arm is chosen by opening, not by loading: a library that resolves but
/// yields no usable connection demotes exactly as a missing library does. The
/// report is the caller's so each device class keeps its own log line.
pub(crate) fn first_device_backend_arm_that_opens_among<SharedBackend>(
    arms: impl IntoIterator<Item = DeviceBackendArm<SharedBackend>>,
    report_demotion: impl Fn(&'static str, &DeviceBackendArmUnavailableReason),
) -> Option<SharedBackend> {
    for arm in arms {
        match (arm.open)() {
            Ok(backend) => return Some(backend),
            Err(reason) => report_demotion(arm.backend_name, &reason),
        }
    }
    None
}
