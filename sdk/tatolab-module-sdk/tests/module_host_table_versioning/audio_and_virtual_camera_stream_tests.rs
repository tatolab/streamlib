// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The audio capture and playback streams and the virtual camera a module
//! opens through a table it loaded.

use super::*;

#[test]
fn a_capture_stream_carries_the_request_and_reports_the_format_it_opened_in() {
    let node_host = stood_up_node_host();
    let table = v1_12_table_over(node_host);
    let module_host = unsafe { ModuleHost::from_module_host_table_pointer(&table) }.unwrap();

    let capture_stream = module_host
        .open_audio_capture_stream(&ModuleHostAudioCaptureStreamRequest {
            device_id: Some("hw:1"),
            sample_rate: Some(48_000),
            channels: None,
            sample_format: Some(ModuleHostAudioSampleFormat::F32),
        })
        .expect("the stood-up host opens every device but `unplugged`");

    assert_eq!(
        capture_stream.stream_format(),
        ModuleHostAudioStreamFormat {
            sample_rate: 48_000,
            channels: 2,
            sample_format: ModuleHostAudioSampleFormat::I16,
        }
    );
    let audio_capture = node_host.audio_capture.lock().unwrap();
    assert_eq!(audio_capture.requested_device_id.as_deref(), Some("hw:1"));
    assert_eq!(
        audio_capture.requested_format,
        Some((48_000, 0, MODULE_HOST_TABLE_AUDIO_SAMPLE_FORMAT_F32))
    );
}

#[test]
fn a_capture_request_naming_nothing_asks_for_the_default_device_in_its_own_format() {
    let node_host = stood_up_node_host();
    let table = v1_12_table_over(node_host);
    let module_host = unsafe { ModuleHost::from_module_host_table_pointer(&table) }.unwrap();

    let _capture_stream = module_host
        .open_audio_capture_stream(&ModuleHostAudioCaptureStreamRequest::default())
        .unwrap();

    let audio_capture = node_host.audio_capture.lock().unwrap();
    assert_eq!(audio_capture.requested_device_id.as_deref(), Some(""));
    assert_eq!(
        audio_capture.requested_format,
        Some((0, 0, MODULE_HOST_TABLE_AUDIO_SAMPLE_FORMAT_ANY))
    );
}

#[test]
fn a_capture_stream_the_host_refuses_carries_its_message() {
    let node_host = stood_up_node_host();
    let table = v1_12_table_over(node_host);
    let module_host = unsafe { ModuleHost::from_module_host_table_pointer(&table) }.unwrap();

    let refusal = module_host
        .open_audio_capture_stream(&ModuleHostAudioCaptureStreamRequest {
            device_id: Some("unplugged"),
            ..ModuleHostAudioCaptureStreamRequest::default()
        })
        .unwrap_err();

    assert_eq!(
        refusal.to_string(),
        "audio_capture_stream_open: audio device 'unplugged' is not attached"
    );
}

#[test]
fn captured_blocks_reach_the_hand_off_until_delivery_stops() {
    let node_host = stood_up_node_host();
    let table = v1_12_table_over(node_host);
    let module_host = unsafe { ModuleHost::from_module_host_table_pointer(&table) }.unwrap();
    let mut capture_stream = module_host
        .open_audio_capture_stream(&ModuleHostAudioCaptureStreamRequest::default())
        .unwrap();
    let handed_off = std::sync::Arc::new(Mutex::new(Vec::new()));
    let handed_off_in_the_hand_off = std::sync::Arc::clone(&handed_off);

    capture_stream
        .start_delivering_to(Box::new(move |captured_block| {
            handed_off_in_the_hand_off.lock().unwrap().push((
                captured_block.interleaved_sample_bytes.to_vec(),
                captured_block.sample_count,
                captured_block.first_sample_timestamp_ns,
            ));
        }))
        .unwrap();
    assert!(deliver_one_block_through_the_installed_callback(
        node_host,
        &[1, 2, 3, 4],
        1,
        77
    ));
    capture_stream.stop_delivering().unwrap();

    assert!(!deliver_one_block_through_the_installed_callback(
        node_host,
        &[5, 6, 7, 8],
        1,
        78
    ));
    assert_eq!(*handed_off.lock().unwrap(), vec![(vec![1, 2, 3, 4], 1, 77)]);
}

