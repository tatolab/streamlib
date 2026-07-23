// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `streamlib nodes` — discover running StreamLib control planes.
//!
//! Scans the node registry
//! ([`streamlib_api_server::node_registry`], written by every
//! ApiServer-hosting runtime at `$XDG_RUNTIME_DIR/streamlib/nodes/`),
//! liveness-checks each entry, prunes the ones that are definitively gone, and
//! prints a table. Only runtimes that host an `@tatolab/api-server/ApiServer`
//! processor write an entry, so a runtime with no control endpoint never
//! appears here — its absence is expected, not a missing node.
//!
//! Liveness has two independent signals: a `graph` JSON-RPC round-trip to the
//! entry's `control_url` (the authoritative "usable via control plane" signal),
//! and — on unix — whether the host pid still exists. An entry is pruned from
//! disk only when BOTH say dead (unreachable AND no such process), so a live
//! process that is briefly slow to answer is never deleted. The `alive?` column
//! reflects control-plane reachability, since that is what a control verb needs.

use std::io::Write;
use std::time::Duration;

use anyhow::Result;
use serde_json::json;
use streamlib_api_server::node_registry::{self, NodeRegistryEntry};

/// A discovered registry entry paired with its liveness verdict.
pub struct DiscoveredNode {
    pub entry: NodeRegistryEntry,
    /// The control plane answered a `graph` round-trip.
    pub reachable: bool,
}

/// Scan the registry, liveness-check every entry, prune the entries that are
/// definitively gone (unreachable AND no live pid), and return each surviving
/// entry with its reachability verdict. Dead-but-not-yet-pruned entries (pid
/// alive, control plane silent) are returned with `reachable == false`.
pub fn scan_check_and_prune() -> Result<Vec<DiscoveredNode>> {
    let entries = node_registry::scan_entries()?;
    let mut discovered = Vec::with_capacity(entries.len());
    for entry in entries {
        let reachable = control_url_reachable(&entry.control_url);
        if !reachable && !pid_alive(entry.pid) {
            if let Err(error) = node_registry::remove_entry(&entry.runtime_id) {
                tracing::warn!(
                    %error,
                    runtime_id = %entry.runtime_id,
                    "failed to prune stale node registry entry"
                );
            }
            continue;
        }
        discovered.push(DiscoveredNode { entry, reachable });
    }
    Ok(discovered)
}

/// The reachable nodes only, with dead entries already pruned. Shared with the
/// control-verb URL resolver ([`super::control::resolve_control_url`]).
pub fn live_nodes() -> Result<Vec<NodeRegistryEntry>> {
    Ok(scan_check_and_prune()?
        .into_iter()
        .filter(|node| node.reachable)
        .map(|node| node.entry)
        .collect())
}

/// `streamlib nodes`: print the discovered control planes as a table.
pub fn run() -> Result<()> {
    let stdout = std::io::stdout();
    print_nodes(&scan_check_and_prune()?, &mut stdout.lock())
}

/// Render `nodes` as an aligned table to `writer`. Generic over the writer so a
/// test captures the output.
fn print_nodes(nodes: &[DiscoveredNode], writer: &mut impl Write) -> Result<()> {
    if nodes.is_empty() {
        writeln!(
            writer,
            "No running nodes found in {}.",
            node_registry::registry_dir().display()
        )?;
        writeln!(
            writer,
            "(Only runtimes hosting an ApiServer control plane appear here.)"
        )?;
        return Ok(());
    }

    let runtime_id_width = nodes
        .iter()
        .map(|node| node.entry.runtime_id.len())
        .chain(std::iter::once("RUNTIME_ID".len()))
        .max()
        .unwrap_or(0);
    let control_url_width = nodes
        .iter()
        .map(|node| node.entry.control_url.len())
        .chain(std::iter::once("CONTROL_URL".len()))
        .max()
        .unwrap_or(0);

    writeln!(
        writer,
        "{:<rid$}  {:<url$}  {:>7}  {:<6}  {}",
        "RUNTIME_ID",
        "CONTROL_URL",
        "PID",
        "ALIVE?",
        "HINT",
        rid = runtime_id_width,
        url = control_url_width,
    )?;
    for node in nodes {
        writeln!(
            writer,
            "{:<rid$}  {:<url$}  {:>7}  {:<6}  {}",
            node.entry.runtime_id,
            node.entry.control_url,
            node.entry.pid,
            if node.reachable { "yes" } else { "no" },
            node.entry.hint,
            rid = runtime_id_width,
            url = control_url_width,
        )?;
    }
    writeln!(
        writer,
        "\nOnly runtimes hosting an ApiServer control plane appear here; a runtime \
         without a control endpoint is not listed (and is not missing)."
    )?;
    Ok(())
}

