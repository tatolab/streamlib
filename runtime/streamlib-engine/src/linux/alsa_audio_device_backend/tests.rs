// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

use super::alsa_period_wait_recovery_and_timestamps::a_recovered_stream_still_has_to_be_started;

/// The clause the whole seam turns on, at the one place it can be lost: a
/// stop requested during the final wait is not the device failing.
///
/// Mental revert: read the flag before the wait only, and a graph shutting
/// down while its microphone happens to be quiet reports a device failure
/// that never happened.
#[test]
fn a_stop_arriving_during_the_last_silent_wait_outranks_the_silence() {
    assert!(
        matches!(
            exit_for_a_device_that_delivered_nothing(
                true,
                CONSECUTIVE_SILENT_WAITS_BEFORE_GIVING_UP
            ),
            AlsaDeviceThreadExit::StopThatWasAskedFor
        ),
        "a stop the owner asked for was reported as the device going quiet"
    );
    assert!(
        matches!(
            exit_for_a_device_that_delivered_nothing(
                false,
                CONSECUTIVE_SILENT_WAITS_BEFORE_GIVING_UP
            ),
            AlsaDeviceThreadExit::DeviceWentQuiet { .. }
        ),
        "a device that really did go quiet has to still be reported as one"
    );
}

/// A stop the owner asked for is not a failure, and this is the assertion
/// that keeps it that way: reported as one, the signal would fire on every
/// clean teardown and an owner could no longer tell a dead microphone from
/// one it switched off.
#[test]
fn a_thread_that_stopped_because_it_was_told_to_reports_no_failure() {
    for direction in [AlsaStreamDirection::Capture, AlsaStreamDirection::Playback] {
        let (failure_recorder, liveness_report) =
            DeviceStreamFailureRecorder::recording_into_a_new_report();

        record_an_alsa_device_thread_exit(
            &AlsaDeviceThreadExit::StopThatWasAskedFor,
            direction,
            "default",
            &failure_recorder,
        );

        assert!(
            liveness_report.failure_that_ended_the_stream().is_none(),
            "a deliberate stop was reported as a {} failure",
            direction.as_word()
        );
    }
}

/// Every way a transfer thread can end early, in both directions: each one
/// leaves the owner a reason it can act on rather than only a log line.
///
/// Driven through the exit value rather than through a device, which is
/// what makes the three reachable at all — two of them need hardware that
/// has begun failing, and the third needs a `snd_pcm_recover` that cannot.
#[test]
fn every_way_a_thread_dies_early_reaches_the_owner_naming_what_happened() {
    let exits_and_what_they_must_say = [
        (
            AlsaDeviceThreadExit::DeviceWentQuiet {
                consecutive_silent_waits: CONSECUTIVE_SILENT_WAITS_BEFORE_GIVING_UP,
            },
            "consecutive waits",
        ),
        (
            AlsaDeviceThreadExit::DeviceRefused(Error::Runtime(
                "snd_pcm_status refused".to_string(),
            )),
            "snd_pcm_status refused",
        ),
        (
            AlsaDeviceThreadExit::StreamCouldNotBeRecovered,
            "could not be recovered",
        ),
    ];

    for (exit, what_the_reason_must_say) in exits_and_what_they_must_say {
        for direction in [AlsaStreamDirection::Capture, AlsaStreamDirection::Playback] {
            let (failure_recorder, liveness_report) =
                DeviceStreamFailureRecorder::recording_into_a_new_report();

            record_an_alsa_device_thread_exit(&exit, direction, "hw:0,0", &failure_recorder);

            let reason = liveness_report
                .failure_that_ended_the_stream()
                .expect("a thread that ended early has to leave its owner a reason")
                .to_string();
            assert!(
                reason.contains(what_the_reason_must_say),
                "a {} stream's reason has to name what happened, not just that something \
                 did: {reason}",
                direction.as_word()
            );
            assert!(
                reason.contains(direction.as_word()),
                "the reason has to name the direction it came from, because a graph runs \
                 both against the same device: {reason}"
            );
        }
    }
}

/// The stalled-device reason says the opposite thing either side of the
/// seam, and the wait that produced it is the same call — so a reader
/// looking at a `took nothing` line knows it is the speaker.
#[test]
fn a_stalled_device_is_described_by_what_that_direction_stopped_doing() {
    assert_eq!(
        AlsaStreamDirection::Capture.what_a_stalled_device_stopped_doing(),
        "delivered"
    );
    assert_eq!(
        AlsaStreamDirection::Playback.what_a_stalled_device_stopped_doing(),
        "took"
    );
}

const SAMPLE_RATE_48K: u32 = 48_000;
/// 512 samples at 48 kHz.
const PERIOD_NANOS: i64 = 10_666_666;

/// A soname nothing on any machine resolves, so the loader's first refusal
/// is reachable without a stub on disk.
const A_SONAME_NO_MACHINE_HAS: &str = "libasound-this-soname-names-nothing.so.999";

