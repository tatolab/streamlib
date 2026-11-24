// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn asking_for_realtime_on_a_pipewire_audio_thread_leaves_it_running_when_refused() {
    let (asked_sender, asked_receiver) = std::sync::mpsc::channel::<()>();
    let (thread_id_sender, thread_id_receiver) = std::sync::mpsc::channel::<u64>();
    let audio_thread = std::thread::Builder::new()
        .name("streamlib-audio".to_string())
        .spawn(move || {
            // SAFETY: `SYS_gettid` takes no arguments and cannot fail.
            let thread_id = unsafe { libc::syscall(libc::SYS_gettid) } as u64;
            thread_id_sender.send(thread_id).unwrap();
            asked_receiver.recv().unwrap();
        })
        .unwrap();
    thread_id_receiver.recv().unwrap();

    ask_for_realtime_on_every_pipewire_audio_thread_of_this_process();

    assert!(!audio_thread.is_finished());
    asked_sender.send(()).unwrap();
    audio_thread.join().unwrap();
}

const RATE_48K_NUMERATOR: u32 = 1;
const RATE_48K_DENOMINATOR: u32 = 48_000;
const QUANTUM_SAMPLES: u32 = 1024;
/// 1024 samples at 48 kHz, truncated the way the derivation truncates.
const QUANTUM_NANOS: i64 = 21_333_333;

fn first_sample_timestamp_ns(cycle_timestamp_ns: i64, delay_in_rate_units: i64) -> i64 {
    unsafe {
        audio_shim::streamlib_pipewire_first_sample_timestamp_ns(
            cycle_timestamp_ns,
            delay_in_rate_units,
            RATE_48K_NUMERATOR,
            RATE_48K_DENOMINATOR,
            QUANTUM_SAMPLES,
            48_000,
        )
    }
}

/// The whole of the arm's timestamp claim, on a machine with no audio
/// server: the value names the *first* sample, so it sits one block before
/// the cycle that delivered it — not at the cycle, which is what a
/// publish-time stamp would give and what a rig assertion measures.
///
/// Mental revert: drop the block-duration term and this returns the cycle
/// time, putting the stamp at the block's end and making a joined camera
/// frame one quantum early.
#[test]
fn a_blocks_stamp_is_one_block_before_the_cycle_that_delivered_it() {
    let cycle_timestamp_ns = 541_560_469_372_719;
    assert_eq!(
        first_sample_timestamp_ns(cycle_timestamp_ns, 0),
        cycle_timestamp_ns - QUANTUM_NANOS
    );
}

/// A device that reports travel time pushes the capture instant further
/// back by exactly that much, converted out of the graph's rate units.
#[test]
fn a_reported_device_delay_moves_the_stamp_further_into_the_past() {
    let cycle_timestamp_ns = 1_000_000_000;
    let delay_of_one_quantum = i64::from(QUANTUM_SAMPLES);
    assert_eq!(
        first_sample_timestamp_ns(cycle_timestamp_ns, delay_of_one_quantum),
        cycle_timestamp_ns - QUANTUM_NANOS - QUANTUM_NANOS,
        "a delay of one quantum in rate units is one quantum of nanoseconds"
    );
}

/// The shim's half of the death path, held without a daemon: the state
/// change lands on the loop thread, and what it leaves behind is a reason
/// the stream's owner can read from its own thread.
///
/// Mental revert: drop the record and this arm is back to a stream that
/// entered its error state, said so once in a log, and went on looking
/// healthy to everything holding it.
#[test]
fn a_failure_the_shim_reports_lands_in_the_report_the_owner_holds() {
    let (failure_recorder, liveness_report) =
        DeviceStreamFailureRecorder::recording_into_a_new_report();
    let reason_the_daemon_gave = CString::new("node destroyed").expect("no interior NUL");

    // SAFETY: the context is the address of a live report, and the reason
    // is a NUL-terminated string that outlives the call — exactly what the
    // shim hands over on the loop thread.
    unsafe {
        record_a_stream_failure_in_the_liveness_report(
            (&raw const failure_recorder).cast_mut().cast::<c_void>(),
            reason_the_daemon_gave.as_ptr(),
        );
    }

    let reason = liveness_report
        .failure_that_ended_the_stream()
        .expect("a stream that entered its error state has to leave its owner a reason")
        .to_string();
    assert!(
        reason.contains("node destroyed") && reason.contains("PipeWire"),
        "the reason has to carry both the daemon's own words and which arm they came \
             from: {reason}"
    );
}

