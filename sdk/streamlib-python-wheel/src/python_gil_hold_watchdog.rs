// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The dev-mode diagnostic for the GIL-release contract.
//!
//! One interpreter runs every Python processor, so a callback that holds the
//! GIL holds it against all of them — the failure is invisible in the
//! offending processor and shows up as unrelated processors starving. This
//! names the holder instead. Off unless a launcher turns it on: the cost is a
//! monotonic clock read per callback, which `run` should not pay.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use pyo3::prelude::*;

/// Nanoseconds a callback may hold the GIL before it is reported.
///
/// [`DISARMED`] rather than zero as the off value, so that a threshold of zero
/// stays meaningful — it arms the watchdog on every callback, which is what
/// makes the reporting path testable without a sleep.
static GIL_HOLD_WARNING_THRESHOLD_NANOS: AtomicU64 = AtomicU64::new(DISARMED);

const DISARMED: u64 = u64::MAX;

/// Well past any per-frame budget — a 60fps frame is 16.7ms — so this fires on
/// a stall rather than on a processor doing real work.
pub(crate) const DEFAULT_GIL_HOLD_WARNING_THRESHOLD_MS: u64 = 50;

/// Arm the watchdog at `threshold_ms`, or disarm it with `None`.
pub(crate) fn set_gil_hold_warning_threshold_ms(threshold_ms: Option<u64>) {
    let threshold_nanos = match threshold_ms {
        Some(milliseconds) => milliseconds.saturating_mul(1_000_000).min(DISARMED - 1),
        None => DISARMED,
    };
    GIL_HOLD_WARNING_THRESHOLD_NANOS.store(threshold_nanos, Ordering::Relaxed);
}

/// `streamlib.arm_gil_hold_watchdog(threshold_ms=None)` — the `dev` diagnostic.
///
/// The launcher arms this; `run` leaves it off. Passing `None` for
/// `threshold_ms` takes the default rather than disarming, so the caller that
/// wants the diagnostic never has to name a number.
#[pyfunction]
#[pyo3(signature = (*, threshold_ms = None))]
pub(crate) fn arm_gil_hold_watchdog(threshold_ms: Option<u64>) {
    set_gil_hold_warning_threshold_ms(Some(
        threshold_ms.unwrap_or(DEFAULT_GIL_HOLD_WARNING_THRESHOLD_MS),
    ));
}

/// `streamlib.disarm_gil_hold_watchdog()` — back to the `run` posture.
#[pyfunction]
pub(crate) fn disarm_gil_hold_watchdog() {
    set_gil_hold_warning_threshold_ms(None);
}

/// Run `call` with the GIL held, reporting a hold past the threshold.
///
/// Timed inside the attach rather than around it: the wait to acquire the GIL
/// is this thread being a victim, not a holder, and counting it would blame
/// whichever processor happened to run after the real offender.
pub(crate) fn call_watching_gil_hold<CallOutcome>(
    processor_display_name: &str,
    hook_name: &str,
    call: impl FnOnce() -> CallOutcome,
) -> CallOutcome {
    let threshold_nanos = GIL_HOLD_WARNING_THRESHOLD_NANOS.load(Ordering::Relaxed);
    if threshold_nanos == DISARMED {
        return call();
    }

    let call_started = Instant::now();
    let call_outcome = call();
    let gil_held = call_started.elapsed();

    if u64::try_from(gil_held.as_nanos()).unwrap_or(u64::MAX) > threshold_nanos {
        tracing::warn!(
            processor = processor_display_name,
            hook = hook_name,
            gil_held_ms = gil_held.as_secs_f64() * 1_000.0,
            threshold_ms = threshold_nanos / 1_000_000,
            "a processor callback held the GIL past the dev-mode threshold — every other \
             Python processor in this process was stalled for that long. Release the GIL \
             around the blocking call, or move the work off the callback."
        );
    }
    call_outcome
}

#[cfg(test)]
mod tests {
    use std::sync::{Mutex, MutexGuard};

    use super::*;

    /// The threshold is one process-global, and cargo runs these on parallel
    /// threads — without this, one test's disarm lands inside another's
    /// assertion. Non-poisoning so a failing test reports its own failure
    /// rather than poisoning every later one.
    fn exclusive_access_to_the_threshold() -> MutexGuard<'static, ()> {
        static THRESHOLD_IN_USE: Mutex<()> = Mutex::new(());
        THRESHOLD_IN_USE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Disarmed is the shipped default, and a disarmed watchdog still runs the
    /// call — `run` pays nothing for a diagnostic it did not ask for.
    #[test]
    fn a_disarmed_watchdog_passes_the_call_through() {
        let _threshold_held = exclusive_access_to_the_threshold();
        set_gil_hold_warning_threshold_ms(None);

        assert_eq!(
            GIL_HOLD_WARNING_THRESHOLD_NANOS.load(Ordering::Relaxed),
            DISARMED
        );
        assert_eq!(call_watching_gil_hold("Blur", "process", || 42), 42);
    }

    /// Arming is what the `dev` launcher drives; the threshold has to survive
    /// the round trip into nanoseconds.
    #[test]
    fn arming_stores_the_threshold_in_nanoseconds() {
        let _threshold_held = exclusive_access_to_the_threshold();
        set_gil_hold_warning_threshold_ms(Some(DEFAULT_GIL_HOLD_WARNING_THRESHOLD_MS));

        assert_eq!(
            GIL_HOLD_WARNING_THRESHOLD_NANOS.load(Ordering::Relaxed),
            DEFAULT_GIL_HOLD_WARNING_THRESHOLD_MS * 1_000_000
        );

        set_gil_hold_warning_threshold_ms(None);
    }

    /// A zero threshold reports every callback — the reporting path itself,
    /// exercised without a sleep — and must still be observation only.
    #[test]
    fn the_reporting_path_does_not_disturb_the_call() {
        let _threshold_held = exclusive_access_to_the_threshold();
        set_gil_hold_warning_threshold_ms(Some(0));
        let call_outcome = call_watching_gil_hold("Blur", "process", || "callback outcome");
        set_gil_hold_warning_threshold_ms(None);

        assert_eq!(call_outcome, "callback outcome");
    }

    /// A threshold big enough to overflow nanoseconds must clamp rather than
    /// wrap into a value that silently disarms — or reports on everything.
    #[test]
    fn an_absurd_threshold_clamps_instead_of_wrapping() {
        let _threshold_held = exclusive_access_to_the_threshold();
        set_gil_hold_warning_threshold_ms(Some(u64::MAX));

        let stored = GIL_HOLD_WARNING_THRESHOLD_NANOS.load(Ordering::Relaxed);
        assert_ne!(stored, DISARMED, "a huge threshold must not disarm the watchdog");

        set_gil_hold_warning_threshold_ms(None);
    }
}
