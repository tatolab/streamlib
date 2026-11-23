// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use objc2_audio_toolbox::{kAudioUnitScope_Global, kAudioUnitScope_Input};
use objc2_core_audio::{AudioObjectID, kAudioObjectUnknown};

use crate::core::context::{
    AudioCaptureDeviceKind, AudioSampleFormat, AudioStreamFormat, CapturedAudioBlockFromDevice,
    RequestedAudioStreamFormat,
};

use super::audio_objects_and_device_discovery::{
    CoreAudioDevice, CoreAudioStreamDirection, capture_devices_default_first,
    duration_of_frames_in_ns, first_sample_timestamp_ns, interleaved_f32_description_of,
    osstatus_text, refusal_for_a_named_audio_device_that_is_not_attached,
};
use super::capture_stream::CoreAudioCaptureDelivery;
use super::hal_output_unit::CoreAudioStreamFormatBridge;
use super::input_cycle_render_and_capture_format_conversion::{
    ConsecutiveInputCycleRenderFailures, ConvertedCaptureBlockTimeline, ConvertedInputCycle,
    CoreAudioCaptureFormatConverter, InputCycleRenderVerdict, duration_of_many_frames_in_ns,
};
use super::playback_stream::{CoreAudioPlaybackDelivery, playback_stream_format_for};
use super::stream_device_binding_and_following::{
    CoreAudioDeviceFactsAfterAChange, CoreAudioStreamDeviceChange,
    CoreAudioStreamDeviceChangeResponse, CoreAudioStreamDevicePolicy,
    how_a_stream_responds_to_a_device_change,
};
use super::stream_unit_and_callback_context::CoreAudioDeliveryHoldingAHandOff;
use super::{CONSECUTIVE_FAILED_INPUT_RENDERS_BEFORE_THE_STREAM_ENDS, NO_ERR};

const A_DEVICE_AT_44_1_KHZ_IN_SIX_CHANNELS: AudioStreamFormat = AudioStreamFormat {
    sample_rate: 44_100,
    channels: 6,
    sample_format: AudioSampleFormat::F32,
};

#[test]
fn a_playback_stream_carries_the_requested_rate_and_channels_for_auhal_to_convert() {
    assert_eq!(
        playback_stream_format_for(
            RequestedAudioStreamFormat {
                sample_rate: Some(48_000),
                channels: Some(2),
                sample_format: Some(AudioSampleFormat::F32),
            },
            A_DEVICE_AT_44_1_KHZ_IN_SIX_CHANNELS,
        )
        .expect("a float request is carried"),
        AudioStreamFormat {
            sample_rate: 48_000,
            channels: 2,
            sample_format: AudioSampleFormat::F32,
        }
    );
}

#[test]
fn a_playback_stream_asked_for_nothing_carries_the_devices_own_format() {
    assert_eq!(
        playback_stream_format_for(
            RequestedAudioStreamFormat::default(),
            A_DEVICE_AT_44_1_KHZ_IN_SIX_CHANNELS
        )
        .expect("an empty request is carried"),
        A_DEVICE_AT_44_1_KHZ_IN_SIX_CHANNELS
    );
}

#[test]
fn a_playback_stream_asked_for_i16_samples_is_refused_by_name() {
    let refusal = playback_stream_format_for(
        RequestedAudioStreamFormat {
            sample_format: Some(AudioSampleFormat::I16),
            ..RequestedAudioStreamFormat::default()
        },
        A_DEVICE_AT_44_1_KHZ_IN_SIX_CHANNELS,
    )
    .expect_err("the unit's client side is float")
    .to_string();
    assert!(refusal.contains("I16"), "{refusal}");
}

#[test]
fn a_device_with_no_latency_stamps_its_first_sample_at_the_cycles_host_time() {
    assert_eq!(
        first_sample_timestamp_ns(5_000_000_000, 0, 48_000),
        5_000_000_000
    );
}

/// The device's latency is the distance between a sample arriving and the
/// cycle that carries it; a stamp that ignored it would sit that far late,
/// every block, forever.
#[test]
fn the_devices_latency_moves_the_stamp_back_by_that_many_frames() {
    assert_eq!(
        first_sample_timestamp_ns(5_000_000_000, 480, 48_000),
        5_000_000_000 - 10_000_000,
        "480 frames at 48 kHz is 10 ms"
    );
    assert_eq!(
        first_sample_timestamp_ns(5_000_000_000, 441, 44_100),
        5_000_000_000 - 10_000_000,
        "441 frames at 44.1 kHz is 10 ms"
    );
}

