// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The audio capture and playback streams and the virtual camera a module opens
//! through the table, on whichever arm this machine probes.

use std::time::Duration;

use tatolab_module_sdk::module_host_table_abi::ModuleHostTableVirtualCameraStreamHandle;

use super::*;

/// A host over a node that lent nothing: the audio verbs need no port.
pub(super) fn module_host_lending_no_port(label: &str) -> ModuleHost {
    Python::initialize();
    Python::attach(|python| {
        let (source, _destination) = one_wired_link(python, label);
        let (context, _) = full_access_context_recording_escalates(python, source, "microphone");
        module_host_of(&open_module_host_table(&context, &[], &[]).unwrap())
    })
}

/// Whichever arm this machine probes — a device, or the null backend's
/// silence — delivers whole, device-stamped blocks through the module's
/// callback on its own thread, and none once delivery stops.
#[test]
fn a_module_captures_blocks_through_the_engines_audio_device_backend() {
    let module_host = module_host_lending_no_port("audio_capture");
    let mut capture_stream = module_host
        .open_audio_capture_stream(
            &tatolab_module_sdk::ModuleHostAudioCaptureStreamRequest::default(),
        )
        .expect("the default device, or silence, opens");
    let stream_format = capture_stream.stream_format();
    let (block_sender, block_receiver) = std::sync::mpsc::sync_channel(64);
    capture_stream
        .start_delivering_to(Box::new(move |captured_block| {
            let _ = block_sender.try_send((
                captured_block.interleaved_sample_bytes.len(),
                captured_block.sample_count,
                captured_block.first_sample_timestamp_ns,
            ));
        }))
        .unwrap();

    let mut previous_first_sample_timestamp_ns = i64::MIN;
    for _ in 0..3 {
        let (byte_count, sample_count, first_sample_timestamp_ns) = block_receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("a block arrives within five seconds");
        assert!(sample_count > 0);
        assert_eq!(
            byte_count,
            sample_count as usize
                * stream_format.channels as usize
                * stream_format.sample_format.bytes_per_sample()
        );
        assert!(first_sample_timestamp_ns > previous_first_sample_timestamp_ns);
        previous_first_sample_timestamp_ns = first_sample_timestamp_ns;
    }
    assert_eq!(
        capture_stream.failure_that_ended_the_stream().unwrap(),
        None
    );

    capture_stream.stop_delivering().unwrap();
    while block_receiver.try_recv().is_ok() {}
    std::thread::sleep(Duration::from_millis(100));
    assert!(
        block_receiver.try_recv().is_err(),
        "no block is delivered once delivery stops"
    );
}

#[test]
fn a_device_no_backend_has_is_refused_by_name() {
    let module_host = module_host_lending_no_port("audio_unknown_device");

    let refusal = module_host
        .open_audio_capture_stream(&tatolab_module_sdk::ModuleHostAudioCaptureStreamRequest {
            device_id: Some("tatolab-no-such-audio-device"),
            ..Default::default()
        })
        .unwrap_err()
        .to_string();

    assert!(
        refusal.starts_with("audio_capture_stream_open: "),
        "{refusal}"
    );
    assert!(
        refusal.contains("tatolab-no-such-audio-device"),
        "{refusal}"
    );
}

#[test]
fn a_format_the_device_does_not_open_in_is_refused_never_converted() {
    let module_host = module_host_lending_no_port("audio_format");

    let refusal = module_host
        .open_audio_capture_stream(&tatolab_module_sdk::ModuleHostAudioCaptureStreamRequest {
            channels: Some(31),
            ..Default::default()
        })
        .unwrap_err()
        .to_string();

    assert!(
        refusal.contains("31 channel(s) were requested"),
        "{refusal}"
    );
}

#[test]
fn a_closed_capture_streams_handle_names_nothing() {
    let audio_capture_streams = audio_capture_streams::AudioCaptureStreamsOpenedForOneNode::under(
        CaptureDeviceAccessPrompting::RefuseWhatWasNeverAskedFor,
    );
    let (handle, _stream_format) = audio_capture_streams
        .open("microphone", None, Default::default())
        .unwrap();
    assert!(audio_capture_streams.opened_stream(handle).is_ok());

    audio_capture_streams.close(handle);

    let refusal = audio_capture_streams
        .opened_stream(handle)
        .err()
        .expect("a closed stream's handle is refused");
    assert!(
        refusal.contains("names no capture stream this table has open"),
        "{refusal}"
    );
    let (next_handle, _stream_format) = audio_capture_streams
        .open("microphone", None, Default::default())
        .unwrap();
    assert_ne!(next_handle, handle, "a handle is never reused");
}