/// `pw_stream_events::state_changed` may hand over a NULL error, and a
/// stream that failed for reasons the daemon did not spell still has to be
/// reported as failed — silence here would be the bug this change exists
/// to remove.
#[test]
fn a_failure_the_daemon_did_not_explain_is_still_reported_as_one() {
    let (failure_recorder, liveness_report) =
        DeviceStreamFailureRecorder::recording_into_a_new_report();

    // SAFETY: as above, with the NULL reason libpipewire is allowed to
    // pass.
    unsafe {
        record_a_stream_failure_in_the_liveness_report(
            (&raw const failure_recorder).cast_mut().cast::<c_void>(),
            std::ptr::null(),
        );
    }

    assert!(
        liveness_report.failure_that_ended_the_stream().is_some(),
        "a stream that failed without a message is still a stream that failed"
    );
}

/// Consecutive cycles are one block apart, which is what makes
/// `first_sample_timestamp_ns + sample_count / sample_rate` the next
/// block's expected stamp — the property a consumer joins on.
#[test]
fn consecutive_cycles_are_exactly_one_block_apart() {
    let first_cycle_ns = 1_000_000_000;
    let second_cycle_ns = first_cycle_ns + QUANTUM_NANOS;
    assert_eq!(
        first_sample_timestamp_ns(second_cycle_ns, 0)
            - first_sample_timestamp_ns(first_cycle_ns, 0),
        QUANTUM_NANOS
    );
}

/// A rate fraction of 0/0 is what a stream that has not settled reports,
/// and no delay is derivable from it — the stamp must still be the cycle
/// minus the block rather than a division by zero.
#[test]
fn an_unsettled_rate_fraction_contributes_no_delay_rather_than_dividing_by_zero() {
    let stamp = unsafe {
        audio_shim::streamlib_pipewire_first_sample_timestamp_ns(
            1_000_000_000,
            512,
            0,
            0,
            QUANTUM_SAMPLES,
            48_000,
        )
    };
    assert_eq!(stamp, 1_000_000_000 - QUANTUM_NANOS);
}

pub(super) fn monitored_sink_name_of(device_id: &str) -> Option<String> {
    let device_id = CString::new(device_id).expect("a test device id has no NUL");
    let length = unsafe {
        audio_shim::streamlib_pipewire_sink_name_length_of_monitor_device_id(device_id.as_ptr())
    };
    (length > 0).then(|| device_id.to_string_lossy()[..length].to_string())
}

/// `STREAMLIB_PIPEWIRE_MAX_STREAM_PROPERTIES`, restated. The shim refuses to
/// compose anything when handed a smaller capacity than its own maximum, so
/// a property added on the C side without updating this fails these tests
/// loudly rather than overflowing the array.
const MAX_STREAM_PROPERTIES: usize = 6;

/// The key/value pairs the shim would announce a stream with, read back as
/// owned strings.
fn composed_stream_properties(
    direction: c_int,
    device_id: Option<&str>,
) -> Option<Vec<(String, String)>> {
    let device_id = device_id.map(|id| CString::new(id).expect("a test device id has no NUL"));
    let mut items = [audio_shim::StreamProperty {
        key: std::ptr::null(),
        value: std::ptr::null(),
    }; MAX_STREAM_PROPERTIES];
    let mut sink_name = [0u8; 256];
    let count = unsafe {
        audio_shim::streamlib_pipewire_stream_properties(
            items.as_mut_ptr(),
            MAX_STREAM_PROPERTIES as u32,
            direction,
            device_id
                .as_ref()
                .map_or(std::ptr::null(), |id| id.as_ptr()),
            sink_name.as_mut_ptr().cast::<c_char>(),
            sink_name.len(),
        )
    };
    if count == 0 {
        return None;
    }
    Some(
        items[..count as usize]
            .iter()
            .map(|item| unsafe {
                (
                    CStr::from_ptr(item.key).to_string_lossy().into_owned(),
                    CStr::from_ptr(item.value).to_string_lossy().into_owned(),
                )
            })
            .collect(),
    )
}