#[test]
fn the_capture_devices_are_listed_by_uid_the_default_first() {
    let a_device = |object_id: AudioObjectID, uid: &str, name: &str| CoreAudioDevice {
        object_id,
        uid: uid.into(),
        name: name.into(),
    };
    let listed = capture_devices_default_first(
        vec![
            a_device(41, "BuiltInMicrophoneDevice", "MacBook Pro Microphone"),
            a_device(42, "", "A device with no UID"),
            a_device(43, "AppleUSBAudioEngine:Mic", "USB Microphone"),
            a_device(44, "BlackHole2ch_UID", ""),
        ],
        Some(43),
    );
    let listed: Vec<(&str, &str)> = listed
        .iter()
        .map(|device| {
            assert_eq!(device.kind, AudioCaptureDeviceKind::Input);
            (device.id.as_str(), device.name.as_str())
        })
        .collect();
    assert_eq!(
        listed,
        [
            ("AppleUSBAudioEngine:Mic", "USB Microphone"),
            ("BuiltInMicrophoneDevice", "MacBook Pro Microphone"),
            ("BlackHole2ch_UID", "BlackHole2ch_UID"),
        ]
    );
}

#[test]
fn a_named_device_that_is_not_attached_is_refused_naming_it_and_the_ones_that_are() {
    let refusal = refusal_for_a_named_audio_device_that_is_not_attached(
        "NoSuchDeviceUID",
        CoreAudioStreamDirection::Capture,
        &[CoreAudioDevice {
            object_id: 42,
            uid: "BuiltInMicrophoneDevice".into(),
            name: "MacBook Pro Microphone".into(),
        }],
    );
    assert!(refusal.contains("'NoSuchDeviceUID'"), "{refusal}");
    assert!(
        refusal.contains("BuiltInMicrophoneDevice (MacBook Pro Microphone)"),
        "{refusal}"
    );
    assert!(refusal.contains("capture"), "{refusal}");
}

#[test]
fn a_named_device_with_nothing_attached_says_no_device_carries_that_direction() {
    let refusal = refusal_for_a_named_audio_device_that_is_not_attached(
        "NoSuchDeviceUID",
        CoreAudioStreamDirection::Playback,
        &[],
    );
    assert!(refusal.contains("'NoSuchDeviceUID'"), "{refusal}");
    assert!(
        refusal.contains("no device on this Mac carries playback"),
        "{refusal}"
    );
}

const A_FAILED_RENDER: i32 = -10863;

#[test]
fn a_rendered_cycle_is_handed_off() {
    let mut render_failures = ConsecutiveInputCycleRenderFailures::default();
    assert_eq!(
        render_failures.verdict_on(NO_ERR),
        InputCycleRenderVerdict::HandTheBlockOff
    );
}

/// The first failed render is the one worth a line; the ones behind it
/// until the bound would only repeat it at device cadence.
#[test]
fn the_first_failed_render_is_reported_and_the_rest_below_the_bound_are_not() {
    let mut render_failures = ConsecutiveInputCycleRenderFailures::default();
    assert_eq!(
        render_failures.verdict_on(A_FAILED_RENDER),
        InputCycleRenderVerdict::DropTheCycleAndReportTheFirstFailure
    );
    for _ in 2..CONSECUTIVE_FAILED_INPUT_RENDERS_BEFORE_THE_STREAM_ENDS {
        assert_eq!(
            render_failures.verdict_on(A_FAILED_RENDER),
            InputCycleRenderVerdict::DropTheCycleQuietly
        );
    }
}

/// Mental revert: return the status and nothing else, as the arm did, and
/// a device that stopped rendering leaves its stream's report clear
/// forever.
#[test]
fn failed_renders_reaching_the_bound_in_a_row_end_the_stream_once() {
    let mut render_failures = ConsecutiveInputCycleRenderFailures::default();
    let verdicts: Vec<InputCycleRenderVerdict> = (0
        ..CONSECUTIVE_FAILED_INPUT_RENDERS_BEFORE_THE_STREAM_ENDS + 5)
        .map(|_| render_failures.verdict_on(A_FAILED_RENDER))
        .collect();
    let endings: Vec<usize> = verdicts
        .iter()
        .enumerate()
        .filter(|(_, verdict)| matches!(verdict, InputCycleRenderVerdict::EndTheStream { .. }))
        .map(|(cycle, _)| cycle)
        .collect();
    assert_eq!(
        endings,
        [CONSECUTIVE_FAILED_INPUT_RENDERS_BEFORE_THE_STREAM_ENDS as usize - 1],
        "the stream ends on the bound-th failure in a row, and only then"
    );
    assert_eq!(
        verdicts[endings[0]],
        InputCycleRenderVerdict::EndTheStream {
            consecutive_failures: CONSECUTIVE_FAILED_INPUT_RENDERS_BEFORE_THE_STREAM_ENDS
        }
    );
}

