// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

use crate::apple::permissions::{
    AvFoundationCaptureDeviceAuthorizationAuthority, PrivacyGatedCaptureDevice,
    microphone_access_for_a_capture_hardware_test,
};
use crate::core::context::{
    AudioBlockRequestedByDevice, AudioCaptureStream, AudioClockConfig, AudioDeviceStreamRequest,
    AudioPlaybackStream, AudioSampleFormat, AudioStreamFormat, CaptureDeviceAccessPrompting,
    CapturedAudioBlockFromDevice, RequestedAudioStreamFormat, SoftwareAudioClock,
};
use crate::core::media_clock::MediaClock;

use super::audio_objects_and_device_discovery::{
    CoreAudioDevice, CoreAudioStreamDirection, channel_count_of, default_device_object_id,
    describe_device, duration_of_frames_in_ns, rate_and_channels_of, stream_format_of,
};
use super::capture_stream::{CoreAudioCaptureStream, MicrophoneAccessForTheStream};
use super::playback_stream::CoreAudioPlaybackStream;
use super::stream_device_binding_and_following::{
    CoreAudioStreamDeviceChange, CoreAudioStreamDevicePolicy, rebind_the_stream_to,
};

const DELIVERY_DEADLINE: Duration = Duration::from_secs(10);

/// Stamps of one binding's consecutive blocks part or overlap by no more
/// than this — far under the shortest cycle a device runs, so a lost
/// cycle shows.
const CONTIGUOUS_STAMP_TOLERANCE_NS: i64 = 500_000;

/// How long after its last sample was captured a block may arrive.
const LONGEST_DELIVERY_DELAY_NS: i64 = 250_000_000;

/// How far before its stamp says a block's last sample was captured it
/// may arrive: the device's timing model, not a block from the future.
const TIMING_MODEL_ALLOWANCE_NS: i64 = 2_000_000;

#[allow(clippy::disallowed_macros)]
fn print_for_the_evidence_record(line: impl std::fmt::Display) {
    println!("{line}");
}

fn the_engine_logs_into_this_tests_output() {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    let _ = tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with(tracing_subscriber::fmt::layer().with_test_writer())
        .try_init();
}

fn now_ns() -> i64 {
    MediaClock::now().as_nanos() as i64
}

/// The system default device in `direction` and its own format, or `None`
/// — said so — where there is none.
fn the_default_device(
    direction: CoreAudioStreamDirection,
) -> Option<(CoreAudioDevice, AudioStreamFormat)> {
    the_engine_logs_into_this_tests_output();
    let Some(object_id) = default_device_object_id(direction) else {
        print_for_the_evidence_record(format!(
            "cannot run: CoreAudio lists no default {} device",
            direction.lowercase_direction_name()
        ));
        return None;
    };
    let device = describe_device(object_id);
    let device_own_format =
        stream_format_of(&device, direction).expect("the default device reports its format");
    print_for_the_evidence_record(format!(
        "default {} device: {device} at {}",
        direction.lowercase_direction_name(),
        rate_and_channels_of(device_own_format)
    ));
    Some((device, device_own_format))
}

fn the_microphone_must_be_allowed() {
    if let Err(instruction) = microphone_access_for_a_capture_hardware_test() {
        panic!("{instruction}");
    }
}

fn a_request_for_the_default_device() -> AudioDeviceStreamRequest {
    AudioDeviceStreamRequest {
        device_id: None,
        deviceless_pacing_clock: Arc::new(SoftwareAudioClock::new(AudioClockConfig::new(
            48_000, 512,
        ))),
        capture_device_access_prompting: CaptureDeviceAccessPrompting::MayAskTheUser,
        requested_format: RequestedAudioStreamFormat::default(),
    }
}

/// A format differing from `device_own_format` in both rate and channel
/// count.
fn a_format_the_device_does_not_carry(device_own_format: AudioStreamFormat) -> AudioStreamFormat {
    AudioStreamFormat {
        sample_rate: if device_own_format.sample_rate == 44_100 {
            48_000
        } else {
            44_100
        },
        channels: if device_own_format.channels == 1 {
            2
        } else {
            1
        },
        sample_format: AudioSampleFormat::F32,
    }
}