/// The format the playback tests ask for: what the speaker module plays.
const PLAYBACK_REQUEST_AT_48_KHZ_STEREO_F32:
    tatolab_module_sdk::ModuleHostAudioPlaybackStreamRequest<'static> =
    tatolab_module_sdk::ModuleHostAudioPlaybackStreamRequest {
        device_id: None,
        sample_rate: Some(48_000),
        channels: Some(2),
        sample_format: Some(tatolab_module_sdk::ModuleHostAudioSampleFormat::F32),
    };

/// Whichever arm this machine probes opens the stream in the format the module
/// asked for, and asks the module's callback for whole device buffers in it on
/// its own thread, and for none once requesting stops.
#[test]
fn a_module_plays_through_the_engines_audio_device_backend_in_the_format_it_asked_for() {
    let module_host = module_host_lending_no_port("audio_playback");
    let mut playback_stream = module_host
        .open_audio_playback_stream(&PLAYBACK_REQUEST_AT_48_KHZ_STEREO_F32)
        .expect("the default device, or the null arm, opens at 48 kHz stereo f32");
    assert_eq!(
        playback_stream.stream_format(),
        tatolab_module_sdk::ModuleHostAudioStreamFormat {
            sample_rate: 48_000,
            channels: 2,
            sample_format: tatolab_module_sdk::ModuleHostAudioSampleFormat::F32,
        }
    );
    let (request_sender, request_receiver) = std::sync::mpsc::sync_channel(64);
    playback_stream
        .start_requesting_from(Box::new(move |requested_block| {
            requested_block.interleaved_sample_bytes_to_fill.fill(0);
            let _ = request_sender.try_send((
                requested_block.interleaved_sample_bytes_to_fill.len(),
                requested_block.sample_count,
            ));
        }))
        .unwrap();

    for _ in 0..3 {
        let (byte_count, sample_count) = request_receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("the device asks for samples within five seconds");
        assert!(sample_count > 0);
        assert_eq!(byte_count, sample_count as usize * 2 * 4);
    }
    assert_eq!(
        playback_stream.failure_that_ended_the_stream().unwrap(),
        None
    );

    playback_stream.stop_requesting().unwrap();
    while request_receiver.try_recv().is_ok() {}
    std::thread::sleep(Duration::from_millis(100));
    assert!(
        request_receiver.try_recv().is_err(),
        "no buffer is asked for once requesting stops"
    );
}

#[test]
fn a_playback_device_no_backend_has_is_refused_by_name() {
    let module_host = module_host_lending_no_port("audio_playback_unknown_device");

    let refusal = module_host
        .open_audio_playback_stream(&tatolab_module_sdk::ModuleHostAudioPlaybackStreamRequest {
            device_id: Some("tatolab-no-such-audio-device"),
            ..PLAYBACK_REQUEST_AT_48_KHZ_STEREO_F32
        })
        .unwrap_err()
        .to_string();

    assert!(
        refusal.starts_with("audio_playback_stream_open: "),
        "{refusal}"
    );
    assert!(
        refusal.contains("tatolab-no-such-audio-device"),
        "{refusal}"
    );
}

#[test]
fn a_closed_playback_streams_handle_names_nothing_and_is_never_reused() {
    let audio_playback_streams =
        audio_playback_streams::AudioPlaybackStreamsOpenedForOneNode::default();
    let (handle, _stream_format, _device_period) = audio_playback_streams
        .open("speaker", None, Default::default())
        .unwrap();
    assert!(audio_playback_streams.opened_stream(handle).is_ok());

    audio_playback_streams.close(handle);

    let refusal = audio_playback_streams
        .opened_stream(handle)
        .err()
        .expect("a closed stream's handle is refused");
    assert!(
        refusal.contains("names no playback stream this table has open"),
        "{refusal}"
    );
    let (next_handle, _stream_format, _device_period) = audio_playback_streams
        .open("speaker", None, Default::default())
        .unwrap();
    assert_ne!(next_handle, handle, "a handle is never reused");
}

