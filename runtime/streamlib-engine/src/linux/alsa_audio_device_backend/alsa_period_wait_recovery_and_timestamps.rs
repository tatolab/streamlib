// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Waiting for the device to release a period, recovering from a failed transfer, and stamping a block with the device's own timing.

use super::*;

/// Whether a wait produced a whole period the device is ready to transfer —
/// samples to read on a capture stream, room to write on a playback one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DevicePeriodReadiness {
    Ready,
    NothingYet,
}

/// Wait for the device to release a period in whichever direction it runs.
///
/// The wait is the cadence on this arm — `snd_pcm_wait` returns when the device
/// says so, not when a timer does — which is what makes an ALSA stream
/// device-paced despite the arm owning the thread.
pub(super) fn wait_for_the_device_to_release_a_period(
    entry_points: &AlsaLibraryEntryPoints,
    pcm: *mut c_void,
    device_name: &str,
    direction: AlsaStreamDirection,
) -> Result<DevicePeriodReadiness> {
    // SAFETY: `pcm` is an open, started handle held by this thread alone;
    // `snd_pcm_wait` polls it and returns without touching anything else.
    let waited = unsafe { (entry_points.snd_pcm_wait)(pcm, DEVICE_WAIT_TIMEOUT_MS) };
    if waited == 0 {
        return Ok(DevicePeriodReadiness::NothingYet);
    }
    if waited < 0 {
        if recover_from_a_transfer_failure(entry_points, pcm, device_name, waited, direction)
            == AlsaTransferRecovery::StreamIsUnusable
        {
            return Err(entry_points.refuse(
                format_args!("waiting on '{device_name}' for {}", direction.as_word()),
                waited,
            ));
        }
        return Ok(DevicePeriodReadiness::NothingYet);
    }
    Ok(DevicePeriodReadiness::Ready)
}

/// Wait for a period, then fill `status` with the device's own account of where
/// it is.
///
/// Status is read *before* the samples are, which is what makes "status minus
/// reported delay" name the first sample of the block about to be read: at this
/// instant the reported delay is exactly the samples captured and not yet
/// handed over, and the oldest of them is the one the next read starts at.
pub(super) fn read_status_of_a_readable_period(
    entry_points: &AlsaLibraryEntryPoints,
    pcm: *mut c_void,
    device_status: &AlsaAllocatedObject,
    device_name: &str,
) -> Result<DevicePeriodReadiness> {
    if wait_for_the_device_to_release_a_period(
        entry_points,
        pcm,
        device_name,
        AlsaStreamDirection::Capture,
    )? == DevicePeriodReadiness::NothingYet
    {
        return Ok(DevicePeriodReadiness::NothingYet);
    }

    // SAFETY: `pcm` is open and the status is a live libasound allocation.
    entry_points.refuse_a_negative_return_code(
        format_args!("reading the device status of '{device_name}'"),
        unsafe { (entry_points.snd_pcm_status)(pcm, device_status.pointer()) },
    )?;
    Ok(DevicePeriodReadiness::Ready)
}

/// Whether a stream survived a failed transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AlsaTransferRecovery {
    TransferMayContinue,
    StreamIsUnusable,
}

/// Ask libasound to put a broken stream back together, and say whether the
/// transfer may continue.
///
/// A break leaves a gap, and the gap is left visible rather than papered over:
/// on capture the timestamps and sample counts either side of it say exactly
/// how much audio went missing, and on playback the count of what the device
/// had to be given instead does.
///
/// One function for both directions because everything but the restart is the
/// same, and the restart is the one thing that must *not* be: a capture stream
/// this arm started by hand has to be started again, and starting a prepared
/// playback stream whose buffer is empty underruns it on the spot.
pub(super) fn recover_from_a_transfer_failure(
    entry_points: &AlsaLibraryEntryPoints,
    pcm: *mut c_void,
    device_name: &str,
    error_code: c_int,
    direction: AlsaStreamDirection,
) -> AlsaTransferRecovery {
    // SAFETY: `pcm` is an open handle held by this thread alone.
    let recovered = unsafe { (entry_points.snd_pcm_recover)(pcm, error_code, 1) };
    if recovered < 0 {
        tracing::error!(
            device = %device_name,
            "ALSA {} could not recover from {}: {}",
            direction.as_word(),
            entry_points.error_text(error_code),
            entry_points.error_text(recovered)
        );
        return AlsaTransferRecovery::StreamIsUnusable;
    }
    // Which state recovery left the stream in depends on what broke it, so the
    // stream is asked rather than assumed. `snd_pcm_prepare` against a running
    // PCM is `EBUSY` on a raw device — and silently fine through the PipeWire
    // ALSA plugin, which is why only a rig with real hardware would ever show
    // it.
    // SAFETY: `pcm` is an open handle held by this thread alone.
    let state_recovery_left = unsafe { (entry_points.snd_pcm_state)(pcm) };
    if direction == AlsaStreamDirection::Capture
        && a_recovered_stream_still_has_to_be_started(state_recovery_left)
        && let Err(refusal) = prepare_and_start_capture(entry_points, pcm, device_name)
    {
        tracing::error!(
            device = %device_name,
            %refusal,
            "ALSA capture recovered but could not be restarted"
        );
        return AlsaTransferRecovery::StreamIsUnusable;
    }
    if error_code == ALSA_BROKEN_PIPE || error_code == ALSA_SUSPENDED {
        tracing::warn!(
            device = %device_name,
            "ALSA {} recovered from {} — the audio it covers is missing, and {}",
            direction.as_word(),
            entry_points.error_text(error_code),
            direction.where_the_missing_audio_shows_up()
        );
    }
    AlsaTransferRecovery::TransferMayContinue
}