/// A device that drops the odd cycle is not a device that stopped.
#[test]
fn a_rendered_cycle_resets_the_run_so_scattered_failures_never_end_the_stream() {
    let mut render_failures = ConsecutiveInputCycleRenderFailures::default();
    for _ in 0..4 {
        for _ in 1..CONSECUTIVE_FAILED_INPUT_RENDERS_BEFORE_THE_STREAM_ENDS {
            assert!(!matches!(
                render_failures.verdict_on(A_FAILED_RENDER),
                InputCycleRenderVerdict::EndTheStream { .. }
            ));
        }
        assert_eq!(
            render_failures.verdict_on(NO_ERR),
            InputCycleRenderVerdict::HandTheBlockOff
        );
    }
}

#[test]
fn a_later_run_of_failures_is_not_reported_again_before_it_ends_the_stream() {
    let mut render_failures = ConsecutiveInputCycleRenderFailures::default();
    render_failures.verdict_on(A_FAILED_RENDER);
    render_failures.verdict_on(NO_ERR);
    assert_eq!(
        render_failures.verdict_on(A_FAILED_RENDER),
        InputCycleRenderVerdict::DropTheCycleQuietly
    );
}

/// TN2091: the render callback belongs on the output element's input
/// scope, the input callback on global scope.
#[test]
fn each_callback_is_installed_on_the_scope_its_property_documents() {
    assert_eq!(
        CoreAudioPlaybackDelivery::CALLBACK_SCOPE,
        kAudioUnitScope_Input
    );
    assert_eq!(
        CoreAudioCaptureDelivery::CALLBACK_SCOPE,
        kAudioUnitScope_Global
    );
}

#[test]
fn a_status_that_is_a_four_character_code_is_spelled_out() {
    assert_eq!(
        osstatus_text(i32::from_be_bytes(*b"!dev")),
        format!("OSStatus {} ('!dev')", i32::from_be_bytes(*b"!dev"))
    );
    assert_eq!(osstatus_text(-10863), "OSStatus -10863");
}

/// Whatever rate and channel count the unit's client side carries, it is
/// described as the seam's interleaved float.
#[test]
fn the_units_client_format_is_the_streams_interleaved_float_format() {
    let description = interleaved_f32_description_of(AudioStreamFormat {
        sample_rate: 48_000,
        channels: 2,
        sample_format: AudioSampleFormat::F32,
    });
    assert_eq!(description.mSampleRate, 48_000.0);
    assert_eq!(description.mChannelsPerFrame, 2);
    assert_eq!(description.mBytesPerFrame, 8);
    assert_eq!(description.mBitsPerChannel, 32);
    assert_eq!(
        description.mFormatFlags & objc2_core_audio_types::kAudioFormatFlagIsNonInterleaved,
        0,
        "the seam carries interleaved payloads"
    );
}

fn interleaved_float(sample_rate: u32, channels: u32) -> AudioStreamFormat {
    AudioStreamFormat {
        sample_rate,
        channels,
        sample_format: AudioSampleFormat::F32,
    }
}

#[test]
fn a_capture_device_carrying_the_streams_format_renders_it_with_no_converter() {
    let stream_format = interleaved_float(48_000, 1);
    let format_bridge = CoreAudioStreamFormatBridge::between(
        CoreAudioStreamDirection::Capture,
        stream_format,
        interleaved_float(48_000, 1),
    );
    assert_eq!(
        format_bridge,
        CoreAudioStreamFormatBridge::TheDeviceCarriesTheStreamFormat
    );
    assert_eq!(
        format_bridge.unit_client_side_format(stream_format, stream_format),
        stream_format
    );
}

