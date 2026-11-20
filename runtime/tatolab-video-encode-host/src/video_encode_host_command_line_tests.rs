// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::path::PathBuf;

use crate::video_encode_host_command_line::{
    VideoEncodeHostCommandLine, video_encode_host_command_line_of,
};

fn arguments(arguments: &[&str]) -> Vec<String> {
    arguments
        .iter()
        .map(|argument| argument.to_string())
        .collect()
}

#[test]
fn the_host_takes_the_launch_contract_of_a_shared_native_service() {
    assert_eq!(
        video_encode_host_command_line_of(arguments(&[
            "--listen-socket",
            "/run/tatolab/video-encode-host.sock",
            "--state-directory",
            "/var/tatolab/video-encode-host",
        ])),
        Ok(VideoEncodeHostCommandLine {
            listen_socket: PathBuf::from("/run/tatolab/video-encode-host.sock"),
            state_directory: PathBuf::from("/var/tatolab/video-encode-host"),
        })
    );
}

#[test]
fn a_missing_or_unknown_switch_is_refused_by_name() {
    assert_eq!(
        video_encode_host_command_line_of(arguments(&["--state-directory", "/s"])),
        Err("--listen-socket is required".to_string())
    );
    assert_eq!(
        video_encode_host_command_line_of(arguments(&["--listen-socket"])),
        Err("--listen-socket names a path".to_string())
    );
    assert_eq!(
        video_encode_host_command_line_of(arguments(&["--width", "1"])),
        Err("--width is not a switch the encode host takes".to_string())
    );
}
