// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The dev-mode diagnostic for a processor callback that runs too long.
//!
//! One interpreter runs every Python processor, so a callback that holds the
//! GIL holds it against all of them — the failure is invisible in the offending
//! processor and shows up as unrelated processors starving. This names the
//! candidate.
//!
//! Interim, and deliberately so. What is measured is wall-clock time between
//! attaching to the interpreter and returning, which is an *upper bound* on the
//! GIL hold, not the hold itself: a callback that releases the GIL while it
//! blocks (`time.sleep`, socket IO, most numpy and torch calls, and this
//! wheel's own blocking bindings) stalls nobody yet spends the same wall time.
//! So the report says what it saw and leaves the inference to the reader rather
//! than accusing a processor that did the right thing. A successor that
//! measures GIL ownership — which needs instrumentation CPython's stable ABI
//! does not expose — replaces this outright; until then a slow callback is
//! worth surfacing in `dev` whatever made it slow.
//!
//! Off unless a launcher turns it on: the cost is a monotonic clock read per
//! callback, which `run` should not pay.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use pyo3::prelude::*;

/// Nanoseconds a callback may run before it is reported.
///
/// A dedicated sentinel rather than zero as the off value, so that a threshold
/// of zero stays meaningful — it reports every callback, which is what makes
/// the reporting path testable without a sleep. `Relaxed` throughout: the
/// threshold publishes no other memory, so there is no happens-before edge to
/// establish, and a processor thread reading a stale value for a few ticks
/// after arming is exactly the intended semantics for a diagnostic.
static SLOW_CALLBACK_THRESHOLD_NANOS: AtomicU64 = AtomicU64::new(SLOW_CALLBACK_WATCHDOG_DISARMED);

/// The off value. Not a duration — no threshold may collide with it.
const SLOW_CALLBACK_WATCHDOG_DISARMED: u64 = u64::MAX;

/// The largest threshold that is still armed (~584 years).
const LONGEST_ARMED_THRESHOLD_NANOS: u64 = SLOW_CALLBACK_WATCHDOG_DISARMED - 1;

/// Well past any per-frame budget — a 60fps frame is 16.7ms — so this fires on
/// a stall rather than on a processor doing real work.
const DEFAULT_SLOW_CALLBACK_THRESHOLD_MS: u64 = 50;

/// `streamlib.arm_slow_callback_watchdog(threshold_ms=None)` — the `dev`
/// diagnostic.
///
/// The launcher arms this; `run` leaves it off. `None` takes the default
/// threshold, so a caller that wants the diagnostic never has to name a number.
#[pyfunction]
#[pyo3(signature = (*, threshold_ms = None))]
pub(crate) fn arm_slow_callback_watchdog(threshold_ms: Option<u64>) {
    let threshold_ms = threshold_ms.unwrap_or(DEFAULT_SLOW_CALLBACK_THRESHOLD_MS);
    SLOW_CALLBACK_THRESHOLD_NANOS.store(
        threshold_ms
            .saturating_mul(1_000_000)
            .min(LONGEST_ARMED_THRESHOLD_NANOS),
        Ordering::Relaxed,
    );
}

/// `streamlib.disarm_slow_callback_watchdog()` — back to the `run` posture.
#[pyfunction]
pub(crate) fn disarm_slow_callback_watchdog() {
    SLOW_CALLBACK_THRESHOLD_NANOS.store(SLOW_CALLBACK_WATCHDOG_DISARMED, Ordering::Relaxed);
}

/// Time `call`, reporting a run past the armed threshold.
///
/// Caller contract: time inside your `Python::attach`, never around it. The
/// wait to acquire the GIL is this thread being a victim rather than a holder,
/// and charging it here would blame whichever processor happened to run after
/// the real offender.
pub(crate) fn call_watching_callback_duration<CallOutcome>(
    processor_display_name: &str,
    hook_name: &str,
    call: impl FnOnce() -> CallOutcome,
) -> CallOutcome {
    let threshold_nanos = SLOW_CALLBACK_THRESHOLD_NANOS.load(Ordering::Relaxed);
    if threshold_nanos == SLOW_CALLBACK_WATCHDOG_DISARMED {
        return call();
    }

    let call_started = Instant::now();
    let call_outcome = call();
    let callback_ran_for = call_started.elapsed();

    if u64::try_from(callback_ran_for.as_nanos()).unwrap_or(u64::MAX) > threshold_nanos {
        tracing::warn!(
            processor = processor_display_name,
            hook = hook_name,
            callback_ran_for_ms = callback_ran_for.as_secs_f64() * 1_000.0,
            threshold_ms = threshold_nanos / 1_000_000,
            "a processor callback ran past the dev-mode threshold. If it held the GIL for \
             that time, every other Python processor in this process was stalled for it; if \
             it released the GIL while blocking, this is only slow, not blocking. Check \
             whether the work releases the GIL."
        );
    }
    call_outcome
}

