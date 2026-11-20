// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::path::PathBuf;

use crate::shared_native_service_command_line::SharedNativeServiceCommandLine;

fn command_line_of(arguments: &[&str]) -> Result<SharedNativeServiceCommandLine, String> {
    SharedNativeServiceCommandLine::of_the_arguments(
        "the encode host",
        arguments.iter().map(|argument| argument.to_string()),
    )
}

#[test]
fn a_service_reads_the_socket_and_the_directory_it_was_started_with() {
    assert_eq!(
        command_line_of(&[
            "--state-directory",
            "/var/tatolab/services/video-encode-host",
            "--listen-socket",
            "/run/tatolab/video-encode-host.sock",
        ]),
        Ok(SharedNativeServiceCommandLine {
            listen_socket: PathBuf::from("/run/tatolab/video-encode-host.sock"),
            state_directory: PathBuf::from("/var/tatolab/services/video-encode-host"),
        })
    );
}

#[test]
fn a_missing_value_switch_or_unknown_switch_is_refused_by_name() {
    assert_eq!(
        command_line_of(&["--state-directory", "/s"]),
        Err("--listen-socket is required".to_string())
    );
    assert_eq!(
        command_line_of(&["--listen-socket", "/l"]),
        Err("--state-directory is required".to_string())
    );
    assert_eq!(
        command_line_of(&["--listen-socket"]),
        Err("--listen-socket names a path".to_string())
    );
    assert_eq!(
        command_line_of(&["--width", "1"]),
        Err("--width is not a switch the encode host takes".to_string())
    );
}
