// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! `tatolab-video-encode-host`: the machine's encode host, run by the runtime
//! as a shared native service. It listens on the socket the runtime names,
//! and every connection is one encoder node's session on the one encode
//! device it opens per GPU. `SIGTERM` ends it.

use std::process::ExitCode;

use streamlib_runtime_client_contract::shared_native_service_command_line::SharedNativeServiceCommandLine;

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();
    let command_line = match SharedNativeServiceCommandLine::of_the_arguments(
        "the encode host",
        std::env::args().skip(1),
    ) {
        Ok(command_line) => command_line,
        Err(refusal) => {
            tracing::error!("{refusal}");
            return ExitCode::from(2);
        }
    };
    match serve_encode_sessions_on(&command_line.listen_socket) {
        Ok(()) => ExitCode::SUCCESS,
        Err(failure) => {
            tracing::error!("{failure}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(target_os = "linux")]
fn serve_encode_sessions_on(listen_socket: &std::path::Path) -> Result<(), String> {
    // The runtime made the socket's directory; replace a socket file an
    // earlier host left in it.
    let _ = std::fs::remove_file(listen_socket);
    let listener = std::os::unix::net::UnixListener::bind(listen_socket)
        .map_err(|failure| format!("{} could not be bound: {failure}", listen_socket.display()))?;
    tracing::info!(
        "the encode host {} serves encode sessions on {}",
        std::process::id(),
        listen_socket.display()
    );
    streamlib_owned_device_video_codec_nodes::video_encode_host::serve_video_encode_sessions(
        listener,
    );
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn serve_encode_sessions_on(_listen_socket: &std::path::Path) -> Result<(), String> {
    Err("the encode host encodes with Vulkan Video, on Linux only".to_string())
}
