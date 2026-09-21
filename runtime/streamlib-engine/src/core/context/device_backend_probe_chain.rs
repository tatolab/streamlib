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

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    fn an_arm_that_opens(backend_name: &'static str) -> DeviceBackendArm<&'static str> {
        DeviceBackendArm::named(backend_name, move || Ok(backend_name))
    }

    fn an_arm_that_declines(backend_name: &'static str) -> DeviceBackendArm<&'static str> {
        DeviceBackendArm::named(backend_name, move || {
            Err(DeviceBackendArmUnavailableReason::of(format!(
                "{backend_name} was made to decline by this test"
            )))
        })
    }

    #[test]
    fn the_walk_takes_the_first_arm_that_opens_and_asks_no_arm_behind_it() {
        let chosen = first_device_backend_arm_that_opens_among(
            [
                an_arm_that_declines("first"),
                an_arm_that_opens("second"),
                DeviceBackendArm::named("third", || {
                    unreachable!("an arm behind one that opened is never asked")
                }),
            ],
            |_, _| {},
        );
        assert_eq!(chosen, Some("second"));
    }

    /// Every arm that declined before the chosen one is reported, in the order
    /// the chain was given, each with the reason it gave — that report is the
    /// only record of why a machine landed where it did.
    #[test]
    fn every_arm_that_declines_is_reported_in_order_with_its_reason() {
        let demotions = RefCell::new(Vec::new());
        first_device_backend_arm_that_opens_among(
            [
                an_arm_that_declines("first"),
                an_arm_that_declines("second"),
                an_arm_that_opens("third"),
            ],
            |backend_name, reason| {
                demotions
                    .borrow_mut()
                    .push((backend_name, reason.to_string()))
            },
        );
        assert_eq!(
            demotions.into_inner(),
            [
                (
                    "first",
                    "first was made to decline by this test".to_string()
                ),
                (
                    "second",
                    "second was made to decline by this test".to_string()
                ),
            ]
        );
    }

    #[test]
    fn a_chain_whose_arms_all_decline_yields_no_backend() {
        assert_eq!(
            first_device_backend_arm_that_opens_among(
                [
                    an_arm_that_declines("first"),
                    an_arm_that_declines("second")
                ],
                |_, _| {},
            ),
            None
        );
    }
}
