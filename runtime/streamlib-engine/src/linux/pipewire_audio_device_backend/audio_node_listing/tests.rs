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

fn listed_devices(
    session: &PipeWireAudioNodesListed,
) -> Vec<(String, String, AudioCaptureDeviceKind)> {
    session
        .capture_device_listing()
        .devices
        .into_iter()
        .map(|device| (device.id, device.name, device.kind))
        .collect()
}

#[test]
fn the_sources_then_each_sinks_monitor_are_listed_each_default_first() {
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
    assert_eq!(listed_devices(&a_session()), expected);
}

#[test]
fn each_listed_monitor_id_opens_the_monitor_of_its_sink() {
    let sink_names: Vec<String> = a_session()
        .nodes
        .into_iter()
        .filter(|node| node.plays())
        .map(|node| node.node_name)
        .collect();
    let monitored: Vec<String> = a_session()
        .capture_device_listing()
        .devices
        .into_iter()
        .filter(|device| device.kind == AudioCaptureDeviceKind::OutputMonitor)
        .map(|device| {
            crate::linux::pipewire_audio_device_backend::tests::monitored_sink_name_of(&device.id)
                .unwrap_or_else(|| panic!("{} names no sink to the open path", device.id))
        })
        .collect();
    assert_eq!(monitored.len(), sink_names.len());
    for sink_name in sink_names {
        assert!(
            monitored.contains(&sink_name),
            "{sink_name} has no monitor that opens"
        );
    }
}

#[test]
fn a_sink_whose_name_the_open_path_cannot_hold_lists_no_monitor() {
    // SAFETY: the shim returns a compile-time constant.
    let too_long =
        "s".repeat(unsafe { listing_shim::streamlib_pipewire_max_monitored_sink_name_bytes() });
    let mut session = a_session();
    session.nodes.push(a_node(&too_long, None, "Audio/Sink"));
    session.default_sink_name = Some(too_long.clone());
    let listing = session.capture_device_listing();
    assert!(
        listing
            .devices
            .iter()
            .all(|device| !device.id.starts_with(&too_long))
    );
    assert_eq!(listing.default_output_monitor_device_id, None);
    assert_eq!(listing.devices.len(), listed_devices(&a_session()).len());
}

#[test]
fn the_default_output_monitor_is_the_default_sinks_monitor() {
    assert_eq!(
        a_session()
            .capture_device_listing()
            .default_output_monitor_device_id
            .as_deref(),
        Some("alsa_output.usb-headset.analog-stereo.monitor")
    );
}

#[test]
fn a_default_sink_the_session_does_not_hold_resolves_to_no_monitor() {
    let session = PipeWireAudioNodesListed {
        default_sink_name: Some("alsa_output.unplugged".to_owned()),
        ..a_session()
    };
    assert_eq!(
        session
            .capture_device_listing()
            .default_output_monitor_device_id,
        None
    );
}

#[test]
fn a_default_naming_a_source_is_no_default_sink() {
    let session = PipeWireAudioNodesListed {
        default_sink_name: Some("alsa_input.usb-mic.mono".to_owned()),
        ..a_session()
    };
    assert_eq!(
        session
            .capture_device_listing()
            .default_output_monitor_device_id,
        None
    );
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

#[test]
fn a_key_too_long_to_read_is_skipped_and_the_name_after_it_still_read() {
    let value = format!(
        r#"{{"{}":"x","name":"alsa_output.usb-headset"}}"#,
        "k".repeat(200)
    );
    assert_eq!(
        node_name_of_default_metadata_value(&value, 256).as_deref(),
        Some("alsa_output.usb-headset")
    );
}