/// The commit's actual behaviour, not just its string parsing: a
/// `<sink>.monitor` id has to reach PipeWire as the bare sink name *plus*
/// `stream.capture.sink`, because a sink named without that property is
/// answered with the session's default source.
///
/// Mental revert: drop the `stream.capture.sink` item and this reddens —
/// which nothing did before, so the fix was held only by a manual rig run.
#[test]
fn a_monitor_device_id_asks_pipewire_for_the_sinks_monitor() {
    let properties = composed_stream_properties(
        audio_shim::STREAM_DIRECTION_CAPTURE,
        Some("streamlib-fixture-audio-sink.monitor"),
    )
    .expect("a monitor id composes properties");
    assert!(
        properties.contains(&(
            "target.object".to_string(),
            "streamlib-fixture-audio-sink".to_string()
        )),
        "the target is the sink itself, not the `.monitor` spelling: {properties:?}"
    );
    assert!(
        properties.contains(&("stream.capture.sink".to_string(), "true".to_string())),
        "without this the stream attaches to the default source: {properties:?}"
    );
}

/// A plain device id is targeted as itself and gains no capture-sink flag,
/// which would otherwise look for a monitor a source does not have.
#[test]
fn a_plain_device_id_is_targeted_as_an_ordinary_source() {
    let properties = composed_stream_properties(
        audio_shim::STREAM_DIRECTION_CAPTURE,
        Some("alsa_input.pci-0000_00_1f.3"),
    )
    .expect("a plain id composes properties");
    assert!(properties.contains(&(
        "target.object".to_string(),
        "alsa_input.pci-0000_00_1f.3".to_string()
    )));
    assert!(
        !properties
            .iter()
            .any(|(key, _)| key == "stream.capture.sink"),
        "a source has no monitor to capture: {properties:?}"
    );
}

/// No device named: the session routes the stream to its own default, so
/// nothing targets anything.
#[test]
fn no_device_id_names_no_target_at_all() {
    let properties = composed_stream_properties(audio_shim::STREAM_DIRECTION_CAPTURE, None)
        .expect("the default composes");
    assert!(!properties.iter().any(|(key, _)| key == "target.object"));
    assert!(
        !properties
            .iter()
            .any(|(key, _)| key == "stream.capture.sink")
    );
}

/// A monitor id whose sink name will not fit composes nothing, which the
/// caller turns into a named refusal. Falling through to a plain target
/// would be the very bug the convention exists to remove.
#[test]
fn an_over_long_monitor_device_id_composes_nothing_rather_than_a_plain_target() {
    let over_long = format!("{}.monitor", "s".repeat(400));
    assert!(
        composed_stream_properties(audio_shim::STREAM_DIRECTION_CAPTURE, Some(&over_long))
            .is_none()
    );
}

/// WirePlumber links a capture stream whose `target.object` it cannot find to the session's
/// default source unless the stream sets `node.dont-fallback`, so every named capture device
/// carries it, on the monitor path too; the default and playback streams carry none.
#[test]
fn a_named_capture_device_asks_the_session_not_to_fall_back_to_its_default() {
    let dont_fallback = ("node.dont-fallback".to_string(), "true".to_string());
    for device_id in [
        "alsa_input.usb-gone.mono",
        "streamlib-fixture-audio-sink.monitor",
    ] {
        let properties =
            composed_stream_properties(audio_shim::STREAM_DIRECTION_CAPTURE, Some(device_id))
                .expect("a named device composes properties");
        assert!(
            properties.contains(&dont_fallback),
            "{device_id}: {properties:?}"
        );
    }
    for (direction, device_id) in [
        (audio_shim::STREAM_DIRECTION_CAPTURE, None),
        (
            audio_shim::STREAM_DIRECTION_PLAYBACK,
            Some("streamlib-fixture-audio-sink"),
        ),
    ] {
        let properties = composed_stream_properties(direction, device_id)
            .expect("the stream composes properties");
        assert!(
            !properties
                .iter()
                .any(|(key, _)| key == "node.dont-fallback"),
            "{device_id:?}: {properties:?}"
        );
    }
}