/// AUHAL input cannot resample, so a capture unit on a device of another
/// format renders the device's own and the arm converts. Mental revert:
/// keep the stream's format on the unit, and the unit is asked for a rate
/// its device does not run at.
#[test]
fn a_capture_device_at_another_rate_or_channel_count_renders_its_own_format_for_a_converter() {
    let stream_format = interleaved_float(48_000, 1);
    for device_own_format in [
        interleaved_float(24_000, 1),
        interleaved_float(16_000, 1),
        interleaved_float(48_000, 2),
        interleaved_float(96_000, 2),
    ] {
        let format_bridge = CoreAudioStreamFormatBridge::between(
            CoreAudioStreamDirection::Capture,
            stream_format,
            device_own_format,
        );
        assert_eq!(
            format_bridge,
            CoreAudioStreamFormatBridge::AnAudioConverterTakesTheDevicesFormatToTheStreams,
            "{device_own_format:?}"
        );
        assert_eq!(
            format_bridge.unit_client_side_format(stream_format, device_own_format),
            device_own_format
        );
    }
}

#[test]
fn a_playback_device_of_another_format_is_left_to_auhal_and_the_client_keeps_the_streams() {
    let stream_format = interleaved_float(48_000, 2);
    let device_own_format = interleaved_float(24_000, 1);
    let format_bridge = CoreAudioStreamFormatBridge::between(
        CoreAudioStreamDirection::Playback,
        stream_format,
        device_own_format,
    );
    assert_eq!(
        format_bridge,
        CoreAudioStreamFormatBridge::AuhalConvertsTheStreamFormatForTheDevice
    );
    assert_eq!(
        format_bridge.unit_client_side_format(stream_format, device_own_format),
        stream_format,
        "the stream's format is fixed for its lifetime"
    );
    assert_eq!(
        CoreAudioStreamFormatBridge::between(
            CoreAudioStreamDirection::Playback,
            stream_format,
            stream_format
        ),
        CoreAudioStreamFormatBridge::TheDeviceCarriesTheStreamFormat
    );
}

const THE_BOUND_DEVICE: AudioObjectID = 71;
const ANOTHER_DEVICE: AudioObjectID = 93;

fn device_facts(
    the_bound_device_is_alive: bool,
    the_bound_devices_own_format_changed: bool,
    system_default_device: Option<AudioObjectID>,
) -> CoreAudioDeviceFactsAfterAChange {
    CoreAudioDeviceFactsAfterAChange {
        the_bound_device_is_alive,
        the_bound_devices_own_format_changed,
        system_default_device,
    }
}

fn response_of_a_stream(
    device_policy: CoreAudioStreamDevicePolicy,
    change: CoreAudioStreamDeviceChange,
    facts: CoreAudioDeviceFactsAfterAChange,
) -> CoreAudioStreamDeviceChangeResponse {
    how_a_stream_responds_to_a_device_change(change, device_policy, THE_BOUND_DEVICE, facts)
}

const FOLLOWS_THE_DEFAULT: CoreAudioStreamDevicePolicy =
    CoreAudioStreamDevicePolicy::FollowsTheSystemDefault;
const NAMED: CoreAudioStreamDevicePolicy = CoreAudioStreamDevicePolicy::PinnedToTheNamedDevice;

#[test]
fn a_named_stream_whose_device_went_away_ends_rather_than_landing_elsewhere() {
    assert_eq!(
        response_of_a_stream(
            NAMED,
            CoreAudioStreamDeviceChange::BoundDeviceLivenessChanged,
            device_facts(false, false, Some(ANOTHER_DEVICE)),
        ),
        CoreAudioStreamDeviceChangeResponse::EndTheStreamBecauseTheNamedDeviceWentAway
    );
}

#[test]
fn a_named_stream_stays_on_its_device_when_the_default_moves() {
    assert_eq!(
        response_of_a_stream(
            NAMED,
            CoreAudioStreamDeviceChange::SystemDefaultDeviceMoved,
            device_facts(true, false, Some(ANOTHER_DEVICE)),
        ),
        CoreAudioStreamDeviceChangeResponse::StayOnTheBoundDevice
    );
}

/// Plugging in headphones or connecting AirPods moves the default; an
/// unnamed stream goes with it, as a macOS app does.
#[test]
fn an_unnamed_stream_moves_to_a_default_that_moved() {
    assert_eq!(
        response_of_a_stream(
            FOLLOWS_THE_DEFAULT,
            CoreAudioStreamDeviceChange::SystemDefaultDeviceMoved,
            device_facts(true, false, Some(ANOTHER_DEVICE)),
        ),
        CoreAudioStreamDeviceChangeResponse::MoveToTheSystemDefault(ANOTHER_DEVICE)
    );
}

