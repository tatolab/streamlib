// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Negotiating a PCM's hardware parameters, sample format, capture timestamps and playback start.

use super::*;

/// What the hardware parameter pass settled on.
pub(super) struct NegotiatedAlsaStream {
    pub(super) stream_format: AudioStreamFormat,
    pub(super) period_sample_count: u32,
}

/// Settle the hardware parameters both directions share, aiming at
/// `requested_format` wherever it names a value and at the direction's
/// preference elsewhere. A plug PCM such as `default` converts to whatever was
/// aimed at; a raw device settles where it can, and the caller compares.
///
/// The software pass is the caller's, because it is the half that differs: a
/// capture stream states the timestamp contract there and a playback stream
/// states when the device starts.
pub(super) fn negotiate_hardware_parameters(
    entry_points: &Arc<AlsaLibraryEntryPoints>,
    pcm: *mut c_void,
    pcm_name: &str,
    direction: AlsaStreamDirection,
    requested_format: RequestedAudioStreamFormat,
) -> Result<NegotiatedAlsaStream> {
    let hardware_parameters = AlsaAllocatedObject::hardware_parameters(entry_points)?;

    // SAFETY (every call in this function): `pcm` is an open capture handle and
    // the parameter object is a live libasound allocation; every out-parameter
    // is an owned local outliving its call.
    entry_points.refuse_a_negative_return_code(
        format_args!("reading the parameter space of '{pcm_name}'"),
        unsafe { (entry_points.snd_pcm_hw_params_any)(pcm, hardware_parameters.pointer()) },
    )?;
    entry_points.refuse_a_negative_return_code(
        format_args!("interleaved read/write access"),
        unsafe {
            (entry_points.snd_pcm_hw_params_set_access)(
                pcm,
                hardware_parameters.pointer(),
                SND_PCM_ACCESS_RW_INTERLEAVED,
            )
        },
    )?;

    let sample_format = negotiate_sample_format(
        entry_points,
        pcm,
        hardware_parameters.pointer(),
        requested_format.sample_format,
    )?;

    let mut channels = requested_format
        .channels
        .unwrap_or_else(|| direction.preferred_channels());
    entry_points.refuse_a_negative_return_code(format_args!("a channel count"), unsafe {
        (entry_points.snd_pcm_hw_params_set_channels_near)(
            pcm,
            hardware_parameters.pointer(),
            &mut channels,
        )
    })?;

    let mut sample_rate = requested_format
        .sample_rate
        .unwrap_or(PREFERRED_SAMPLE_RATE);
    entry_points.refuse_a_negative_return_code(format_args!("a sample rate"), unsafe {
        (entry_points.snd_pcm_hw_params_set_rate_near)(
            pcm,
            hardware_parameters.pointer(),
            &mut sample_rate,
            std::ptr::null_mut(),
        )
    })?;

    let mut period_sample_count = PREFERRED_PERIOD_SAMPLE_COUNT;
    entry_points.refuse_a_negative_return_code(format_args!("a period size"), unsafe {
        (entry_points.snd_pcm_hw_params_set_period_size_near)(
            pcm,
            hardware_parameters.pointer(),
            &mut period_sample_count,
            std::ptr::null_mut(),
        )
    })?;

    let mut device_buffer_sample_count =
        period_sample_count.saturating_mul(PERIODS_PER_DEVICE_BUFFER);
    entry_points.refuse_a_negative_return_code(format_args!("a device buffer size"), unsafe {
        (entry_points.snd_pcm_hw_params_set_buffer_size_near)(
            pcm,
            hardware_parameters.pointer(),
            &mut device_buffer_sample_count,
        )
    })?;

    entry_points.refuse_a_negative_return_code(
        format_args!("the negotiated hardware parameters for '{pcm_name}'"),
        unsafe { (entry_points.snd_pcm_hw_params)(pcm, hardware_parameters.pointer()) },
    )?;

    // Read back rather than trusting the requests: every `_near` setter is free
    // to land somewhere else, and what the stream reports has to be what the
    // device is actually doing.
    let mut settled_channels = 0;
    entry_points.refuse_a_negative_return_code(
        format_args!("reading back the channel count"),
        unsafe {
            (entry_points.snd_pcm_hw_params_get_channels)(
                hardware_parameters.pointer(),
                &mut settled_channels,
            )
        },
    )?;
    let mut settled_sample_rate = 0;
    entry_points.refuse_a_negative_return_code(
        format_args!("reading back the sample rate"),
        unsafe {
            (entry_points.snd_pcm_hw_params_get_rate)(
                hardware_parameters.pointer(),
                &mut settled_sample_rate,
                std::ptr::null_mut(),
            )
        },
    )?;
    let mut settled_period_sample_count = 0;
    entry_points.refuse_a_negative_return_code(
        format_args!("reading back the period size"),
        unsafe {
            (entry_points.snd_pcm_hw_params_get_period_size)(
                hardware_parameters.pointer(),
                &mut settled_period_sample_count,
                std::ptr::null_mut(),
            )
        },
    )?;
    if settled_sample_rate == 0 || settled_channels == 0 || settled_period_sample_count == 0 {
        return Err(Error::Runtime(format!(
            "ALSA settled '{pcm_name}' on {settled_sample_rate} Hz, {settled_channels} \
             channels and a {settled_period_sample_count}-sample period — no block duration \
             is derivable from that"
        )));
    }

    Ok(NegotiatedAlsaStream {
        stream_format: AudioStreamFormat {
            sample_rate: settled_sample_rate,
            channels: settled_channels,
            sample_format,
        },
        period_sample_count: u32::try_from(settled_period_sample_count).map_err(|_| {
            Error::Runtime(format!(
                "ALSA settled '{pcm_name}' on a period of {settled_period_sample_count} \
                 samples, which no block can carry"
            ))
        })?,
    })
}