#[cfg(test)]
mod tests {
    use std::sync::{Mutex, MutexGuard};

    use super::*;

    /// Exclusive use of the process-global threshold, disarmed again on drop.
    ///
    /// Cargo runs these on parallel threads, so without the lock one test's
    /// disarm lands inside another's assertion. Restoring in `Drop` rather than
    /// at the end of each body is what makes a failing assertion fail only its
    /// own test — a straight-line restore is skipped by the unwind, and the
    /// leaked threshold then fails the next test for a reason of its own.
    struct ThresholdHeldForOneTest {
        // Underscore-prefixed: `Drop` does not count as a read, so a plain
        // field name draws a dead_code warning for a guard whose whole job is
        // to exist.
        _exclusive_threshold_access: MutexGuard<'static, ()>,
    }

    impl Drop for ThresholdHeldForOneTest {
        fn drop(&mut self) {
            disarm_slow_callback_watchdog();
        }
    }

    fn exclusive_access_to_the_threshold() -> ThresholdHeldForOneTest {
        static THRESHOLD_IN_USE: Mutex<()> = Mutex::new(());
        // Non-poisoning: a failing test reports its own failure rather than
        // poisoning every later one.
        ThresholdHeldForOneTest {
            _exclusive_threshold_access: THRESHOLD_IN_USE
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        }
    }

    /// Disarmed is the shipped default, and a disarmed watchdog still runs the
    /// call — `run` pays nothing for a diagnostic it did not ask for.
    #[test]
    fn a_disarmed_watchdog_passes_the_call_through() {
        let _threshold_held = exclusive_access_to_the_threshold();
        disarm_slow_callback_watchdog();

        assert_eq!(
            SLOW_CALLBACK_THRESHOLD_NANOS.load(Ordering::Relaxed),
            SLOW_CALLBACK_WATCHDOG_DISARMED
        );
        assert_eq!(
            call_watching_callback_duration("Blur", "process", || 42),
            42
        );
    }

    /// Arming is what the `dev` launcher drives; the threshold has to survive
    /// the round trip into nanoseconds.
    #[test]
    fn arming_stores_the_default_threshold_in_nanoseconds() {
        let _threshold_held = exclusive_access_to_the_threshold();
        arm_slow_callback_watchdog(None);

        assert_eq!(
            SLOW_CALLBACK_THRESHOLD_NANOS.load(Ordering::Relaxed),
            DEFAULT_SLOW_CALLBACK_THRESHOLD_MS * 1_000_000
        );
    }

    /// A zero threshold reports every callback — the reporting path itself,
    /// exercised without a sleep — and must still be observation only.
    #[test]
    fn the_reporting_path_does_not_disturb_the_call() {
        let _threshold_held = exclusive_access_to_the_threshold();
        arm_slow_callback_watchdog(Some(0));

        assert_eq!(
            call_watching_callback_duration("Blur", "process", || "callback outcome"),
            "callback outcome"
        );
    }

    /// A threshold big enough to overflow nanoseconds must clamp to the largest
    /// armed value rather than wrap into the sentinel and silently disarm.
    #[test]
    fn an_absurd_threshold_clamps_to_the_longest_armed_value() {
        let _threshold_held = exclusive_access_to_the_threshold();
        arm_slow_callback_watchdog(Some(u64::MAX));

        assert_eq!(
            SLOW_CALLBACK_THRESHOLD_NANOS.load(Ordering::Relaxed),
            LONGEST_ARMED_THRESHOLD_NANOS
        );
    }
}