/// Unplugging the headphones takes the device away and moves the default
/// back; whichever of the two reports arrives first, the stream follows.
#[test]
fn an_unnamed_stream_whose_device_went_away_moves_to_the_new_default() {
    assert_eq!(
        response_of_a_stream(
            FOLLOWS_THE_DEFAULT,
            CoreAudioStreamDeviceChange::BoundDeviceLivenessChanged,
            device_facts(false, false, Some(ANOTHER_DEVICE)),
        ),
        CoreAudioStreamDeviceChangeResponse::MoveToTheSystemDefault(ANOTHER_DEVICE)
    );
}

/// A device can go away a moment before the default moves off it; the
/// move is its own report, so ending the stream here would end it early.
#[test]
fn an_unnamed_stream_waits_for_the_default_to_move_off_a_device_that_went_away() {
    assert_eq!(
        response_of_a_stream(
            FOLLOWS_THE_DEFAULT,
            CoreAudioStreamDeviceChange::BoundDeviceLivenessChanged,
            device_facts(false, false, Some(THE_BOUND_DEVICE)),
        ),
        CoreAudioStreamDeviceChangeResponse::StayOnTheBoundDevice
    );
}

#[test]
fn an_unnamed_stream_whose_default_went_away_with_no_replacement_ends() {
    for change in [
        CoreAudioStreamDeviceChange::BoundDeviceLivenessChanged,
        CoreAudioStreamDeviceChange::SystemDefaultDeviceMoved,
    ] {
        assert_eq!(
            response_of_a_stream(
                FOLLOWS_THE_DEFAULT,
                change,
                device_facts(false, false, None)
            ),
            CoreAudioStreamDeviceChangeResponse::EndTheStreamBecauseNoDeviceIsTheDefault,
            "{change:?}"
        );
    }
}

#[test]
fn a_default_that_lands_on_the_bound_device_changes_nothing() {
    assert_eq!(
        response_of_a_stream(
            FOLLOWS_THE_DEFAULT,
            CoreAudioStreamDeviceChange::SystemDefaultDeviceMoved,
            device_facts(true, false, Some(THE_BOUND_DEVICE)),
        ),
        CoreAudioStreamDeviceChangeResponse::StayOnTheBoundDevice
    );
}

#[test]
fn a_liveness_report_from_a_device_still_alive_changes_nothing() {
    for device_policy in [NAMED, FOLLOWS_THE_DEFAULT] {
        assert_eq!(
            response_of_a_stream(
                device_policy,
                CoreAudioStreamDeviceChange::BoundDeviceLivenessChanged,
                device_facts(true, false, Some(ANOTHER_DEVICE)),
            ),
            CoreAudioStreamDeviceChangeResponse::StayOnTheBoundDevice
        );
    }
}

/// A 48 kHz input switched to 24 kHz — AirPods going to the headset
/// profile — keeps the stream's format by rebinding, named or not.
#[test]
fn a_bound_device_at_a_new_format_is_rebound_named_or_not() {
    for device_policy in [NAMED, FOLLOWS_THE_DEFAULT] {
        assert_eq!(
            response_of_a_stream(
                device_policy,
                CoreAudioStreamDeviceChange::BoundDeviceFormatChanged,
                device_facts(true, true, Some(ANOTHER_DEVICE)),
            ),
            CoreAudioStreamDeviceChangeResponse::RebindToTheBoundDeviceAtItsNewFormat
        );
    }
}

#[test]
fn a_format_report_that_changed_nothing_the_stream_bound_at_rebinds_nothing() {
    assert_eq!(
        response_of_a_stream(
            NAMED,
            CoreAudioStreamDeviceChange::BoundDeviceFormatChanged,
            device_facts(true, false, Some(THE_BOUND_DEVICE)),
        ),
        CoreAudioStreamDeviceChangeResponse::StayOnTheBoundDevice
    );
}

const FIRST_INPUT_SAMPLE_NS: i64 = 1_000_000_000_000;

