// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

fn a_device(id: &str, name: &str, kind: AudioCaptureDeviceKind) -> AudioCaptureDevice {
    AudioCaptureDevice {
        id: id.to_owned(),
        name: name.to_owned(),
        kind,
    }
}

#[test]
fn the_alias_is_listed_ahead_of_the_monitors_named_after_the_one_it_resolves_to() {
    let listed = with_the_default_output_monitor_alias(
        vec![
            a_device("mic", "USB Microphone", AudioCaptureDeviceKind::Input),
            a_device(
                "hdmi.monitor",
                "Monitor of HDMI",
                AudioCaptureDeviceKind::OutputMonitor,
            ),
            a_device(
                "speakers.monitor",
                "Monitor of Speakers",
                AudioCaptureDeviceKind::OutputMonitor,
            ),
        ],
        Some("speakers.monitor"),
    );
    let ids: Vec<&str> = listed.iter().map(|device| device.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "mic",
            DEFAULT_OUTPUT_MONITOR_DEVICE_ID,
            "hdmi.monitor",
            "speakers.monitor"
        ]
    );
    assert_eq!(
        listed[1].name,
        "What the default output plays (now Monitor of Speakers)"
    );
    assert_eq!(listed[1].kind, AudioCaptureDeviceKind::OutputMonitor);
}

#[test]
fn a_backend_with_no_default_output_monitor_lists_no_alias() {
    let devices = vec![a_device(
        "BuiltInMicrophoneDevice",
        "MacBook Pro Microphone",
        AudioCaptureDeviceKind::Input,
    )];
    assert_eq!(
        with_the_default_output_monitor_alias(devices.clone(), None),
        devices
    );
}

#[test]
fn an_alias_with_no_listed_monitors_goes_last_and_is_named_by_the_id_it_resolves_to() {
    let listed = with_the_default_output_monitor_alias(
        vec![a_device("mic", "Mic", AudioCaptureDeviceKind::Input)],
        Some("sink.monitor"),
    );
    assert_eq!(listed[1].id, DEFAULT_OUTPUT_MONITOR_DEVICE_ID);
    assert_eq!(
        listed[1].name,
        "What the default output plays (now sink.monitor)"
    );
}

#[test]
fn the_alias_on_a_backend_with_no_default_output_monitor_is_refused_naming_both() {
    let refusal = the_default_output_monitor_or_its_refusal("coreaudio", None)
        .expect_err("no monitor to resolve to")
        .to_string();
    assert!(
        refusal.contains(DEFAULT_OUTPUT_MONITOR_DEVICE_ID),
        "{refusal}"
    );
    assert!(refusal.contains("coreaudio"), "{refusal}");
}

/// A backend whose default output's monitor is `sink.monitor`, refusing every open by
/// naming the device it was asked for.
struct BackendNamingTheDeviceItWasAskedFor;

impl AudioDeviceBackend for BackendNamingTheDeviceItWasAskedFor {
    fn backend_name(&self) -> &'static str {
        "naming"
    }

    fn list_capture_devices(&self) -> Result<AudioCaptureDeviceListing> {
        Ok(AudioCaptureDeviceListing {
            devices: Vec::new(),
            default_output_monitor_device_id: Some("sink.monitor".to_owned()),
        })
    }

    fn open_capture_stream(
        &self,
        request: &AudioDeviceStreamRequest,
    ) -> Result<Box<dyn crate::core::context::AudioCaptureStream>> {
        Err(crate::core::Error::Configuration(format!(
            "asked to open {:?}",
            request.device_id
        )))
    }

    fn open_playback_stream(
        &self,
        _request: &AudioDeviceStreamRequest,
    ) -> Result<Box<dyn crate::core::context::AudioPlaybackStream>> {
        unreachable!("only capture opens the monitor")
    }
}

fn a_request_for(device_id: &str) -> AudioDeviceStreamRequest {
    AudioDeviceStreamRequest {
        device_id: Some(device_id.to_owned()),
        deviceless_pacing_clock: std::sync::Arc::new(
            crate::core::context::SoftwareAudioClock::with_defaults(),
        ),
        capture_device_access_prompting: Default::default(),
        requested_format: Default::default(),
    }
}

fn the_device_a_capture_open_asked_for(
    backend: &dyn AudioDeviceBackend,
    device_id: &str,
) -> String {
    crate::core::context::open_capture_stream_in_requested_format(
        backend,
        &a_request_for(device_id),
    )
    .err()
    .expect("the backend refuses every open")
    .to_string()
}

#[test]
fn a_capture_stream_opened_on_the_alias_opens_the_monitor_it_resolves_to() {
    let asked = the_device_a_capture_open_asked_for(
        &BackendNamingTheDeviceItWasAskedFor,
        DEFAULT_OUTPUT_MONITOR_DEVICE_ID,
    );
    assert!(asked.contains("Some(\"sink.monitor\")"), "{asked}");
    let asked = the_device_a_capture_open_asked_for(&BackendNamingTheDeviceItWasAskedFor, "mic");
    assert!(
        asked.contains("Some(\"mic\")"),
        "another id is opened as named: {asked}"
    );
}

#[test]
fn a_capture_stream_opened_on_the_alias_where_no_monitor_exists_is_refused_naming_it() {
    let refusal = the_device_a_capture_open_asked_for(
        &crate::core::context::SilentNullAudioDeviceBackend,
        DEFAULT_OUTPUT_MONITOR_DEVICE_ID,
    );
    assert!(
        refusal.contains(DEFAULT_OUTPUT_MONITOR_DEVICE_ID),
        "{refusal}"
    );
    assert!(refusal.contains("silent-null"), "{refusal}");
}

#[test]
fn the_alias_resolves_to_the_monitor_the_backend_names() {
    assert_eq!(
        the_default_output_monitor_or_its_refusal("pipewire", Some("sink.monitor".into())).unwrap(),
        "sink.monitor"
    );
}

#[test]
fn the_default_moves_to_the_front_and_the_rest_keep_their_order() {
    assert_eq!(
        moved_to_the_front(vec![1, 2, 3, 4], |item| *item == 3),
        [3, 1, 2, 4]
    );
    assert_eq!(
        moved_to_the_front(vec![1, 2, 3], |item| *item == 1),
        [1, 2, 3]
    );
    assert_eq!(
        moved_to_the_front(vec![1, 2, 3], |item| *item == 9),
        [1, 2, 3],
        "no default leaves the order alone"
    );
    assert_eq!(
        moved_to_the_front(vec![1, 3, 3], |item| *item == 3),
        [3, 1, 3],
        "only the first default moves"
    );
}

#[test]
fn a_device_with_no_name_or_a_blank_one_is_named_by_its_id() {
    for name in [None, Some(String::new()), Some("  ".to_owned())] {
        assert_eq!(
            AudioCaptureDevice::named_or_by_id(
                "hw-id".to_owned(),
                name,
                AudioCaptureDeviceKind::Input
            )
            .name,
            "hw-id"
        );
    }
    assert_eq!(
        AudioCaptureDevice::named_or_by_id(
            "hw-id".to_owned(),
            Some("USB Microphone".to_owned()),
            AudioCaptureDeviceKind::Input
        )
        .name,
        "USB Microphone"
    );
}