fn a_capture_request_for(device_id: Option<&str>) -> AudioDeviceStreamRequest {
    AudioDeviceStreamRequest {
        device_id: device_id.map(str::to_owned),
        deviceless_pacing_clock: Arc::new(crate::core::context::SoftwareAudioClock::with_defaults()),
        capture_device_access_prompting: Default::default(),
        requested_format: Default::default(),
    }
}

fn a_session_listing_one_source() -> AudioCaptureDeviceListing {
    AudioCaptureDeviceListing {
        devices: vec![crate::core::context::AudioCaptureDevice {
            id: "alsa_input.usb-mic.mono".to_owned(),
            name: "USB Microphone".to_owned(),
            kind: crate::core::context::AudioCaptureDeviceKind::Input,
        }],
        default_output_monitor_device_id: None,
    }
}

/// Whether the capture open went on to open a stream, and on which device.
fn the_device_a_capture_open_reached(device_id: Option<&str>) -> Result<Option<String>> {
    open_only_a_capture_device_the_session_lists(
        &a_capture_request_for(device_id),
        || Ok(a_session_listing_one_source()),
        |request| Ok(request.device_id.clone()),
    )
}

#[test]
fn a_capture_open_on_a_device_the_session_does_not_list_is_refused_before_opening() {
    let refusal = the_device_a_capture_open_reached(Some("alsa_input.usb-gone.mono"))
        .expect_err("the device is not listed")
        .to_string();
    assert!(
        refusal.contains("'alsa_input.usb-gone.mono'")
            && refusal.contains("alsa_input.usb-mic.mono (USB Microphone)"),
        "{refusal}"
    );
}

#[test]
fn a_capture_open_on_a_listed_device_opens_it() {
    assert_eq!(
        the_device_a_capture_open_reached(Some("alsa_input.usb-mic.mono")).unwrap(),
        Some("alsa_input.usb-mic.mono".to_owned())
    );
}

#[test]
fn a_capture_open_on_the_default_device_opens_without_asking_for_the_listing() {
    let opened = open_only_a_capture_device_the_session_lists(
        &a_capture_request_for(None),
        || panic!("the default device needs no listing"),
        |request| Ok(request.device_id.clone()),
    );
    assert_eq!(opened.unwrap(), None);
}

/// The direction the monitor convention does *not* apply to: a speaker
/// pointed at `<sink>.monitor` wants that sink, and asking for its monitor
/// would target a capture endpoint nothing can be played into.
///
/// Mental revert: share one property composition across both directions and
/// a playback stream announces itself as `Capture` with
/// `stream.capture.sink` set — which is how audio ends up going nowhere
/// while every log line says the stream connected.
#[test]
fn a_playback_stream_targets_the_sink_itself_and_announces_itself_as_playback() {
    let properties = composed_stream_properties(
        audio_shim::STREAM_DIRECTION_PLAYBACK,
        Some("streamlib-fixture-audio-sink"),
    )
    .expect("a playback target composes properties");
    assert!(
        properties.contains(&("media.category".to_string(), "Playback".to_string())),
        "a playback stream announces the direction it runs in: {properties:?}"
    );
    assert!(
        properties.contains(&(
            "target.object".to_string(),
            "streamlib-fixture-audio-sink".to_string()
        )),
        "the target is the sink that was named: {properties:?}"
    );
    assert!(
        !properties
            .iter()
            .any(|(key, _)| key == "stream.capture.sink"),
        "nothing is captured on a playback stream: {properties:?}"
    );
}