/// The blocks a converted stream stamps from a device whose frames each
/// take `host_ns_per_device_frame` on the host clock, with the converter
/// keeping pace with its input as latency mode does. Each block is its
/// stamp, its frames, and the stream frames produced before it.
fn blocks_stamped_from_a_device(
    timeline: &mut ConvertedCaptureBlockTimeline,
    cycle_frames: u32,
    cycle_count: u64,
    host_ns_at_device_frame: impl Fn(u64) -> i64,
) -> Vec<(i64, u32, u64)> {
    let mut stream_frames_produced = 0u64;
    (0..cycle_count)
        .map(|cycle| {
            let first_device_frame = cycle * u64::from(cycle_frames);
            timeline.a_cycle_is_lent(host_ns_at_device_frame(first_device_frame), cycle_frames);
            let stream_frames_producible = (first_device_frame + u64::from(cycle_frames))
                * u64::from(timeline.stream_sample_rate)
                / u64::from(timeline.device_sample_rate);
            let converted_frame_count = (stream_frames_producible - stream_frames_produced) as u32;
            let produced_before = stream_frames_produced;
            stream_frames_produced = stream_frames_producible;
            (
                timeline.stamp_the_next_block(converted_frame_count),
                converted_frame_count,
                produced_before,
            )
        })
        .collect()
}

fn a_steady_device_at(device_sample_rate: u32) -> impl Fn(u64) -> i64 {
    move |device_frame| {
        FIRST_INPUT_SAMPLE_NS
            + duration_of_many_frames_in_ns(device_frame, device_sample_rate) as i64
    }
}

#[test]
fn a_converted_streams_first_block_is_stamped_at_its_first_input_sample_less_the_latency() {
    let mut timeline = ConvertedCaptureBlockTimeline::for_a_new_binding(44_100, 48_000, 16);
    timeline.a_cycle_is_lent(FIRST_INPUT_SAMPLE_NS, 512);
    assert_eq!(
        timeline.stamp_the_next_block(557),
        FIRST_INPUT_SAMPLE_NS - 362_811,
        "16 frames at 44.1 kHz is 362.8 µs"
    );
}

/// On a steady device the stamps are exactly the first cycle's, advanced
/// by the stream frames produced, less the converter's latency — and so
/// back to back.
#[test]
fn a_steady_devices_converted_blocks_are_stamped_back_to_back_from_the_first_cycle() {
    let mut timeline = ConvertedCaptureBlockTimeline::for_a_new_binding(44_100, 48_000, 16);
    let blocks =
        blocks_stamped_from_a_device(&mut timeline, 512, 2_000, a_steady_device_at(44_100));
    let latency_ns = duration_of_frames_in_ns(16, 44_100);
    for &(stamp_ns, _, produced_before) in &blocks {
        let from_the_first_cycle_ns = FIRST_INPUT_SAMPLE_NS
            + duration_of_many_frames_in_ns(produced_before, 48_000) as i64
            - latency_ns;
        assert!(
            (stamp_ns - from_the_first_cycle_ns).abs() <= 2,
            "{stamp_ns} vs {from_the_first_cycle_ns}"
        );
    }
    for pair in blocks.windows(2) {
        let (earlier_stamp_ns, earlier_frames, _) = pair[0];
        let (later_stamp_ns, _, _) = pair[1];
        let gap_ns =
            later_stamp_ns - (earlier_stamp_ns + duration_of_frames_in_ns(earlier_frames, 48_000));
        assert!(gap_ns.abs() <= 3, "blocks overlap or part by {gap_ns} ns");
    }
}

/// Mental revert: anchor once and advance at the nominal rate, and after
/// ten minutes on a device 100 ppm fast the stamps sit 60 ms off the host
/// clock.
#[test]
fn a_device_clock_off_nominal_never_pulls_the_stamps_off_the_host_clock() {
    let host_ns_per_device_frame = 1e9 / 44_100.0 * (1.0 - 100e-6);
    let host_ns_at_device_frame = move |device_frame: u64| {
        FIRST_INPUT_SAMPLE_NS + (device_frame as f64 * host_ns_per_device_frame).round() as i64
    };
    let mut timeline = ConvertedCaptureBlockTimeline::for_a_new_binding(44_100, 48_000, 16);
    let ten_minutes_of_cycles = 600 * 44_100 / 512;
    let blocks = blocks_stamped_from_a_device(
        &mut timeline,
        512,
        ten_minutes_of_cycles,
        host_ns_at_device_frame,
    );
    let &(last_stamp_ns, _, produced_before) = blocks.last().expect("blocks were stamped");
    let input_frame_the_block_begins_at = produced_before as f64 * 44_100.0 / 48_000.0 - 16.0;
    let where_that_frame_is_on_the_host_clock_ns = FIRST_INPUT_SAMPLE_NS
        + (input_frame_the_block_begins_at * host_ns_per_device_frame).round() as i64;
    assert!(
        (last_stamp_ns - where_that_frame_is_on_the_host_clock_ns).abs() < 1_000,
        "stamped {last_stamp_ns}, captured at {where_that_frame_is_on_the_host_clock_ns}"
    );
}