#[test]
fn a_capture_stream_reports_the_failure_that_ended_it() {
    let node_host = stood_up_node_host();
    let table = v1_12_table_over(node_host);
    let module_host = unsafe { ModuleHost::from_module_host_table_pointer(&table) }.unwrap();
    let capture_stream = module_host
        .open_audio_capture_stream(&ModuleHostAudioCaptureStreamRequest::default())
        .unwrap();
    assert_eq!(
        capture_stream.failure_that_ended_the_stream().unwrap(),
        None
    );

    node_host.audio_capture.lock().unwrap().ended_with = Some("the device was unplugged".into());

    assert_eq!(
        capture_stream.failure_that_ended_the_stream().unwrap(),
        Some("the device was unplugged".to_string())
    );
}

#[test]
fn a_dropped_capture_stream_is_closed_and_releases_its_reference() {
    let node_host = stood_up_node_host();
    let table = v1_12_table_over(node_host);
    let module_host = unsafe { ModuleHost::from_module_host_table_pointer(&table) }.unwrap();
    let mut capture_stream = module_host
        .open_audio_capture_stream(&ModuleHostAudioCaptureStreamRequest::default())
        .unwrap();
    capture_stream
        .start_delivering_to(Box::new(|_captured_block| {}))
        .unwrap();
    assert_eq!(node_host.references.load(Ordering::SeqCst), 2);

    drop(capture_stream);

    let audio_capture = node_host.audio_capture.lock().unwrap();
    assert!(audio_capture.closed);
    assert!(audio_capture.installed_callback.is_none());
    drop(audio_capture);
    assert_eq!(node_host.references.load(Ordering::SeqCst), 1);
}

#[test]
fn a_playback_stream_carries_the_request_and_reports_the_format_and_period_it_opened_with() {
    let node_host = stood_up_node_host();
    let table = v1_12_table_over(node_host);
    let module_host = unsafe { ModuleHost::from_module_host_table_pointer(&table) }.unwrap();

    let playback_stream = module_host
        .open_audio_playback_stream(&ModuleHostAudioPlaybackStreamRequest {
            device_id: Some("hw:1"),
            sample_rate: Some(48_000),
            channels: Some(2),
            sample_format: Some(ModuleHostAudioSampleFormat::I16),
        })
        .expect("the stood-up host opens every device but `unplugged`");

    assert_eq!(
        playback_stream.stream_format(),
        ModuleHostAudioStreamFormat {
            sample_rate: 48_000,
            channels: 2,
            sample_format: ModuleHostAudioSampleFormat::I16,
        }
    );
    assert_eq!(
        playback_stream.device_period_in_per_channel_samples(),
        Some(256)
    );
    let audio_playback = node_host.audio_playback.lock().unwrap();
    assert_eq!(audio_playback.requested_device_id.as_deref(), Some("hw:1"));
    assert_eq!(
        audio_playback.requested_format,
        Some((48_000, 2, MODULE_HOST_TABLE_AUDIO_SAMPLE_FORMAT_I16))
    );
}

#[test]
fn a_playback_stream_whose_host_reports_no_period_reports_none() {
    let node_host = stood_up_node_host();
    let table = v1_12_table_over(node_host);
    let module_host = unsafe { ModuleHost::from_module_host_table_pointer(&table) }.unwrap();

    let playback_stream = module_host
        .open_audio_playback_stream(&ModuleHostAudioPlaybackStreamRequest::default())
        .unwrap();

    assert_eq!(playback_stream.device_period_in_per_channel_samples(), None);
    assert_eq!(
        node_host.audio_playback.lock().unwrap().requested_format,
        Some((0, 0, MODULE_HOST_TABLE_AUDIO_SAMPLE_FORMAT_ANY))
    );
}

#[test]
fn a_playback_stream_the_host_refuses_carries_its_message() {
    let node_host = stood_up_node_host();
    let table = v1_12_table_over(node_host);
    let module_host = unsafe { ModuleHost::from_module_host_table_pointer(&table) }.unwrap();

    let refusal = module_host
        .open_audio_playback_stream(&ModuleHostAudioPlaybackStreamRequest {
            device_id: Some("unplugged"),
            ..ModuleHostAudioPlaybackStreamRequest::default()
        })
        .unwrap_err();

    assert_eq!(
        refusal.to_string(),
        "audio_playback_stream_open: audio device 'unplugged' is not attached"
    );
}

