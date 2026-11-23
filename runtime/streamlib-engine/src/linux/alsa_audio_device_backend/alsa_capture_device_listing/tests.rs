// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

fn a_hint(name: &str, description: Option<&str>, direction: Option<&str>) -> AlsaPcmHint {
    AlsaPcmHint {
        name: name.to_owned(),
        description: description.map(str::to_owned),
        direction: direction.map(str::to_owned),
    }
}

#[test]
fn the_capture_pcms_are_listed_default_first_with_each_description_on_one_line() {
    let devices = alsa_capture_devices_among(vec![
        a_hint("null", Some("Discard all samples"), None),
        a_hint(
            "sysdefault:CARD=PCH",
            Some("HDA Intel PCH, ALC892 Analog\nDefault Audio Device"),
            None,
        ),
        a_hint("default", Some("Default ALSA Output"), None),
        a_hint(
            "front:CARD=PCH,DEV=0",
            Some("HDA Intel PCH, ALC892 Analog\nFront output / input"),
            None,
        ),
        a_hint(
            "surround51:CARD=PCH,DEV=0",
            Some("HDA Intel PCH\n5.1 Surround output"),
            Some("Output"),
        ),
        a_hint("dsnoop:CARD=Mic,DEV=0", None, Some("Input")),
        a_hint(
            "hw:CARD=PCH,DEV=0",
            Some("HDA Intel PCH\nDirect hardware device"),
            None,
        ),
        a_hint(
            "plughw:CARD=PCH,DEV=0",
            Some("HDA Intel PCH\nHardware device"),
            None,
        ),
    ]);
    let listed: Vec<(&str, &str)> = devices
        .iter()
        .map(|device| (device.id.as_str(), device.name.as_str()))
        .collect();
    assert_eq!(
        listed,
        [
            ("default", "Default input"),
            (
                "sysdefault:CARD=PCH",
                "HDA Intel PCH, ALC892 Analog, Default Audio Device"
            ),
            (
                "front:CARD=PCH,DEV=0",
                "HDA Intel PCH, ALC892 Analog, Front output / input"
            ),
            ("dsnoop:CARD=Mic,DEV=0", "dsnoop:CARD=Mic,DEV=0"),
        ],
        "an output-only PCM, the discarding one and the raw card ones are not listed, \
         `default` is named for what it captures, and a PCM with no description is named by \
         its id"
    );
    assert!(
        devices
            .iter()
            .all(|device| device.kind == AudioCaptureDeviceKind::Input),
        "an ALSA PCM is never an output's monitor"
    );
}

#[test]
fn a_pcm_listed_twice_is_listed_once() {
    let devices = alsa_capture_devices_among(vec![
        a_hint("pipewire", Some("PipeWire Sound Server"), None),
        a_hint("pipewire", Some("PipeWire Sound Server"), None),
    ]);
    assert_eq!(devices.len(), 1);
}