/// Whether the control plane at `control_url` answers a `graph` round-trip.
/// Any HTTP response — including an auth `401` — counts as reachable (the server
/// is up); only a transport error (connection refused, timeout) is dead. A short
/// timeout keeps a hung reused port from stalling the scan. `STREAMLIB_MCP_TOKEN`
/// rides as a bearer token when set, matching the control verbs.
fn control_url_reachable(control_url: &str) -> bool {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_millis(500))
        .timeout(Duration::from_millis(1500))
        .build();
    let endpoint = format!("{}/mcp", control_url.trim_end_matches('/'));
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": { "name": "graph", "arguments": {} },
    })
    .to_string();
    let mut request = agent
        .post(&endpoint)
        .set("content-type", "application/json");
    if let Ok(token) = std::env::var("STREAMLIB_MCP_TOKEN") {
        request = request.set("authorization", &format!("Bearer {token}"));
    }
    match request.send_string(&body) {
        Ok(_) => true,
        Err(ureq::Error::Status(_, _)) => true,
        Err(_) => false,
    }
}

/// Whether a process with `pid` currently exists.
#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    // `kill(pid, 0)` performs no signal delivery, only permission/existence
    // checks: 0 → exists; EPERM → exists but not signalable by us; ESRCH → gone.
    let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if rc == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Non-unix has no `kill(pid, 0)`; reachability alone decides pruning there.
#[cfg(not(unix))]
fn pid_alive(_pid: u32) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(runtime_id: &str, control_url: &str, pid: u32) -> NodeRegistryEntry {
        NodeRegistryEntry {
            schema_version: node_registry::NODE_REGISTRY_SCHEMA_VERSION,
            runtime_id: runtime_id.to_string(),
            control_url: control_url.to_string(),
            pid,
            hint: "streamlib (/tmp/app)".to_string(),
        }
    }

    #[test]
    fn print_nodes_renders_the_columns_and_footer() {
        let nodes = vec![DiscoveredNode {
            entry: entry("Rabc", "http://127.0.0.1:8080", 1234),
            reachable: true,
        }];
        let mut out = Vec::new();
        print_nodes(&nodes, &mut out).expect("print");
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("RUNTIME_ID"));
        assert!(text.contains("Rabc"));
        assert!(text.contains("http://127.0.0.1:8080"));
        assert!(text.contains("1234"));
        assert!(text.contains("yes"));
        assert!(
            text.contains("ApiServer control plane"),
            "footer must note the ApiServer-only invariant: {text}"
        );
    }

    #[test]
    fn print_nodes_empty_notes_the_apiserver_only_invariant() {
        let mut out = Vec::new();
        print_nodes(&[], &mut out).expect("print");
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("No running nodes"));
        assert!(text.contains("ApiServer control plane"));
    }

    #[test]
    fn pid_alive_is_true_for_this_process_and_false_for_a_reserved_dead_pid() {
        assert!(pid_alive(std::process::id()));
        // pid 0 is never a normal user process to signal; kill(0, 0) targets the
        // process group, so probe an unreachably-high pid that cannot exist.
        assert!(!pid_alive(u32::MAX - 1));
    }

    #[test]
    fn unreachable_control_url_is_not_reachable() {
        // Bind then drop a listener to obtain a port guaranteed closed now.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        assert!(!control_url_reachable(&format!("http://127.0.0.1:{port}")));
    }
}