/// A `.monitor` suffix is a capture spelling, and a speaker handed one must
/// not quietly become a capture stream — it targets what it was given and
/// PipeWire refuses the link if that names nothing playable.
#[test]
fn a_playback_stream_never_takes_the_monitor_path() {
    let properties = composed_stream_properties(
        audio_shim::STREAM_DIRECTION_PLAYBACK,
        Some("streamlib-fixture-audio-sink.monitor"),
    )
    .expect("a playback target composes properties");
    assert!(
        properties.contains(&(
            "target.object".to_string(),
            "streamlib-fixture-audio-sink.monitor".to_string()
        )),
        "the id is passed through rather than having a suffix stripped: {properties:?}"
    );
    assert!(
        !properties
            .iter()
            .any(|(key, _)| key == "stream.capture.sink"),
        "nothing is captured on a playback stream: {properties:?}"
    );
}

/// The capture side keeps saying so, which is what makes the two-value
/// property a real distinction rather than a constant.
#[test]
fn a_capture_stream_announces_itself_as_capture() {
    let properties = composed_stream_properties(audio_shim::STREAM_DIRECTION_CAPTURE, None)
        .expect("the default composes");
    assert!(
        properties.contains(&("media.category".to_string(), "Capture".to_string())),
        "{properties:?}"
    );
}

/// The whole of the monitor convention: a `<sink>.monitor` device id names
/// the sink, and the shim then asks PipeWire for that sink's monitor rather
/// than for a source of that name.
///
/// Mental revert: return 0 here and the stream targets a sink with no
/// `stream.capture.sink`, which PipeWire answers by attaching to the
/// session's default source — silence that looks like a working pipeline.
#[test]
fn a_monitor_device_id_names_the_sink_it_monitors() {
    assert_eq!(
        monitored_sink_name_of("streamlib-fixture-audio-sink.monitor").as_deref(),
        Some("streamlib-fixture-audio-sink")
    );
}

/// An ordinary source is captured as itself, so the convention cannot
/// hijack a device that merely has "monitor" in its name.
#[test]
fn a_plain_device_id_is_not_read_as_a_monitor() {
    assert_eq!(monitored_sink_name_of("alsa_input.pci-0000_00_1f.3"), None);
    assert_eq!(monitored_sink_name_of("my.monitor.device"), None);
    assert_eq!(monitored_sink_name_of(""), None);
}

/// `.monitor` alone names no sink, so it stays an ordinary target and fails
/// as the missing device it is rather than resolving to something.
#[test]
fn the_suffix_alone_names_no_sink() {
    assert_eq!(monitored_sink_name_of(".monitor"), None);
}

fn clamped_chunk_extent(offset: u32, size: u32, maxsize: u32) -> audio_shim::ChunkExtent {
    unsafe { audio_shim::streamlib_pipewire_clamped_chunk_extent(offset, size, maxsize) }
}

/// The ordinary case: a chunk that sits inside its buffer is passed through
/// untouched.
#[test]
fn a_chunk_within_its_buffer_is_left_alone() {
    assert_eq!(
        clamped_chunk_extent(0, 8192, 65536),
        audio_shim::ChunkExtent {
            offset: 0,
            byte_count: 8192
        }
    );
}

/// `spa_chunk` states the offset is taken modulo `maxsize`, so a wrapped
/// offset names a position inside the mapping rather than past its end.
#[test]
fn an_offset_past_the_mapping_wraps_into_it() {
    assert_eq!(clamped_chunk_extent(70000, 16, 65536).offset, 4464);
}

/// The failure this exists to prevent: the pair becomes a Rust slice, so a
/// size the daemon overstates would be a read past the end of PipeWire's
/// mapping — not a bad sample value.
///
/// Mental revert: take `chunk->size` at face value and this returns 999 999
/// bytes out of a 4 096-byte buffer.
#[test]
fn a_size_larger_than_the_mapping_is_clamped_to_what_is_actually_there() {
    assert_eq!(clamped_chunk_extent(0, 999_999, 4096).byte_count, 4096);
    assert_eq!(clamped_chunk_extent(1024, 999_999, 4096).byte_count, 3072);
}

/// A buffer that maps nothing yields nothing, rather than a modulo by zero.
#[test]
fn a_buffer_that_maps_nothing_yields_an_empty_extent() {
    assert_eq!(
        clamped_chunk_extent(16, 64, 0),
        audio_shim::ChunkExtent {
            offset: 0,
            byte_count: 0
        }
    );
}