#[derive(Debug, Clone, Copy)]
struct CapturedBlockAsRecorded {
    sample_count: u32,
    first_sample_timestamp_ns: i64,
    received_at_ns: i64,
    interleaved_byte_count: usize,
    has_a_nonzero_sample: bool,
    every_frame_has_equal_channels: bool,
}

fn start_recording_blocks(
    stream: &mut CoreAudioCaptureStream,
) -> mpsc::Receiver<CapturedBlockAsRecorded> {
    let channels = stream.stream_format().channels as usize;
    let (block_sender, block_receiver) = mpsc::channel();
    stream
        .start_delivering_to(Box::new(move |block: CapturedAudioBlockFromDevice<'_>| {
            let received_at_ns = now_ns();
            let samples: Vec<f32> = block
                .interleaved_sample_bytes
                .chunks_exact(4)
                .map(|scalar| f32::from_le_bytes([scalar[0], scalar[1], scalar[2], scalar[3]]))
                .collect();
            let _ = block_sender.send(CapturedBlockAsRecorded {
                sample_count: block.sample_count,
                first_sample_timestamp_ns: block.first_sample_timestamp_ns,
                received_at_ns,
                interleaved_byte_count: block.interleaved_sample_bytes.len(),
                has_a_nonzero_sample: samples.iter().any(|&sample| sample != 0.0),
                every_frame_has_equal_channels: samples
                    .chunks_exact(channels)
                    .all(|frame| frame.iter().all(|&sample| sample == frame[0])),
            });
        }))
        .expect("delivery starts");
    block_receiver
}

fn blocks_covering(
    block_receiver: &mpsc::Receiver<CapturedBlockAsRecorded>,
    frame_count: u64,
) -> Vec<CapturedBlockAsRecorded> {
    let mut blocks = Vec::new();
    let mut frames_covered = 0u64;
    while frames_covered < frame_count {
        let block = block_receiver
            .recv_timeout(DELIVERY_DEADLINE)
            .expect("the stream keeps delivering");
        frames_covered += u64::from(block.sample_count);
        blocks.push(block);
    }
    blocks
}

/// Where each block begins against where the one before it ended.
fn gaps_between_consecutive_blocks_ns(
    blocks: &[CapturedBlockAsRecorded],
    sample_rate: u32,
) -> Vec<i64> {
    blocks
        .windows(2)
        .map(|pair| {
            pair[1].first_sample_timestamp_ns
                - (pair[0].first_sample_timestamp_ns
                    + duration_of_frames_in_ns(pair[0].sample_count, sample_rate))
        })
        .collect()
}

/// A binding's blocks follow each other back to back, and each arrives
/// after — and soon after — its last sample was captured.
fn assert_one_bindings_blocks_are_continuous_on_the_host_clock(
    blocks: &[CapturedBlockAsRecorded],
    sample_rate: u32,
    which_binding: &str,
) {
    let largest_gap_ns = gaps_between_consecutive_blocks_ns(blocks, sample_rate)
        .into_iter()
        .map(i64::abs)
        .max()
        .unwrap_or(0);
    let delivery_delays_ns: Vec<i64> = blocks
        .iter()
        .map(|block| {
            block.received_at_ns
                - (block.first_sample_timestamp_ns
                    + duration_of_frames_in_ns(block.sample_count, sample_rate))
        })
        .collect();
    let frames: u64 = blocks
        .iter()
        .map(|block| u64::from(block.sample_count))
        .sum();
    print_for_the_evidence_record(format!(
        "{which_binding}: {} blocks, {frames} frames at {sample_rate} Hz, largest |gap| between \
         consecutive stamps {:.1} µs, delivery delay {:.2}..{:.2} ms",
        blocks.len(),
        largest_gap_ns as f64 / 1e3,
        *delivery_delays_ns.iter().min().unwrap_or(&0) as f64 / 1e6,
        *delivery_delays_ns.iter().max().unwrap_or(&0) as f64 / 1e6,
    ));
    assert!(
        largest_gap_ns <= CONTIGUOUS_STAMP_TOLERANCE_NS,
        "{which_binding}: consecutive stamps part by up to {largest_gap_ns} ns"
    );
    for delivery_delay_ns in delivery_delays_ns {
        assert!(
            (-TIMING_MODEL_ALLOWANCE_NS..LONGEST_DELIVERY_DELAY_NS).contains(&delivery_delay_ns),
            "{which_binding}: a block arrived {delivery_delay_ns} ns after its stamp says its \
             last sample was captured"
        );
    }
}

