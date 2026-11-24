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

#[test]
fn a_source_the_open_path_would_read_as_a_monitor_is_not_listed() {
    let mut session = a_session();
    session.nodes.push(a_node(
        "loopback.monitor",
        Some("A loopback"),
        "Audio/Source",
    ));
    assert_eq!(listed_devices(&session), listed_devices(&a_session()));
}

#[test]
fn a_node_the_session_removes_before_the_listing_ends_is_dropped_and_the_rest_keep_their_order() {
    let mut nodes: AudioNodesByRegistryId = Vec::new();
    let context = (&raw mut nodes).cast::<c_void>();
    let name = |text: &str| CString::new(text).unwrap();
    let (sink, source, other_sink) = (name("sink-a"), name("source-a"), name("sink-b"));
    let (sink_class, source_class) = (name("Audio/Sink"), name("Audio/Source"));
    // SAFETY: `context` is `nodes`, and every string outlives its call.
    unsafe {
        record_a_listed_audio_node(
            context,
            30,
            sink.as_ptr(),
            std::ptr::null(),
            sink_class.as_ptr(),
        );
        record_a_listed_audio_node(
            context,
            31,
            source.as_ptr(),
            std::ptr::null(),
            source_class.as_ptr(),
        );
        record_a_listed_audio_node(
            context,
            32,
            other_sink.as_ptr(),
            std::ptr::null(),
            sink_class.as_ptr(),
        );
        forget_a_removed_global(context, 30);
        forget_a_removed_global(context, 99);
    }
    let names: Vec<(u32, String)> = nodes
        .into_iter()
        .map(|(id, node)| (id, node.node_name))
        .collect();
    assert_eq!(
        names,
        [(31, "source-a".to_owned()), (32, "sink-b".to_owned())]
    );
}

fn the_capture_refusal_for(device_id: &str) -> Option<String> {
    super::super::refuse_a_capture_device_id_the_session_does_not_list(
        device_id,
        a_session().capture_device_listing(),
    )
    .err()
    .map(|refusal| refusal.to_string())
}

#[test]
fn a_source_a_sinks_monitor_and_the_default_output_monitor_alias_open_for_capture() {
    for device_id in [
        "alsa_input.usb-mic.mono",
        "virtual-source",
        "alsa_output.pci-0000_00_1f.3.analog-stereo.monitor",
        crate::core::context::DEFAULT_OUTPUT_MONITOR_DEVICE_ID,
    ] {
        assert_eq!(the_capture_refusal_for(device_id), None, "{device_id}");
    }
}

#[test]
fn a_source_the_session_does_not_hold_is_refused_naming_the_ones_it_does() {
    let refusal = the_capture_refusal_for("alsa_input.usb-gone.mono")
        .expect("the source is not in the session");
    for expected in [
        "'alsa_input.usb-gone.mono'",
        "alsa_input.usb-mic.mono (USB Microphone)",
        "alsa_output.usb-headset.analog-stereo.monitor (Monitor of USB Headset)",
    ] {
        assert!(refusal.contains(expected), "{expected} in: {refusal}");
    }
}

#[test]
fn the_monitor_of_a_sink_the_session_does_not_hold_is_refused() {
    assert!(the_capture_refusal_for("alsa_output.usb-gone.analog-stereo.monitor").is_some());
}

#[test]
fn a_sink_named_without_its_monitor_suffix_is_refused_for_capture() {
    assert!(the_capture_refusal_for("alsa_output.usb-headset.analog-stereo").is_some());
}