/// With a PipeWire daemon the camera node registers and offers an extent;
/// without one — the CI container — the open is refused naming the door and
/// why, never a bare failure.
#[test]
fn a_virtual_camera_opens_where_pipewire_answers_and_is_refused_by_name_elsewhere() {
    let module_host = module_host_lending_no_port("virtual_camera");

    match module_host.open_virtual_camera_stream("tatolab test camera") {
        Ok(camera) => {
            camera
                .offer_extent(64, 48, None)
                .expect("a PipeWire camera takes an even extent");
            let presentation = camera
                .write_yuyv_frame(&[0x80; 64 * 2 * 48], 64 * 2, 1)
                .expect("a frame of the offered extent is taken");
            assert_ne!(
                presentation,
                tatolab_module_sdk::ModuleHostVirtualCameraFramePresentation::EveryBufferIsHeldByTheConsumer,
                "with nothing watching, no buffer is held"
            );
        }
        Err(refusal) => {
            let refusal = refusal.to_string();
            assert!(
                refusal.starts_with("virtual_camera_stream_open: "),
                "{refusal}"
            );
            assert!(refusal.contains("PipeWire camera door"), "{refusal}");
        }
    }
}

#[test]
fn a_virtual_camera_handle_no_open_returned_is_refused() {
    let virtual_camera_streams =
        virtual_camera_streams::VirtualCameraStreamsOpenedForOneNode::default();

    let refusal = virtual_camera_streams
        .opened_camera(ModuleHostTableVirtualCameraStreamHandle { value: 3 })
        .err()
        .expect("a handle no open returned is refused");

    assert!(
        refusal.contains("names no camera this table has open"),
        "{refusal}"
    );
}

/// Mental-revert: open a device again beside the stream this node has open on it, and the first
/// stream keeps delivering, two holders of one device.
#[test]
fn a_capture_opened_again_on_a_device_takes_it_over_from_the_stream_that_had_it() {
    let module_host = module_host_lending_no_port("audio_capture_taken_over");
    let mut first = module_host
        .open_audio_capture_stream(
            &tatolab_module_sdk::ModuleHostAudioCaptureStreamRequest::default(),
        )
        .expect("the default device, or silence, opens");
    let (first_sender, first_blocks) = std::sync::mpsc::sync_channel(64);
    first
        .start_delivering_to(Box::new(move |_| {
            let _ = first_sender.try_send(());
        }))
        .unwrap();
    first_blocks
        .recv_timeout(Duration::from_secs(5))
        .expect("the first stream delivers");

    let mut second = module_host
        .open_audio_capture_stream(
            &tatolab_module_sdk::ModuleHostAudioCaptureStreamRequest::default(),
        )
        .expect("the device opens again, taken over");

    while first_blocks.try_recv().is_ok() {}
    std::thread::sleep(Duration::from_millis(100));
    assert!(
        first_blocks.try_recv().is_err(),
        "the stream taken over delivers nothing once the device is opened again"
    );
    let refusal = first
        .start_delivering_to(Box::new(|_| {}))
        .unwrap_err()
        .to_string();
    assert!(refusal.contains("taken over by a later open"), "{refusal}");
    first
        .stop_delivering()
        .expect("a stream taken over is stopped already");
    let (second_sender, second_blocks) = std::sync::mpsc::sync_channel(64);
    second
        .start_delivering_to(Box::new(move |_| {
            let _ = second_sender.try_send(());
        }))
        .unwrap();
    second_blocks
        .recv_timeout(Duration::from_secs(5))
        .expect("the stream that took the device over delivers");
}

#[test]
fn a_playback_opened_again_on_a_device_takes_it_over_from_the_stream_that_had_it() {
    let module_host = module_host_lending_no_port("audio_playback_taken_over");
    let request = tatolab_module_sdk::ModuleHostAudioPlaybackStreamRequest {
        sample_rate: Some(48_000),
        channels: Some(2),
        sample_format: Some(tatolab_module_sdk::ModuleHostAudioSampleFormat::F32),
        ..Default::default()
    };
    let mut first = module_host
        .open_audio_playback_stream(&request)
        .expect("the default device, or the null backend, opens");

    let mut second = module_host
        .open_audio_playback_stream(&request)
        .expect("the device opens again, taken over");

    let refusal = first
        .start_requesting_from(Box::new(|_| {}))
        .unwrap_err()
        .to_string();
    assert!(refusal.contains("taken over by a later open"), "{refusal}");
    first
        .stop_requesting()
        .expect("a stream taken over is stopped already");
    second
        .start_requesting_from(Box::new(|_| {}))
        .expect("the stream that took the device over starts");
}