/// The blocks either side of a rebind: one gap, where the unit was
/// stopped, splits two runs that are each continuous; the stamps move
/// forward across it and the stream delivers on after it.
fn assert_two_continuous_bindings_parted_at_the_rebind(
    blocks: &[CapturedBlockAsRecorded],
    sample_rate: u32,
    rebind_began_ns: i64,
) {
    let gaps_ns = gaps_between_consecutive_blocks_ns(blocks, sample_rate);
    let (last_block_before_the_rebind, rebind_gap_ns) = gaps_ns
        .iter()
        .copied()
        .enumerate()
        .max_by_key(|&(_, gap_ns)| gap_ns)
        .expect("blocks either side of the rebind");
    let (before, after) = blocks.split_at(last_block_before_the_rebind + 1);
    print_for_the_evidence_record(format!(
        "the rebind parted the stamps by {:.2} ms",
        rebind_gap_ns as f64 / 1e6
    ));
    assert!(
        rebind_gap_ns > CONTIGUOUS_STAMP_TOLERANCE_NS && rebind_gap_ns < 1_000_000_000,
        "a rebind stops the unit for a moment, never a second: {rebind_gap_ns} ns"
    );
    assert!(
        after[0].received_at_ns >= rebind_began_ns,
        "the stamps part where the rebind happened"
    );
    assert_one_bindings_blocks_are_continuous_on_the_host_clock(
        before,
        sample_rate,
        "before the rebind",
    );
    assert_one_bindings_blocks_are_continuous_on_the_host_clock(
        after,
        sample_rate,
        "after the rebind",
    );
    let frames_after: u32 = after.iter().map(|block| block.sample_count).sum();
    assert!(
        u64::from(frames_after) * 10 >= u64::from(sample_rate) * 4,
        "blocks keep arriving after the rebind: {frames_after} frames"
    );
}

fn force_a_capture_rebind_to(stream: &CoreAudioCaptureStream, device: &CoreAudioDevice) {
    let device_own_format = stream_format_of(device, CoreAudioStreamDirection::Capture)
        .expect("the device reports its format");
    rebind_the_stream_to(
        &mut *stream.capture_control.lock(),
        device,
        device_own_format,
    );
}

fn a_converter_runs_on(stream: &CoreAudioCaptureStream) -> bool {
    match &stream.capture_control.lock().microphone_access {
        MicrophoneAccessForTheStream::Granted(capture_unit) => capture_unit
            .callback_context
            .delivery
            .lock()
            .format_converter
            .is_some(),
        _ => false,
    }
}

/// The owner's ruling: a stream with no device_id follows the default, and
/// a rebind keeps the stream's hand-off and its stamps' clock. Mental
/// revert: restart without the installed hand-off, and nothing arrives
/// after the rebind.
#[test]
fn an_unnamed_capture_stream_rebound_to_its_default_keeps_delivering_with_continuous_stamps() {
    let Some((default_input, _)) = the_default_device(CoreAudioStreamDirection::Capture) else {
        return;
    };
    the_microphone_must_be_allowed();
    let mut stream = CoreAudioCaptureStream::open(
        &a_request_for_the_default_device(),
        &AvFoundationCaptureDeviceAuthorizationAuthority(PrivacyGatedCaptureDevice::Microphone),
    )
    .expect("an unnamed capture stream opens on the default input");
    assert!(
        stream.capture_control.lock().device_binding.device_policy
            == CoreAudioStreamDevicePolicy::FollowsTheSystemDefault
    );
    let sample_rate = stream.stream_format().sample_rate;
    let block_receiver = start_recording_blocks(&mut stream);
    let mut blocks = blocks_covering(&block_receiver, u64::from(sample_rate) / 2);

    let rebind_began_ns = now_ns();
    force_a_capture_rebind_to(&stream, &default_input);
    assert_eq!(
        stream.liveness_report().failure_that_ended_the_stream(),
        None
    );
    blocks.extend(blocks_covering(&block_receiver, u64::from(sample_rate) / 2));
    stream.stop_delivering().expect("delivery stops");

    assert_two_continuous_bindings_parted_at_the_rebind(&blocks, sample_rate, rebind_began_ns);
    assert_eq!(
        stream.liveness_report().failure_that_ended_the_stream(),
        None
    );
}