/// A stop and restart, or cycles the device dropped, leave a real gap in
/// the input; the stamps show it rather than running on as if none
/// happened.
#[test]
fn a_gap_in_the_input_is_the_same_gap_in_the_stamps() {
    const GAP_NS: i64 = 250_000_000;
    let steady = a_steady_device_at(48_000);
    let after_a_gap_at_frame = 100 * 512;
    let mut timeline = ConvertedCaptureBlockTimeline::for_a_new_binding(48_000, 16_000, 18);
    let blocks = blocks_stamped_from_a_device(&mut timeline, 512, 200, move |device_frame| {
        steady(device_frame)
            + if device_frame >= after_a_gap_at_frame {
                GAP_NS
            } else {
                0
            }
    });
    let gaps_ns: Vec<i64> = blocks
        .windows(2)
        .map(|pair| pair[1].0 - (pair[0].0 + duration_of_frames_in_ns(pair[0].1, 16_000)))
        .collect();
    assert!((gaps_ns[99] - GAP_NS).abs() <= 3, "{}", gaps_ns[99]);
    assert!(
        gaps_ns
            .iter()
            .enumerate()
            .all(|(index, gap_ns)| index == 99 || gap_ns.abs() <= 3)
    );
}

/// Mental revert: count in `i64` nanoseconds, and a month of 192 kHz
/// frames overflows it.
#[test]
fn a_month_long_converted_stream_still_stamps_exactly() {
    let a_month_of_device_frames = 30 * 24 * 3_600 * 192_000u64;
    let mut timeline = ConvertedCaptureBlockTimeline::for_a_new_binding(192_000, 48_000, 16);
    timeline.device_frames_lent = a_month_of_device_frames;
    timeline.stream_frames_produced = a_month_of_device_frames / 4;
    timeline.a_cycle_is_lent(FIRST_INPUT_SAMPLE_NS, 4_096);
    assert_eq!(
        timeline.stamp_the_next_block(1_024),
        FIRST_INPUT_SAMPLE_NS - duration_of_frames_in_ns(16, 192_000)
    );
}

fn no_device() -> CoreAudioDevice {
    CoreAudioDevice {
        object_id: kAudioObjectUnknown,
        uid: "NoDeviceAConverterTestNeeds".into(),
        name: "no device".into(),
    }
}

fn interleaved_f32_bytes_of(samples: &[f32]) -> Vec<u8> {
    samples
        .iter()
        .flat_map(|sample| sample.to_le_bytes())
        .collect()
}

fn interleaved_f32_samples_of(bytes: &[u8]) -> Vec<f32> {
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|scalar| f32::from_le_bytes(*scalar))
        .collect()
}

/// Each converted block's stamp and its interleaved samples, from cycles
/// of `cycle_frames` lent on a steady device, carrying one full-scale
/// impulse at `impulse_at_input_frame` in every channel.
fn blocks_converted_from_an_impulse(
    device_own_format: AudioStreamFormat,
    stream_format: AudioStreamFormat,
    cycle_frames: u32,
    cycle_count: u32,
    impulse_at_input_frame: usize,
) -> Vec<(i64, Vec<f32>)> {
    let mut converter = CoreAudioCaptureFormatConverter::new(
        &no_device(),
        device_own_format,
        stream_format,
        cycle_frames,
    )
    .expect("AudioToolbox converts between two linear PCM float formats");
    let device_channels = device_own_format.channels as usize;
    let mut input = vec![0.0f32; (cycle_frames * cycle_count) as usize * device_channels];
    input[impulse_at_input_frame * device_channels..][..device_channels].fill(1.0);
    let blocks = std::cell::RefCell::new(Vec::new());
    for cycle in 0..cycle_count {
        let first_device_frame = u64::from(cycle * cycle_frames);
        let mut cycle_bytes = interleaved_f32_bytes_of(
            &input[first_device_frame as usize * device_channels..]
                [..cycle_frames as usize * device_channels],
        );
        let outcome = converter.convert_the_cycle(
            &mut cycle_bytes,
            cycle_frames,
            a_steady_device_at(device_own_format.sample_rate)(first_device_frame),
            &|block: CapturedAudioBlockFromDevice<'_>| {
                assert_eq!(
                    block.interleaved_sample_bytes.len(),
                    stream_format.interleaved_byte_count_for(block.sample_count)
                );
                blocks.borrow_mut().push((
                    block.first_sample_timestamp_ns,
                    interleaved_f32_samples_of(block.interleaved_sample_bytes),
                ));
            },
        );
        assert_eq!(outcome, ConvertedInputCycle::HandedOff);
    }
    blocks.into_inner()
}