/// Whether a stream `snd_pcm_recover` handed back still has to be started.
///
/// Recovery does not mean the same thing for every failure: an overrun comes
/// back `PREPARED` and must be started again, because this arm disables ALSA's
/// implicit start — but an interrupted wait, and a suspend whose resume
/// succeeded, come back still `RUNNING`, where preparing is `EBUSY` and the
/// stream needs nothing.
pub(super) fn a_recovered_stream_still_has_to_be_started(state_recovery_left: c_int) -> bool {
    state_recovery_left != SND_PCM_STATE_RUNNING
}

/// The device's own timing for its most recent hardware pointer update, in
/// nanoseconds on the clock the software parameters demanded.
///
/// # Safety
///
/// `device_status` must have been filled by a successful `snd_pcm_status` call.
pub(super) unsafe fn read_htimestamp_ns(
    entry_points: &AlsaLibraryEntryPoints,
    device_status: &AlsaAllocatedObject,
) -> i64 {
    let mut device_stamp = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: the caller guarantees a filled status, and the out-parameter is an
    // owned local.
    unsafe {
        (entry_points.snd_pcm_status_get_htstamp)(device_status.pointer(), &mut device_stamp)
    };
    device_stamp
        .tv_sec
        .saturating_mul(1_000_000_000)
        .saturating_add(device_stamp.tv_nsec)
}

/// When the first sample of the block about to be read was captured.
///
/// `device_stamp_ns` is the device's time for its most recent hardware pointer
/// update — the newest sample it holds — and `unread_sample_count` is how many
/// samples it holds that have not been read. So the oldest unread sample, which
/// is the block's first, sits that many samples in the past. This is the value
/// a consumer joins a camera frame on, and it is not the instant of delivery: a
/// `MediaClock::now()`-at-publish stamp would land a whole block later.
pub(super) fn first_sample_timestamp_ns(
    device_stamp_ns: i64,
    unread_sample_count: SndPcmSframes,
    sample_rate: u32,
) -> i64 {
    if sample_rate == 0 || unread_sample_count <= 0 {
        return device_stamp_ns;
    }
    let unread_nanoseconds =
        i128::from(unread_sample_count) * 1_000_000_000 / i128::from(sample_rate);
    device_stamp_ns.saturating_sub(i64::try_from(unread_nanoseconds).unwrap_or(i64::MAX))
}

/// Refuse a device whose own stamp is not on the machine monotonic clock.
///
/// Setting `SND_PCM_TSTAMP_TYPE_MONOTONIC` is a request, and a driver or an
/// ALSA plugin is free to fill `htstamp` with something else — or with nothing.
/// Publishing such a value would corrupt every A/V join downstream in a way
/// nothing later can detect.
///
/// The bracket is taken around the stream's own start, so nothing the device
/// captured can legitimately fall outside it: a wall-clock stamp misses by five
/// decades, and a zeroed one — the device reporting no time at all — misses by
/// however long the machine has been up.
pub(super) fn refuse_a_stamp_outside_the_monotonic_domain(
    device_name: &str,
    device_stamp_ns: i64,
    started_at_ns: i64,
    read_back_at_ns: i64,
) -> Result<()> {
    if (started_at_ns..=read_back_at_ns).contains(&device_stamp_ns) {
        return Ok(());
    }
    Err(Error::NotSupported(format!(
        "ALSA device '{device_name}' stamped its first period at {device_stamp_ns} ns, \
         outside the CLOCK_MONOTONIC bracket [{started_at_ns}, {read_back_at_ns}] the stream \
         ran in — it ignored SND_PCM_TSTAMP_TYPE_MONOTONIC, so its stamps cannot be joined \
         with a frame timestamp"
    )))
}

pub(super) fn monotonic_now_ns() -> i64 {
    MediaClock::now().as_nanos() as i64
}
