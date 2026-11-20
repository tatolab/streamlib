// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! What the encode host is started with: the launch contract every shared
//! native service takes.

use std::path::PathBuf;

/// The switch naming the socket the host listens for sessions on.
const LISTEN_SOCKET_SWITCH: &str = "--listen-socket";

/// The switch naming the directory the runtime hands every shared native
/// service; the encode host keeps nothing there.
const STATE_DIRECTORY_SWITCH: &str = "--state-directory";

#[derive(Debug, PartialEq)]
pub(crate) struct VideoEncodeHostCommandLine {
    pub(crate) listen_socket: PathBuf,
    pub(crate) state_directory: PathBuf,
}

pub(crate) fn video_encode_host_command_line_of(
    arguments: impl IntoIterator<Item = String>,
) -> Result<VideoEncodeHostCommandLine, String> {
    let mut listen_socket = None;
    let mut state_directory = None;
    let mut arguments = arguments.into_iter();
    while let Some(argument) = arguments.next() {
        let slot = match argument.as_str() {
            LISTEN_SOCKET_SWITCH => &mut listen_socket,
            STATE_DIRECTORY_SWITCH => &mut state_directory,
            other => return Err(format!("{other} is not a switch the encode host takes")),
        };
        let value = arguments
            .next()
            .ok_or_else(|| format!("{argument} names a path"))?;
        *slot = Some(PathBuf::from(value));
    }
    Ok(VideoEncodeHostCommandLine {
        listen_socket: listen_socket
            .ok_or_else(|| format!("{LISTEN_SOCKET_SWITCH} is required"))?,
        state_directory: state_directory
            .ok_or_else(|| format!("{STATE_DIRECTORY_SWITCH} is required"))?,
    })
}
