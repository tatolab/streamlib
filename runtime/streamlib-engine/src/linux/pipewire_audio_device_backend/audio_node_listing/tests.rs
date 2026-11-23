// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

fn a_node(node_name: &str, node_description: Option<&str>, media_class: &str) -> PipeWireAudioNode {
    PipeWireAudioNode {
        node_name: node_name.to_owned(),
        node_description: node_description.map(str::to_owned),
        media_class: media_class.to_owned(),
    }
}

fn a_session() -> PipeWireAudioNodesListed {
    PipeWireAudioNodesListed {
        nodes: vec![
            a_node(
                "alsa_output.pci-0000_00_1f.3.analog-stereo",
                Some("Built-in Audio Analog Stereo"),
                "Audio/Sink",
            ),
            a_node(
                "alsa_input.pci-0000_00_1f.3.analog-stereo",
                Some("Built-in Audio Analog Stereo"),
                "Audio/Source",
            ),
            a_node(
                "alsa_output.usb-headset.analog-stereo",
                Some("USB Headset"),
                "Audio/Sink",
            ),
            a_node(
                "alsa_input.usb-mic.mono",
                Some("USB Microphone"),
                "Audio/Source",
            ),
            a_node("virtual-source", None, "Audio/Source/Virtual"),
        ],
        default_sink_name: Some("alsa_output.usb-headset.analog-stereo".to_owned()),
        default_source_name: Some("alsa_input.usb-mic.mono".to_owned()),
    }
}

#[test]
fn the_sources_then_each_sinks_monitor_are_listed_each_default_first() {
    let listed: Vec<(String, String, AudioCaptureDeviceKind)> = a_session()
        .capture_devices()
        .into_iter()
        .map(|device| (device.id, device.name, device.kind))
        .collect();
    let expected = [
        (
            "alsa_input.usb-mic.mono",
            "USB Microphone",
            AudioCaptureDeviceKind::Input,
        ),
        (
            "alsa_input.pci-0000_00_1f.3.analog-stereo",
            "Built-in Audio Analog Stereo",
            AudioCaptureDeviceKind::Input,
        ),
        (
            "virtual-source",
            "virtual-source",
            AudioCaptureDeviceKind::Input,
        ),
        (
            "alsa_output.usb-headset.analog-stereo.monitor",
            "Monitor of USB Headset",
            AudioCaptureDeviceKind::OutputMonitor,
        ),
        (
            "alsa_output.pci-0000_00_1f.3.analog-stereo.monitor",
            "Monitor of Built-in Audio Analog Stereo",
            AudioCaptureDeviceKind::OutputMonitor,
        ),
    ]
    .map(|(id, name, kind)| (id.to_owned(), name.to_owned(), kind));
    assert_eq!(listed, expected);
}

#[test]
fn the_default_output_monitor_is_the_default_sinks_monitor() {
    assert_eq!(
        a_session().default_output_monitor_device_id().as_deref(),
        Some("alsa_output.usb-headset.analog-stereo.monitor")
    );
}

#[test]
fn a_default_sink_the_session_does_not_hold_resolves_to_no_monitor() {
    let session = PipeWireAudioNodesListed {
        default_sink_name: Some("alsa_output.unplugged".to_owned()),
        ..a_session()
    };
    assert_eq!(session.default_output_monitor_device_id(), None);
}

#[test]
fn a_default_naming_a_source_is_no_default_sink() {
    let session = PipeWireAudioNodesListed {
        default_sink_name: Some("alsa_input.usb-mic.mono".to_owned()),
        ..a_session()
    };
    assert_eq!(session.default_output_monitor_device_id(), None);
}

fn node_name_of_default_metadata_value(value: &str, capacity: usize) -> Option<String> {
    let value = std::ffi::CString::new(value).unwrap();
    let mut node_name = vec![0u8; capacity];
    // SAFETY: `value` is NUL-terminated and `node_name` holds `capacity` bytes.
    let read = unsafe {
        listing_shim::streamlib_pipewire_node_name_of_default_metadata_value(
            value.as_ptr(),
            node_name.as_mut_ptr().cast::<c_char>(),
            capacity,
        )
    };
    let written = node_name_written_into(&node_name);
    assert_eq!(
        read,
        written.is_some(),
        "the verdict and the buffer disagree"
    );
    written
}

#[test]
fn the_default_metadata_value_is_read_for_the_node_it_names() {
    assert_eq!(
        node_name_of_default_metadata_value(
            r#"{ "name": "alsa_output.usb-headset.analog-stereo" }"#,
            256
        )
        .as_deref(),
        Some("alsa_output.usb-headset.analog-stereo")
    );
    assert_eq!(
        node_name_of_default_metadata_value(r#"{"other":[1,2],"name":"sink"}"#, 256).as_deref(),
        Some("sink"),
        "a key before `name` is skipped, whatever its value"
    );
}

#[test]
fn a_default_metadata_value_naming_no_node_reads_as_none() {
    for value in ["", "null", "{}", r#"{"name":""}"#, r#"["name","sink"]"#] {
        assert_eq!(
            node_name_of_default_metadata_value(value, 256),
            None,
            "{value:?} names no node"
        );
    }
}

#[test]
fn a_node_name_too_long_for_the_buffer_reads_as_none_rather_than_truncated() {
    assert_eq!(
        node_name_of_default_metadata_value(r#"{"name":"a-long-sink-name"}"#, 8),
        None
    );
}