/// The path a real change takes: every listener registers, and a change run
/// on the stream's control queue — where the HAL runs the listener blocks —
/// reaches the stream, which keeps delivering.
#[test]
fn an_unnamed_capture_streams_listeners_register_and_a_change_reaches_it_through_its_queue() {
    let Some(_) = the_default_device(CoreAudioStreamDirection::Capture) else {
        return;
    };
    the_microphone_must_be_allowed();
    let mut stream = CoreAudioCaptureStream::open(
        &a_request_for_the_default_device(),
        &AvFoundationCaptureDeviceAuthorizationAuthority(PrivacyGatedCaptureDevice::Microphone),
    )
    .expect("an unnamed capture stream opens on the default input");
    let sample_rate = stream.stream_format().sample_rate;
    let block_receiver = start_recording_blocks(&mut stream);
    blocks_covering(&block_receiver, u64::from(sample_rate) / 4);

    let (handle_a_device_change, stream_control_queue) = {
        let control = stream.capture_control.lock();
        assert!(
            control
                .device_binding
                .system_default_device_listener
                .is_some(),
            "an unnamed stream listens for the system default moving"
        );
        assert_eq!(
            control.device_binding.bound_device_listeners.len(),
            3,
            "liveness, nominal rate and stream configuration are all listened for"
        );
        (
            Arc::clone(&control.device_binding.handle_a_device_change),
            control.device_binding.stream_control_queue.clone(),
        )
    };
    for change in [
        CoreAudioStreamDeviceChange::SystemDefaultDeviceMoved,
        CoreAudioStreamDeviceChange::BoundDeviceFormatChanged,
        CoreAudioStreamDeviceChange::BoundDeviceLivenessChanged,
    ] {
        let handle_a_device_change = Arc::clone(&handle_a_device_change);
        stream_control_queue.exec_async(move || handle_a_device_change(change));
    }
    stream_control_queue.exec_sync(|| {});

    assert_eq!(
        stream.liveness_report().failure_that_ended_the_stream(),
        None,
        "a change that left the default and the device as they were ends nothing"
    );
    let after_the_changes = blocks_covering(&block_receiver, u64::from(sample_rate) / 4);
    assert!(!after_the_changes.is_empty(), "the stream keeps delivering");
    stream.stop_delivering().expect("delivery stops");
}