/// Take `requested_sample_format` if one was asked for; otherwise float samples
/// if the device offers them, 16-bit integers else — the two encodings
/// `AudioBlock`'s `dtype` names.
fn negotiate_sample_format(
    entry_points: &AlsaLibraryEntryPoints,
    pcm: *mut c_void,
    hardware_parameters: *mut c_void,
    requested_sample_format: Option<AudioSampleFormat>,
) -> Result<AudioSampleFormat> {
    for (alsa_format, sample_format) in [
        (SND_PCM_FORMAT_FLOAT_LE, AudioSampleFormat::F32),
        (SND_PCM_FORMAT_S16_LE, AudioSampleFormat::I16),
    ]
    .into_iter()
    .filter(|&(_, sample_format)| {
        requested_sample_format.is_none_or(|requested| requested == sample_format)
    }) {
        // SAFETY: `pcm` is an open capture handle and `hardware_parameters` a
        // live libasound allocation.
        let format_set = unsafe {
            (entry_points.snd_pcm_hw_params_set_format)(pcm, hardware_parameters, alsa_format)
        };
        if format_set >= 0 {
            return Ok(sample_format);
        }
    }
    Err(Error::NotSupported(match requested_sample_format {
        Some(requested) => format!(
            "this ALSA device does not offer the little-endian {requested:?} samples that were \
             requested"
        ),
        None => "this ALSA device offers neither little-endian float nor little-endian 16-bit \
                 integer samples, and those are the two encodings an AudioBlock's dtype names"
            .to_string(),
    }))
}