#[test]
fn the_loader_names_the_library_it_could_not_open() {
    let refusal = AlsaLibraryEntryPoints::resolve_from(A_SONAME_NO_MACHINE_HAS)
        .err()
        .expect("no machine has this library");
    let refusal = refusal.to_string();
    assert!(
        refusal.contains(A_SONAME_NO_MACHINE_HAS) && refusal.contains("could not be loaded"),
        "the demotion log line has to say which library was missing: {refusal}"
    );
}

/// A library present on every machine this arm targets — it is already
/// mapped into this process — which exports no `snd_*` symbol at all.
const A_LIBRARY_THAT_IS_NOT_ALSA: &str = "libc.so.6";

/// The second refusal: a library that loads and exports none of this. It is
/// the one a host with a stale or wrongly-named `libasound.so.2` produces,
/// and reporting it as "missing" would send a reader looking for a file
/// that is right there.
#[test]
fn the_loader_names_the_symbol_a_wrong_library_does_not_export() {
    let refusal = AlsaLibraryEntryPoints::resolve_from(A_LIBRARY_THAT_IS_NOT_ALSA)
        .err()
        .expect("libc is not libasound");
    let refusal = refusal.to_string();
    assert!(
        refusal.contains("exports no snd_"),
        "a library that loads but is not ALSA must be named as such, not reported as \
         missing: {refusal}"
    );
}

/// Mental revert: drop the delay term and this returns the status time,
/// stamping the block at its end and making a joined camera frame one
/// period late.
#[test]
fn a_blocks_stamp_sits_one_period_before_a_status_reporting_one_unread_period() {
    let device_stamp_ns = 541_560_469_372_719;
    assert_eq!(
        first_sample_timestamp_ns(device_stamp_ns, 512, SAMPLE_RATE_48K),
        device_stamp_ns - PERIOD_NANOS
    );
}

/// A device that has fallen further behind reports more unread samples, and
/// the block it is about to hand over starts correspondingly earlier.
#[test]
fn more_unread_samples_move_the_stamp_further_into_the_past() {
    let device_stamp_ns = 1_000_000_000;
    assert_eq!(
        first_sample_timestamp_ns(device_stamp_ns, 1024, SAMPLE_RATE_48K),
        device_stamp_ns - 2 * PERIOD_NANOS - 1,
        "two periods of unread samples put the first one two periods back"
    );
}

/// Consecutive periods are one block apart, which is what makes
/// `first_sample_timestamp_ns + sample_count / sample_rate` the next
/// block's expected stamp — the property a consumer joins on.
#[test]
fn consecutive_periods_are_exactly_one_block_apart() {
    let first_status_ns = 1_000_000_000;
    let second_status_ns = first_status_ns + PERIOD_NANOS;
    assert_eq!(
        first_sample_timestamp_ns(second_status_ns, 512, SAMPLE_RATE_48K)
            - first_sample_timestamp_ns(first_status_ns, 512, SAMPLE_RATE_48K),
        PERIOD_NANOS
    );
}

const A_MONOTONIC_START_NS: i64 = 541_560_000_000_000;
/// What `snd_pcm_status_get_htstamp` hands back when the device ignored the
/// requested timestamp type: seconds since 1970, not since boot.
const A_WALL_CLOCK_STAMP_NS: i64 = 1_787_879_043_681_980_184;

fn refuse_a_stamp_of(device_stamp_ns: i64) -> Result<()> {
    refuse_a_stamp_outside_the_monotonic_domain(
        "a-test-device",
        device_stamp_ns,
        A_MONOTONIC_START_NS,
        A_MONOTONIC_START_NS + PERIOD_NANOS,
    )
}

/// The guard that keeps a wrong-epoch stamp off the data plane, driven red
/// by each way a device can miss the domain — including what a host whose
/// `alsa.conf` leaves `defaults.pcm.tstamp_type` unset actually produces.
#[test]
fn a_stamp_from_the_wrong_clock_is_refused_and_a_monotonic_one_is_not() {
    assert!(refuse_a_stamp_of(A_MONOTONIC_START_NS + 1).is_ok());

    let refusal = refuse_a_stamp_of(A_WALL_CLOCK_STAMP_NS)
        .expect_err("a wall-clock stamp is five decades outside any uptime bracket");
    assert!(
        matches!(refusal, Error::NotSupported(_)),
        "a device that cannot be timed is unsupported, not a runtime failure: {refusal:?}"
    );
    let refusal = refusal.to_string();
    assert!(
        refusal.contains("a-test-device") && refusal.contains(&A_WALL_CLOCK_STAMP_NS.to_string()),
        "the refusal has to name the device and the stamp it refused: {refusal}"
    );

    assert!(
        refuse_a_stamp_of(0).is_err(),
        "a zeroed htstamp is a device reporting no time at all, not time zero"
    );
    assert!(
        refuse_a_stamp_of(A_MONOTONIC_START_NS + PERIOD_NANOS + 1).is_err(),
        "a device cannot have captured a sample after the status that reported it"
    );
}