/// A stream whose device carries another rate and channel count — as
/// AirPods' microphone does once the headset profile engages — is
/// converted to the stream's format, with the room's signal intact and
/// the stamps continuous on the host clock, through a rebind too.
#[test]
fn a_capture_stream_at_a_format_its_device_does_not_carry_converts_with_continuous_stamps() {
    let Some((default_input, device_own_format)) =
        the_default_device(CoreAudioStreamDirection::Capture)
    else {
        return;
    };
    the_microphone_must_be_allowed();
    let stream_format = a_format_the_device_does_not_carry(device_own_format);
    let mut stream = CoreAudioCaptureStream::open_on(
        default_input.clone(),
        stream_format,
        CoreAudioStreamDevicePolicy::FollowsTheSystemDefault,
        &AvFoundationCaptureDeviceAuthorizationAuthority(PrivacyGatedCaptureDevice::Microphone),
        CaptureDeviceAccessPrompting::MayAskTheUser,
    )
    .expect("a capture stream opens at a format its device does not carry");
    assert!(
        a_converter_runs_on(&stream),
        "the unit renders the device's own format and a converter takes it to the stream's"
    );
    let block_receiver = start_recording_blocks(&mut stream);
    let mut blocks = blocks_covering(&block_receiver, u64::from(stream_format.sample_rate));

    let rebind_began_ns = now_ns();
    force_a_capture_rebind_to(&stream, &default_input);
    assert!(
        a_converter_runs_on(&stream),
        "the rebind made a converter again"
    );
    blocks.extend(blocks_covering(
        &block_receiver,
        u64::from(stream_format.sample_rate) / 2,
    ));
    stream.stop_delivering().expect("delivery stops");

    print_for_the_evidence_record(format!(
        "converted {} to {}",
        rate_and_channels_of(device_own_format),
        rate_and_channels_of(stream_format)
    ));
    for block in &blocks {
        assert_eq!(
            block.interleaved_byte_count,
            stream_format.interleaved_byte_count_for(block.sample_count),
            "every block carries the stream's channel count"
        );
    }
    assert!(
        blocks.iter().any(|block| block.has_a_nonzero_sample),
        "the room reaches the stream through the converter"
    );
    if device_own_format.channels == 1 {
        assert!(
            blocks
                .iter()
                .all(|block| block.every_frame_has_equal_channels),
            "a mono device's one channel is copied to each of the stream's"
        );
    }
    assert_two_continuous_bindings_parted_at_the_rebind(
        &blocks,
        stream_format.sample_rate,
        rebind_began_ns,
    );
    assert_eq!(
        stream.liveness_report().failure_that_ended_the_stream(),
        None
    );
}

/// A rebind CoreAudio refuses ends the stream: the failure names the
/// device and the status, and the stream starts no more. An output-only
/// device cannot take an input unit.
#[test]
fn a_capture_rebind_coreaudio_refuses_ends_the_stream_naming_the_device() {
    let Some((default_input, device_own_format)) =
        the_default_device(CoreAudioStreamDirection::Capture)
    else {
        return;
    };
    let Some((default_output, _)) = the_default_device(CoreAudioStreamDirection::Playback) else {
        return;
    };
    if channel_count_of(default_output.object_id, CoreAudioStreamDirection::Capture) > 0 {
        print_for_the_evidence_record(
            "cannot run: the default output device also carries input channels",
        );
        return;
    }
    the_microphone_must_be_allowed();
    let mut stream = CoreAudioCaptureStream::open(
        &a_request_for_the_default_device(),
        &AvFoundationCaptureDeviceAuthorizationAuthority(PrivacyGatedCaptureDevice::Microphone),
    )
    .expect("an unnamed capture stream opens on the default input");
    let block_receiver = start_recording_blocks(&mut stream);
    blocks_covering(
        &block_receiver,
        u64::from(device_own_format.sample_rate) / 10,
    );

    rebind_the_stream_to(
        &mut *stream.capture_control.lock(),
        &default_output,
        device_own_format,
    );

    let failure = stream
        .liveness_report()
        .failure_that_ended_the_stream()
        .expect("a refused rebind ends the stream");
    print_for_the_evidence_record(format!("recorded: {failure}"));
    let failure = failure.to_string();
    assert!(failure.contains(&default_output.uid), "{failure}");
    assert!(failure.contains(&default_input.uid), "{failure}");
    assert!(failure.contains("OSStatus"), "{failure}");
    let restart = stream.start_delivering_to(Box::new(|_block| {}));
    assert!(
        restart.is_err(),
        "a stream a refused rebind ended starts no more"
    );
}

fn start_answering_with_silence(
    stream: &mut CoreAudioPlaybackStream,
) -> mpsc::Receiver<(u32, usize, i64)> {
    let (request_sender, request_receiver) = mpsc::channel();
    stream
        .start_requesting_from(Box::new(
            move |requested: AudioBlockRequestedByDevice<'_>| {
                requested.interleaved_sample_bytes_to_fill.fill(0);
                let _ = request_sender.send((
                    requested.sample_count,
                    requested.interleaved_sample_bytes_to_fill.len(),
                    now_ns(),
                ));
            },
        ))
        .expect("the device starts asking");
    request_receiver
}