/// State the timestamp contract on a capture stream before it runs.
///
/// This is the whole reason the arm can be trusted for A/V join: without the
/// tstamp *mode* `snd_pcm_status` reports no time at all, and without the
/// tstamp *type* it reports one on the wrong clock.
pub(super) fn negotiate_capture_timestamp_contract(
    entry_points: &Arc<AlsaLibraryEntryPoints>,
    pcm: *mut c_void,
    period_sample_count: SndPcmUframes,
    pcm_name: &str,
) -> Result<()> {
    let software_parameters = AlsaAllocatedObject::software_parameters(entry_points)?;

    // SAFETY (every call here): `pcm` is an open capture handle whose hardware
    // parameters are applied, and the parameter object is a live libasound
    // allocation; every out-parameter is an owned local outliving its call.
    entry_points.refuse_a_negative_return_code(
        format_args!("reading the software parameters of '{pcm_name}'"),
        unsafe { (entry_points.snd_pcm_sw_params_current)(pcm, software_parameters.pointer()) },
    )?;
    entry_points.refuse_a_negative_return_code(
        format_args!("timestamping on the capture stream"),
        unsafe {
            (entry_points.snd_pcm_sw_params_set_tstamp_mode)(
                pcm,
                software_parameters.pointer(),
                SND_PCM_TSTAMP_ENABLE,
            )
        },
    )?;
    entry_points.refuse_a_negative_return_code(
        format_args!("monotonic timestamps on the capture stream"),
        unsafe {
            (entry_points.snd_pcm_sw_params_set_tstamp_type)(
                pcm,
                software_parameters.pointer(),
                SND_PCM_TSTAMP_TYPE_MONOTONIC,
            )
        },
    )?;

    // Wake the reader once a whole period is readable, which is what makes
    // "status minus reported delay" name the first sample of the block about to
    // be read rather than a sample still being captured.
    entry_points.refuse_a_negative_return_code(
        format_args!("a one-period wake threshold"),
        unsafe {
            (entry_points.snd_pcm_sw_params_set_avail_min)(
                pcm,
                software_parameters.pointer(),
                period_sample_count,
            )
        },
    )?;

    // The stream's `boundary` is ALSA's own "unreachable frame count", and a
    // start threshold of it is the API's way to spell "never start implicitly".
    // A capture stream starts when `snd_pcm_start` says so, so that the
    // monotonic bracket taken around the start is a real bracket.
    let mut never_start_implicitly = 0;
    entry_points.refuse_a_negative_return_code(
        format_args!("reading the stream's frame boundary"),
        unsafe {
            (entry_points.snd_pcm_sw_params_get_boundary)(
                software_parameters.pointer(),
                &mut never_start_implicitly,
            )
        },
    )?;
    entry_points.refuse_a_negative_return_code(
        format_args!("an explicit start threshold"),
        unsafe {
            (entry_points.snd_pcm_sw_params_set_start_threshold)(
                pcm,
                software_parameters.pointer(),
                never_start_implicitly,
            )
        },
    )?;

    entry_points.refuse_a_negative_return_code(
        format_args!("the timestamp contract on '{pcm_name}'"),
        unsafe { (entry_points.snd_pcm_sw_params)(pcm, software_parameters.pointer()) },
    )
}

/// State when a playback stream starts, before a sample is written.
///
/// The start threshold is the whole of it: at one period the device begins
/// playing as soon as the first period is queued, which is why the writer
/// thread never calls `snd_pcm_start` — and why recovery must not either,
/// since starting a prepared stream whose buffer is empty underruns it on the
/// spot.
pub(super) fn negotiate_playback_start_contract(
    entry_points: &Arc<AlsaLibraryEntryPoints>,
    pcm: *mut c_void,
    period_sample_count: SndPcmUframes,
    pcm_name: &str,
) -> Result<()> {
    let software_parameters = AlsaAllocatedObject::software_parameters(entry_points)?;

    // SAFETY (every call here): `pcm` is an open playback handle whose hardware
    // parameters are applied, and the parameter object is a live libasound
    // allocation; every out-parameter is an owned local outliving its call.
    entry_points.refuse_a_negative_return_code(
        format_args!("reading the software parameters of '{pcm_name}'"),
        unsafe { (entry_points.snd_pcm_sw_params_current)(pcm, software_parameters.pointer()) },
    )?;

    // Wake the writer once a whole period of room is free, so it assembles one
    // period per wake rather than dribbling frames in.
    entry_points.refuse_a_negative_return_code(
        format_args!("a one-period wake threshold"),
        unsafe {
            (entry_points.snd_pcm_sw_params_set_avail_min)(
                pcm,
                software_parameters.pointer(),
                period_sample_count,
            )
        },
    )?;

    entry_points.refuse_a_negative_return_code(
        format_args!("a one-period start threshold"),
        unsafe {
            (entry_points.snd_pcm_sw_params_set_start_threshold)(
                pcm,
                software_parameters.pointer(),
                period_sample_count,
            )
        },
    )?;

    entry_points.refuse_a_negative_return_code(
        format_args!("the playback start contract on '{pcm_name}'"),
        unsafe { (entry_points.snd_pcm_sw_params)(pcm, software_parameters.pointer()) },
    )
}
