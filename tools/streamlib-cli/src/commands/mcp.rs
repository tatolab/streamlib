// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `streamlib mcp` — speak the Model Context Protocol over stdio so any MCP host
//! (`claude mcp add streamlib -- streamlib mcp`) can spawn StreamLib's
//! agent-operable tools with zero port/daemon juggling.
//!
//! Two modes, both hosting the api-server's one transport-free MCP dispatch
//! ([`streamlib_api_server::serve_stdio_jsonrpc`]) — never a parallel MCP impl:
//!
//! - **Default (in-process):** build a fresh live [`Runner`] and serve its MCP
//!   over the process's stdio; the host gets an operable runtime with no setup.
//!   Torn down when the host closes stdin (EOF).
//! - **`--attach <url>`:** forward each stdio JSON-RPC line to a running
//!   runtime's `POST /mcp`, to operate an existing live pipeline; no local
//!   Runner is built.
//!
//! Auth is a no-op on the in-process path by construction (a local child
//! process, no bearer header); only `--attach` may forward a token
//! (`STREAMLIB_MCP_TOKEN`) to the remote endpoint.

use std::sync::Arc;

use anyhow::Result;
use streamlib::sdk::RunnerAutoBuild;
use streamlib::sdk::runtime::{Runner, RuntimeOperations};

/// Run the `mcp` subcommand. `attach` selects the bridge-to-remote mode; its
/// absence is the in-process default.
pub async fn run(attach: Option<String>) -> Result<()> {
    match attach {
        Some(url) => attach_to_remote(url).await,
        None => serve_in_process().await,
    }
}

/// Build a live in-process runtime and serve MCP over stdio against it. The
/// runtime is started before the loop and stopped on stdin EOF (the host
/// closing the pipe). Needs the runtime rig (GPU/iceoryx2) — an MCP host spawns
/// this in the user's environment, so it has the rig.
async fn serve_in_process() -> Result<()> {
    let runner = Runner::with_auto_build()?;
    runner.start()?;

    let runtime: Arc<dyn RuntimeOperations> = runner.clone();
    let served = streamlib_api_server::serve_stdio_jsonrpc(
        runtime,
        tokio::io::BufReader::new(tokio::io::stdin()),
        tokio::io::stdout(),
    )
    .await;

    // Tear the runtime down regardless of how the loop ended, then surface any
    // transport error.
    if let Err(stop_error) = runner.stop() {
        tracing::warn!("runtime stop after MCP stdio EOF failed: {stop_error}");
    }
    served?;
    Ok(())
}

/// Bridge stdio ↔ a running runtime's `POST /mcp`: each inbound JSON-RPC line is
/// POSTed to the remote endpoint and the response body (if any) written back as
/// a line. Runs the whole blocking bridge on a blocking thread so the tokio
/// runtime is never parked on `ureq` I/O.
async fn attach_to_remote(url: String) -> Result<()> {
    tokio::task::spawn_blocking(move || attach_to_remote_blocking(&url)).await?
}

fn attach_to_remote_blocking(url: &str) -> Result<()> {
    use std::io::{BufRead, Write};

    let endpoint = format!("{}/mcp", url.trim_end_matches('/'));
    let bearer_token = std::env::var("STREAMLIB_MCP_TOKEN").ok();

    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();

    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let mut request = ureq::post(&endpoint).set("content-type", "application/json");
        if let Some(bearer_token) = &bearer_token {
            request = request.set("authorization", &format!("Bearer {bearer_token}"));
        }
        match request.send_string(&line) {
            // 2xx: a request answers `200` with the JSON-RPC envelope; a
            // notification answers `202` with an empty body → no response line.
            Ok(response) => {
                let body = response.into_string()?;
                if !body.trim().is_empty() {
                    writeln!(stdout, "{body}")?;
                    stdout.flush()?;
                }
            }
            Err(ureq::Error::Status(code, response)) => {
                let body = response.into_string().unwrap_or_default();
                anyhow::bail!("attach POST {endpoint} failed: HTTP {code}: {body}");
            }
            Err(error) => anyhow::bail!("attach POST {endpoint} transport error: {error}"),
        }
    }
    Ok(())
}