fn requests_covering(
    request_receiver: &mpsc::Receiver<(u32, usize, i64)>,
    frame_count: u64,
    received_after_ns: i64,
) -> Vec<(u32, usize, i64)> {
    let mut requests = Vec::new();
    let mut frames_covered = 0u64;
    while frames_covered < frame_count {
        let request = request_receiver
            .recv_timeout(DELIVERY_DEADLINE)
            .expect("the device keeps asking");
        if request.2 > received_after_ns {
            frames_covered += u64::from(request.0);
            requests.push(request);
        }
    }
    requests
}

/// AUHAL's output side converts the client's format to the device's, so
/// a playback stream is asked at its own rate and channel count whatever
/// the device runs at. It plays only zeros.
#[test]
fn a_playback_stream_at_a_format_its_device_does_not_carry_is_asked_at_its_own_rate() {
    let Some((default_output, device_own_format)) =
        the_default_device(CoreAudioStreamDirection::Playback)
    else {
        return;
    };
    let stream_format = a_format_the_device_does_not_carry(device_own_format);
    let mut stream = CoreAudioPlaybackStream::open_on(
        default_output,
        stream_format,
        CoreAudioStreamDevicePolicy::PinnedToTheNamedDevice,
    )
    .expect("AUHAL takes a client format its device does not run at");
    let request_receiver = start_answering_with_silence(&mut stream);
    let requests = requests_covering(
        &request_receiver,
        u64::from(stream_format.sample_rate) * 3 / 2,
        i64::MIN,
    );
    stream.stop_requesting().expect("requests stop");

    for &(sample_count, byte_count, _) in &requests {
        assert_eq!(
            byte_count,
            stream_format.interleaved_byte_count_for(sample_count)
        );
    }
    let (_, _, first_request_at_ns) = requests[0];
    let (_, _, last_request_at_ns) = requests[requests.len() - 1];
    let frames_after_the_first: u64 = requests[1..]
        .iter()
        .map(|&(sample_count, _, _)| u64::from(sample_count))
        .sum();
    let asked_rate_hz =
        frames_after_the_first as f64 * 1e9 / (last_request_at_ns - first_request_at_ns) as f64;
    let request_sizes: std::collections::BTreeSet<u32> = requests
        .iter()
        .map(|&(sample_count, _, _)| sample_count)
        .collect();
    print_for_the_evidence_record(format!(
        "played {} as zeros to a device at {}: asked at {asked_rate_hz:.0} Hz, request sizes \
         {request_sizes:?}",
        rate_and_channels_of(stream_format),
        rate_and_channels_of(device_own_format)
    ));
    let rate_error = (asked_rate_hz / f64::from(stream_format.sample_rate) - 1.0).abs();
    assert!(
        rate_error < 0.02,
        "asked at {asked_rate_hz:.0} Hz for a {} Hz stream",
        stream_format.sample_rate
    );
    assert_eq!(
        stream.liveness_report().failure_that_ended_the_stream(),
        None
    );
}

/// Mental revert: rebind without restarting a running unit, and the
/// device never asks again.
#[test]
fn an_unnamed_playback_stream_rebound_to_its_default_keeps_being_asked_for_samples() {
    let Some((default_output, device_own_format)) =
        the_default_device(CoreAudioStreamDirection::Playback)
    else {
        return;
    };
    let mut stream = CoreAudioPlaybackStream::open(&a_request_for_the_default_device())
        .expect("an unnamed playback stream opens on the default output");
    let request_receiver = start_answering_with_silence(&mut stream);
    requests_covering(
        &request_receiver,
        u64::from(device_own_format.sample_rate) / 4,
        i64::MIN,
    );

    rebind_the_stream_to(
        &mut *stream.playback_control.lock(),
        &default_output,
        device_own_format,
    );
    let rebind_returned_ns = now_ns();
    assert_eq!(
        stream.liveness_report().failure_that_ended_the_stream(),
        None
    );
    let requests_after_the_rebind = requests_covering(
        &request_receiver,
        u64::from(device_own_format.sample_rate) / 4,
        rebind_returned_ns,
    );
    stream.stop_requesting().expect("requests stop");
    print_for_the_evidence_record(format!(
        "after the rebind: {} requests of zeros",
        requests_after_the_rebind.len()
    ));
    assert_eq!(
        stream.liveness_report().failure_that_ended_the_stream(),
        None
    );
}