/// Ask libasound to spell back every ABI constant this arm hard-codes.
///
/// The values are the arm's only claim about a library it never links, and
/// a wrong one is silent: `SND_PCM_STATE_RUNNING` off by two restores the
/// recovery defect with every other test still green, because those tests
/// are written in terms of the constant rather than the value. libasound
/// names its own enumerators, so they are checked against the library that
/// defines them rather than against a header someone read once.
#[test]
fn libasound_spells_back_every_abi_constant_this_arm_hard_codes() {
    // SAFETY: `dlopen` of a soname; nothing is dereferenced unless it opens.
    let Ok(library) = (unsafe { Library::new(ALSA_LIBRARY_SONAME) }) else {
        // A machine with no ALSA cannot answer the question. The chain is
        // built for exactly that machine, so it is not a failure.
        return;
    };

    for (naming_entry_point, enumerator, expected_name) in [
        (
            &b"snd_pcm_stream_name\0"[..],
            SND_PCM_STREAM_CAPTURE,
            "CAPTURE",
        ),
        (
            b"snd_pcm_access_name\0",
            SND_PCM_ACCESS_RW_INTERLEAVED,
            "RW_INTERLEAVED",
        ),
        (b"snd_pcm_format_name\0", SND_PCM_FORMAT_S16_LE, "S16_LE"),
        (
            b"snd_pcm_format_name\0",
            SND_PCM_FORMAT_FLOAT_LE,
            "FLOAT_LE",
        ),
        (
            b"snd_pcm_tstamp_mode_name\0",
            SND_PCM_TSTAMP_ENABLE,
            "ENABLE",
        ),
        (
            b"snd_pcm_tstamp_type_name\0",
            SND_PCM_TSTAMP_TYPE_MONOTONIC,
            "MONOTONIC",
        ),
        (b"snd_pcm_state_name\0", SND_PCM_STATE_RUNNING, "RUNNING"),
    ] {
        // SAFETY: every one of these is an `ALSA_0.9` entry point taking one
        // enumerator and returning a pointer into libasound's own static
        // name table, valid while the library is loaded.
        let named = unsafe {
            let name_of: libloading::Symbol<'_, unsafe extern "C" fn(c_int) -> *const c_char> =
                library
                    .get(naming_entry_point)
                    .expect("libasound names its own enumerators");
            let named = name_of(enumerator);
            assert!(!named.is_null(), "libasound named nothing for {enumerator}");
            CStr::from_ptr(named).to_string_lossy().into_owned()
        };
        assert_eq!(
            named, expected_name,
            "this arm calls {enumerator} {expected_name}, and libasound calls it {named}"
        );
    }

    // The two error codes have no name function; they are plain errnos, and
    // comparing them to the C library's own is exact where comparing
    // `snd_strerror` text would depend on the locale.
    assert_eq!(
        -ALSA_BROKEN_PIPE,
        libc::EPIPE,
        "a capture overrun and a playback underrun are both EPIPE"
    );
    assert_eq!(
        -ALSA_SUSPENDED,
        libc::ESTRPIPE,
        "a suspended stream is ESTRPIPE"
    );
}

/// `snd_pcm_recover` does not mean one thing. Reverting this to an
/// unconditional restart is what a first reading of "recover only prepares"
/// gives you, and it kills capture on the first interrupted wait —
/// `snd_pcm_prepare` against a running PCM is `EBUSY` on a raw device, and
/// silently fine through the PipeWire ALSA plugin, so only a rig with real
/// hardware would ever show it.
#[test]
fn only_a_stream_recovery_left_stopped_is_started_again() {
    const SND_PCM_STATE_PREPARED: c_int = 2;
    const SND_PCM_STATE_XRUN: c_int = 4;
    const SND_PCM_STATE_SETUP: c_int = 1;

    assert!(
        !a_recovered_stream_still_has_to_be_started(SND_PCM_STATE_RUNNING),
        "an interrupted wait, and a suspend whose resume succeeded, come back running — \
         preparing one of those is EBUSY"
    );
    for state_recovery_left in [
        SND_PCM_STATE_PREPARED,
        SND_PCM_STATE_XRUN,
        SND_PCM_STATE_SETUP,
    ] {
        assert!(
            a_recovered_stream_still_has_to_be_started(state_recovery_left),
            "a stream left in state {state_recovery_left} delivers nothing until it is \
             started, and this arm disables ALSA's implicit start"
        );
    }
}

/// A stream that has not settled reports neither a rate nor a delay, and no
/// offset is derivable from that — the stamp must still be the device's
/// rather than a division by zero or a stamp from the future.
#[test]
fn an_unsettled_stream_contributes_no_offset_rather_than_dividing_by_zero() {
    assert_eq!(
        first_sample_timestamp_ns(1_000_000_000, 512, 0),
        1_000_000_000
    );
    assert_eq!(
        first_sample_timestamp_ns(1_000_000_000, 0, SAMPLE_RATE_48K),
        1_000_000_000
    );
    assert_eq!(
        first_sample_timestamp_ns(1_000_000_000, -512, SAMPLE_RATE_48K),
        1_000_000_000,
        "a negative delay is not a stamp in the future"
    );
}
