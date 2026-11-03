// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `tatolab-browser-host`: the machine's one CEF offscreen browser, started and
//! kept running by the runtime while any `BrowserSource` holds a lease on it.
//!
//! It listens on the socket the runtime names, and every connection is one
//! node's tab. CEF runs on this process's main thread; each stream's tabs share
//! one profile under the state directory. `SIGTERM` closes every page and ends
//! the host.

mod browser_host_cef_thread;
mod browser_host_page_handlers;
mod browser_host_tab;
mod browser_host_tab_connections;
mod page_data_scripts_awaiting_the_loaded_document;
mod tenant_profile_directory;

use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};

use browser_host_cef_thread::{
    cef_thread_request_channel, initialize_cef, serve_the_cef_thread_until_stopped,
};

/// The switch naming the socket the host listens for tabs on.
const LISTEN_SOCKET_SWITCH: &str = "--listen-socket";

/// The switch naming the directory the host keeps CEF's cache and every
/// stream's profile in.
const STATE_DIRECTORY_SWITCH: &str = "--state-directory";

/// CEF's root cache, under the state directory.
const CEF_ROOT_CACHE_DIRECTORY_NAME: &str = "cef";

/// Set by `SIGTERM` or `SIGINT`; the CEF thread stops once it sees it.
static STOP_REQUESTED: AtomicBool = AtomicBool::new(false);

/// What the host is started with.
#[derive(Debug, PartialEq)]
struct BrowserHostCommandLine {
    listen_socket: PathBuf,
    state_directory: PathBuf,
}

fn browser_host_command_line_of(
    arguments: impl IntoIterator<Item = String>,
) -> Result<BrowserHostCommandLine, String> {
    let mut listen_socket = None;
    let mut state_directory = None;
    let mut arguments = arguments.into_iter();
    while let Some(argument) = arguments.next() {
        let slot = match argument.as_str() {
            LISTEN_SOCKET_SWITCH => &mut listen_socket,
            STATE_DIRECTORY_SWITCH => &mut state_directory,
            other => return Err(format!("{other} is not a switch the browser host takes")),
        };
        let value = arguments
            .next()
            .ok_or_else(|| format!("{argument} names a path"))?;
        *slot = Some(PathBuf::from(value));
    }
    Ok(BrowserHostCommandLine {
        listen_socket: listen_socket
            .ok_or_else(|| format!("{LISTEN_SOCKET_SWITCH} is required"))?,
        state_directory: state_directory
            .ok_or_else(|| format!("{STATE_DIRECTORY_SWITCH} is required"))?,
    })
}

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();
    let command_line = match browser_host_command_line_of(std::env::args().skip(1)) {
        Ok(command_line) => command_line,
        Err(refusal) => {
            tracing::error!("{refusal}");
            return ExitCode::from(2);
        }
    };
    match run_the_browser_host(&command_line) {
        Ok(()) => ExitCode::SUCCESS,
        Err(failure) => {
            tracing::error!("{failure}");
            ExitCode::FAILURE
        }
    }
}

fn run_the_browser_host(command_line: &BrowserHostCommandLine) -> Result<(), String> {
    let cef_runtime_directory = cef_runtime_directory_beside_this_executable()?;
    let root_cache_directory = command_line
        .state_directory
        .join(CEF_ROOT_CACHE_DIRECTORY_NAME);
    request_a_stop_on_sigterm_and_sigint();
    let (cef_thread_requests, requests_to_serve) = cef_thread_request_channel();
    initialize_cef(
        &cef_runtime_directory,
        &root_cache_directory,
        &cef_thread_requests,
    )?;
    let listener = bind_the_listen_socket(&command_line.listen_socket)?;
    let accepting_requests = cef_thread_requests.clone();
    std::thread::Builder::new()
        .name("tab-acceptor".to_string())
        .spawn(move || {
            browser_host_tab_connections::accept_tab_connections(listener, accepting_requests)
        })
        .map_err(|failure| format!("the tab acceptor did not start: {failure}"))?;
    tracing::info!(
        "the browser host {} serves tabs on {}",
        std::process::id(),
        command_line.listen_socket.display()
    );
    serve_the_cef_thread_until_stopped(
        requests_to_serve,
        &cef_thread_requests,
        &root_cache_directory,
        &STOP_REQUESTED,
    );
    let _ = std::fs::remove_file(&command_line.listen_socket);
    tracing::info!("the browser host {} stopped", std::process::id());
    Ok(())
}

fn cef_runtime_directory_beside_this_executable() -> Result<PathBuf, String> {
    let executable = std::env::current_exe()
        .map_err(|failure| format!("the host's own executable was not found: {failure}"))?;
    executable
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| format!("{} has no directory", executable.display()))
}

/// Bind `listen_socket`, replacing a socket file an earlier host left.
fn bind_the_listen_socket(listen_socket: &Path) -> Result<UnixListener, String> {
    if let Some(directory) = listen_socket.parent() {
        std::fs::create_dir_all(directory).map_err(|failure| {
            format!("{} could not be created: {failure}", directory.display())
        })?;
    }
    let _ = std::fs::remove_file(listen_socket);
    UnixListener::bind(listen_socket)
        .map_err(|failure| format!("{} could not be bound: {failure}", listen_socket.display()))
}

fn request_a_stop_on_sigterm_and_sigint() {
    extern "C" fn request_a_stop(_signal: libc::c_int) {
        STOP_REQUESTED.store(true, Ordering::Release);
    }
    // SAFETY: the handler only stores to an atomic, which is async-signal-safe.
    unsafe {
        libc::signal(
            libc::SIGTERM,
            request_a_stop as *const () as libc::sighandler_t,
        );
        libc::signal(
            libc::SIGINT,
            request_a_stop as *const () as libc::sighandler_t,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arguments(text: &[&str]) -> Vec<String> {
        text.iter().map(|argument| argument.to_string()).collect()
    }

    #[test]
    fn the_host_is_started_with_its_socket_and_state_directory() {
        assert_eq!(
            browser_host_command_line_of(arguments(&[
                "--listen-socket",
                "/run/tatolab/browser-host.sock",
                "--state-directory",
                "/state/browser-host",
            ])),
            Ok(BrowserHostCommandLine {
                listen_socket: PathBuf::from("/run/tatolab/browser-host.sock"),
                state_directory: PathBuf::from("/state/browser-host"),
            })
        );
    }

    #[test]
    fn a_missing_or_unknown_switch_is_refused_by_name() {
        for (command_line, named) in [
            (
                arguments(&["--state-directory", "/s"]),
                "--listen-socket is required",
            ),
            (
                arguments(&["--listen-socket", "/l"]),
                "--state-directory is required",
            ),
            (arguments(&["--listen-socket"]), "names a path"),
            (arguments(&["--profile", "x"]), "--profile is not a switch"),
        ] {
            let refusal = browser_host_command_line_of(command_line).unwrap_err();
            assert!(refusal.contains(named), "{refusal}");
        }
    }
}