#[test]
fn the_hand_off_fills_the_devices_own_buffer_until_requesting_stops() {
    let node_host = stood_up_node_host();
    let table = v1_12_table_over(node_host);
    let module_host = unsafe { ModuleHost::from_module_host_table_pointer(&table) }.unwrap();
    let mut playback_stream = module_host
        .open_audio_playback_stream(&ModuleHostAudioPlaybackStreamRequest::default())
        .unwrap();
    let asked_sample_counts = std::sync::Arc::new(Mutex::new(Vec::new()));
    let asked_sample_counts_in_the_hand_off = std::sync::Arc::clone(&asked_sample_counts);

    playback_stream
        .start_requesting_from(Box::new(move |requested_block| {
            requested_block.interleaved_sample_bytes_to_fill.fill(0x11);
            asked_sample_counts_in_the_hand_off
                .lock()
                .unwrap()
                .push(requested_block.sample_count);
        }))
        .unwrap();
    assert_eq!(
        ask_for_one_block_through_the_installed_callback(node_host, 8, 2),
        Some(vec![0x11; 8]),
        "what the hand-off wrote is what the device's buffer holds"
    );
    playback_stream.stop_requesting().unwrap();

    assert_eq!(
        ask_for_one_block_through_the_installed_callback(node_host, 8, 2),
        None
    );
    assert_eq!(*asked_sample_counts.lock().unwrap(), vec![2]);
}

#[test]
fn a_playback_stream_reports_the_failure_that_ended_it() {
    let node_host = stood_up_node_host();
    let table = v1_12_table_over(node_host);
    let module_host = unsafe { ModuleHost::from_module_host_table_pointer(&table) }.unwrap();
    let playback_stream = module_host
        .open_audio_playback_stream(&ModuleHostAudioPlaybackStreamRequest::default())
        .unwrap();
    let liveness = playback_stream.liveness();
    assert_eq!(liveness.failure_that_ended_the_stream().unwrap(), None);

    node_host.audio_playback.lock().unwrap().ended_with = Some("the sink went away".into());

    assert_eq!(
        liveness.failure_that_ended_the_stream().unwrap(),
        Some("the sink went away".to_string())
    );
}

#[test]
fn a_dropped_playback_stream_is_closed_and_releases_its_reference() {
    let node_host = stood_up_node_host();
    let table = v1_12_table_over(node_host);
    let module_host = unsafe { ModuleHost::from_module_host_table_pointer(&table) }.unwrap();
    let mut playback_stream = module_host
        .open_audio_playback_stream(&ModuleHostAudioPlaybackStreamRequest::default())
        .unwrap();
    playback_stream
        .start_requesting_from(Box::new(|_requested_block| {}))
        .unwrap();
    assert_eq!(node_host.references.load(Ordering::SeqCst), 2);

    drop(playback_stream);

    let audio_playback = node_host.audio_playback.lock().unwrap();
    assert!(audio_playback.closed);
    assert!(audio_playback.installed_callback.is_none());
    drop(audio_playback);
    assert_eq!(node_host.references.load(Ordering::SeqCst), 1);
}

#[test]
fn a_virtual_camera_offers_an_extent_writes_rows_and_is_closed_once_when_it_drops() {
    let node_host = stood_up_node_host();
    let table = v1_12_table_over(node_host);
    let module_host = unsafe { ModuleHost::from_module_host_table_pointer(&table) }.unwrap();

    let camera = module_host
        .open_virtual_camera_stream("desk cam")
        .expect("the camera opens");
    camera.offer_extent(4, 2, None).unwrap();
    let presentation = camera.write_yuyv_frame(&[0; 16], 8, 42).unwrap();
    assert_eq!(
        presentation,
        ModuleHostVirtualCameraFramePresentation::EveryBufferIsHeldByTheConsumer
    );
    assert_eq!(
        camera.failure_that_ended_the_stream().unwrap().as_deref(),
        Some("the PipeWire daemon went away")
    );
    drop(camera);

    assert_eq!(
        *node_host.virtual_camera_calls.lock().unwrap(),
        vec![
            "open desk cam".to_string(),
            "offer 9 4x2 fps=0".to_string(),
            "write 9 bytes=16 stride=8 at=42".to_string(),
            "close 9".to_string(),
        ]
    );
}

#[test]
fn a_virtual_camera_the_host_cannot_open_is_refused_in_its_words() {
    let node_host = stood_up_node_host();
    let table = v1_12_table_over(node_host);
    let module_host = unsafe { ModuleHost::from_module_host_table_pointer(&table) }.unwrap();

    let refusal = module_host.open_virtual_camera_stream("").unwrap_err();

    assert_eq!(
        refusal.to_string(),
        "virtual_camera_stream_open: no PipeWire daemon answers"
    );
    assert!(node_host.virtual_camera_calls.lock().unwrap().is_empty());
}
