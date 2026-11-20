// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The command line the runtime starts a shared native service with:
//! `<executable> --listen-socket <socket> --state-directory <directory>`,
//! spelled once for the runtime that starts a service and the service that
//! reads it.

use std::path::PathBuf;

/// The switch naming the socket a shared native service listens on.
pub const SHARED_NATIVE_SERVICE_LISTEN_SOCKET_SWITCH: &str = "--listen-socket";

/// The switch naming the directory a shared native service keeps its state in.
pub const SHARED_NATIVE_SERVICE_STATE_DIRECTORY_SWITCH: &str = "--state-directory";

/// What a shared native service is started with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharedNativeServiceCommandLine {
    pub listen_socket: PathBuf,
    pub state_directory: PathBuf,
}

impl SharedNativeServiceCommandLine {
    /// Read `arguments`, the command line after the executable, refusing by
    /// name a switch `service` does not take or one left out.
    pub fn of_the_arguments(
        service: &str,
        arguments: impl IntoIterator<Item = String>,
    ) -> Result<Self, String> {
        let mut listen_socket = None;
        let mut state_directory = None;
        let mut arguments = arguments.into_iter();
        while let Some(argument) = arguments.next() {
            let slot = match argument.as_str() {
                SHARED_NATIVE_SERVICE_LISTEN_SOCKET_SWITCH => &mut listen_socket,
                SHARED_NATIVE_SERVICE_STATE_DIRECTORY_SWITCH => &mut state_directory,
                other => return Err(format!("{other} is not a switch {service} takes")),
            };
            let value = arguments
                .next()
                .ok_or_else(|| format!("{argument} names a path"))?;
            *slot = Some(PathBuf::from(value));
        }
        Ok(Self {
            listen_socket: listen_socket.ok_or_else(|| {
                format!("{SHARED_NATIVE_SERVICE_LISTEN_SOCKET_SWITCH} is required")
            })?,
            state_directory: state_directory.ok_or_else(|| {
                format!("{SHARED_NATIVE_SERVICE_STATE_DIRECTORY_SWITCH} is required")
            })?,
        })
    }
}
