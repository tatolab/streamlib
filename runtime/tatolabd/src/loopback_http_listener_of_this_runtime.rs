// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The runtime's loopback HTTP, served beside its local API on `127.0.0.1` at
//! [`streamlib_api_server::LOOPBACK_HTTP_LISTENER_DEFAULT_PORT`], its preview
//! page read from beside the lend.

use std::path::Path;
use std::sync::Arc;

use streamlib::sdk::processor_interpreter::watch_page_directory_beside_the_lend;
use streamlib::sdk::runtime::{OperationsOnTheStreamsLoadedInThisRuntime, Runner};
use streamlib_api_server::{
    LoopbackHttpListenerServedForAnEngine, serve_the_loopback_http_listener,
};

/// Serve `engine`'s loopback HTTP until the returned holder is dropped, the
/// preview page read from beside `lend_directory`. A port another process
/// holds leaves the runtime running without it, said once.
pub(crate) fn serve_the_loopback_http_listener_of_this_runtime(
    engine: &Arc<Runner>,
    lend_directory: &Path,
) -> Option<LoopbackHttpListenerServedForAnEngine> {
    let port = match the_loopback_http_port() {
        Ok(port) => port,
        Err(why_not) => {
            tracing::info!("no loopback HTTP is served: {why_not}");
            return None;
        }
    };
    let Some(watch_page_directory) = watch_page_directory_beside_the_lend(lend_directory) else {
        tracing::warn!(
            "no loopback HTTP is served: the lend at {} has no directory beside it for the \
             preview page",
            lend_directory.display()
        );
        return None;
    };
    match serve_the_loopback_http_listener(
        Arc::clone(engine) as Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>,
        port,
        watch_page_directory,
        &engine.tokio_handle(),
    ) {
        Ok(served) => Some(served),
        Err(refusal) => {
            tracing::warn!(
                "no loopback HTTP is served, so neither the listing of this machine's streams \
                 nor the preview page answers: {refusal}"
            );
            None
        }
    }
}

/// The port the loopback HTTP listener binds: its default.
#[cfg(not(feature = "machine-directories-under-a-test-root"))]
fn the_loopback_http_port() -> Result<u16, String> {
    Ok(streamlib_api_server::LOOPBACK_HTTP_LISTENER_DEFAULT_PORT)
}

/// The port the test names, or why none is served: a test build never binds
/// the machine's own port, so one whose test names no port serves no loopback
/// HTTP.
#[cfg(feature = "machine-directories-under-a-test-root")]
fn the_loopback_http_port() -> Result<u16, String> {
    use streamlib_runtime_client_contract::machine_directories_test_root::TEST_LOOPBACK_HTTP_PORT_ENVIRONMENT_VARIABLE;
    std::env::var(TEST_LOOPBACK_HTTP_PORT_ENVIRONMENT_VARIABLE)
        .ok()
        .and_then(|port| port.parse().ok())
        .ok_or_else(|| {
            format!(
                "this test build names no port for it in \
                 {TEST_LOOPBACK_HTTP_PORT_ENVIRONMENT_VARIABLE}, and never binds the machine's own"
            )
        })
}