/// The loudest output frame's stamp, and every channel's value there.
fn where_the_impulse_came_out(
    blocks: &[(i64, Vec<f32>)],
    stream_format: AudioStreamFormat,
) -> (i64, Vec<f32>) {
    let channels = stream_format.channels as usize;
    blocks
        .iter()
        .flat_map(|(stamp_ns, samples)| {
            samples
                .chunks_exact(channels)
                .enumerate()
                .map(move |(frame, frame_samples)| {
                    (
                        stamp_ns
                            + duration_of_frames_in_ns(frame as u32, stream_format.sample_rate),
                        frame_samples.to_vec(),
                    )
                })
        })
        .max_by(|(_, left), (_, right)| left[0].abs().total_cmp(&right[0].abs()))
        .expect("the converter produced frames")
}

/// The whole converter path without a device: an impulse lent at a known
/// instant comes out stamped at that instant, to within one output frame.
/// Mental revert: subtract no latency, and the 44.1 → 48 kHz impulse is
/// stamped 16 input frames — 363 µs — late.
#[test]
fn an_impulse_through_the_capture_converter_is_stamped_where_it_was_captured() {
    const IMPULSE_AT_INPUT_FRAME: usize = 3_000;
    for (device_own_format, stream_format) in [
        (interleaved_float(44_100, 1), interleaved_float(48_000, 2)),
        (interleaved_float(48_000, 2), interleaved_float(16_000, 1)),
        (interleaved_float(16_000, 1), interleaved_float(48_000, 1)),
        (interleaved_float(96_000, 2), interleaved_float(48_000, 2)),
    ] {
        let blocks = blocks_converted_from_an_impulse(
            device_own_format,
            stream_format,
            512,
            12,
            IMPULSE_AT_INPUT_FRAME,
        );
        let (impulse_stamp_ns, impulse_frame) = where_the_impulse_came_out(&blocks, stream_format);
        let impulse_captured_at_ns = FIRST_INPUT_SAMPLE_NS
            + duration_of_frames_in_ns(
                IMPULSE_AT_INPUT_FRAME as u32,
                device_own_format.sample_rate,
            );
        let one_output_frame_ns = duration_of_frames_in_ns(1, stream_format.sample_rate);
        assert!(
            (impulse_stamp_ns - impulse_captured_at_ns).abs() <= one_output_frame_ns,
            "{device_own_format:?} → {stream_format:?}: stamped {impulse_stamp_ns}, captured \
             at {impulse_captured_at_ns}"
        );
        assert!(
            impulse_frame.iter().all(|&sample| sample > 0.2),
            "every channel carries the impulse: {impulse_frame:?}"
        );
    }
}

#[test]
fn a_converter_that_only_moves_channels_adds_no_latency_and_copies_mono_to_both() {
    let stream_format = interleaved_float(48_000, 2);
    let blocks =
        blocks_converted_from_an_impulse(interleaved_float(48_000, 1), stream_format, 512, 4, 700);
    assert_eq!(blocks[0].0, FIRST_INPUT_SAMPLE_NS);
    let (impulse_stamp_ns, impulse_frame) = where_the_impulse_came_out(&blocks, stream_format);
    let impulse_captured_at_ns = FIRST_INPUT_SAMPLE_NS + duration_of_frames_in_ns(700, 48_000);
    assert!(
        (impulse_stamp_ns - impulse_captured_at_ns).abs() <= 1,
        "stamped {impulse_stamp_ns}, captured at {impulse_captured_at_ns}"
    );
    assert_eq!(impulse_frame, [1.0, 1.0]);
}
