// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Minimal API Server Example
//!
//! Launches the API server processor and waits for shutdown signal (Ctrl+C).
//! Use this for testing the command and control web interface.
//!
//! The server runs on http://127.0.0.1:9000 with endpoints:
//! - GET /health - Health check
//! - GET /api/registry - List available processor types
//! - GET /api/graph - Get current runtime graph
//! - POST /api/processor - Create a processor
//! - DELETE /api/processors/:id - Remove a processor
//! - POST /api/connections - Create a connection
//! - DELETE /api/connections/:id - Remove a connection
//! - WS /ws/events - WebSocket event stream
//!
//! Packages build automatically on `cargo run` via the build orchestrator.
//! so the runtime can find the staged cdylib at
//! `target/streamlib-plugins/tatolab__api-server/`.
//!
//! Loads `@tatolab/api-server` through the imperative
//! [`Runner::add_module`] API. The fully spelled out
//! [`streamlib::sdk::module_ident!`] call is one of four
//! ergonomic shapes; the other three (split + any-version,
//! joined + version, joined + any-version) are equally valid and
//! resolve to the same `ModuleIdent::new(...)` expression at
//! expansion time.

use streamlib::sdk::module_ident;
use streamlib::sdk::processors::ProcessorSpec;
use streamlib::sdk::runtime::Runner;
use streamlib::sdk::schema_ident;

#[tokio::main]
async fn main() -> streamlib::sdk::error::Result<()> {
    let runtime = Runner::new_with_orchestrator(streamlib::sdk::PolyglotBuildOrchestrator::default())?;

    // Imperative module load — the runtime resolves the ident
    // from its package source (built on demand by the orchestrator),
    // verifies the semver range, then drives the internal
    // module-loading machinery.
    runtime.add_module_with(module_ident!("tatolab", "api-server", "^1.0.0"), streamlib::sdk::runtime::Strategy::Path { path: std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../packages/api-server"), build: streamlib::sdk::runtime::BuildPolicy::IfStale }).await?;

    let config = serde_json::json!({
        "host": "127.0.0.1",
        "port": 9000,
    });
    runtime.add_processor(ProcessorSpec::new(
        schema_ident!("tatolab", "api-server", "ApiServer", "1.0.0"),
        config,
    ))?;
    runtime.start()?;

    println!("API server running at http://127.0.0.1:9000");
    println!("WebSocket events at ws://127.0.0.1:9000/ws/events");
    println!("Press Ctrl+C to stop");

    runtime.wait_for_signal()?;

    Ok(())
}

#[cfg(test)]
mod tests {
    //! Compile-checks for the four `module_ident*!` macro shapes —
    //! every shape must expand to a valid `ModuleIdent` against the
    //! same `streamlib::sdk::descriptors::*` paths. The runtime
    //! resolution of these idents is exercised in the engine's
    //! `add_module_tests` module against fixture-staged packages.
    use streamlib::sdk::descriptors::{ModuleIdent, SemVerRange};
    use streamlib::sdk::{
        module_ident, module_ident_any_version, module_ident_joined,
        module_ident_joined_any_version,
    };

    #[test]
    fn module_ident_split_with_version_round_trips() {
        let id: ModuleIdent = module_ident!("tatolab", "api-server", "^1.0.0");
        assert_eq!(id.to_string(), "@tatolab/api-server@^1.0.0");
    }

    #[test]
    fn module_ident_split_any_version_emits_star_range() {
        let id: ModuleIdent = module_ident_any_version!("tatolab", "api-server");
        assert_eq!(id.to_string(), "@tatolab/api-server@*");
        assert_eq!(id.version, SemVerRange::Any);
    }

    #[test]
    fn module_ident_joined_with_version_round_trips() {
        let id: ModuleIdent = module_ident_joined!("@tatolab/api-server", "~1.0.0");
        assert_eq!(id.to_string(), "@tatolab/api-server@~1.0.0");
    }

    #[test]
    fn module_ident_joined_any_version_emits_star_range() {
        let id: ModuleIdent = module_ident_joined_any_version!("@tatolab/api-server");
        assert_eq!(id.to_string(), "@tatolab/api-server@*");
        assert_eq!(id.version, SemVerRange::Any);
    }
}
