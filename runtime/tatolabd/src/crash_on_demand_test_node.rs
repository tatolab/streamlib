// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A node that crashes the runtime on demand, for the tests of the failed
//! state. Its config's check — which every load of a stream naming it runs,
//! before any GPU exists — crashes the process while the file the config
//! names exists. Compiled into a test build alone.

use std::path::PathBuf;

use serde::{Deserialize, Deserializer, Serialize};
use streamlib::sdk::context::RuntimeContextFullAccess;
use streamlib::sdk::error::Result;
use streamlib::sdk::processors::{ManualProcessor, PROCESSOR_REGISTRY};
use streamlib::sdk::schemars::JsonSchema;

/// How the node crashes the runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[schemars(crate = "streamlib::sdk::schemars")]
pub enum CrashOnDemandSignal {
    /// `raise(SIGSEGV)`.
    #[serde(rename = "SIGSEGV")]
    RaiseSegmentationFault,
    /// `abort()`.
    #[serde(rename = "SIGABRT")]
    Abort,
}

/// The node's config: what file arms it and how it crashes.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[schemars(crate = "streamlib::sdk::schemars")]
pub struct CrashOnDemandTestNodeConfig {
    /// While this file exists, the check of this config crashes the runtime.
    pub crash_while_this_file_exists: PathBuf,
    /// The signal the runtime crashes on.
    pub crash_with: CrashOnDemandSignal,
}

impl Default for CrashOnDemandTestNodeConfig {
    fn default() -> Self {
        Self {
            crash_while_this_file_exists: PathBuf::new(),
            crash_with: CrashOnDemandSignal::RaiseSegmentationFault,
        }
    }
}

impl<'de> Deserialize<'de> for CrashOnDemandTestNodeConfig {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct CrashOnDemandTestNodeConfigAsWritten {
            crash_while_this_file_exists: PathBuf,
            crash_with: CrashOnDemandSignal,
        }
        let as_written = CrashOnDemandTestNodeConfigAsWritten::deserialize(deserializer)?;
        if as_written.crash_while_this_file_exists.exists() {
            tracing::error!(
                "the crash-on-demand test node is armed by {}, and crashes the runtime with {:?}",
                as_written.crash_while_this_file_exists.display(),
                as_written.crash_with
            );
            match as_written.crash_with {
                // SAFETY: `raise` takes a signal number and returns once it is handled.
                CrashOnDemandSignal::RaiseSegmentationFault => unsafe {
                    libc::raise(libc::SIGSEGV);
                },
                CrashOnDemandSignal::Abort => std::process::abort(),
            }
        }
        Ok(Self {
            crash_while_this_file_exists: as_written.crash_while_this_file_exists,
            crash_with: as_written.crash_with,
        })
    }
}

#[streamlib::sdk::processor(
    execution = manual,
    config = crate::crash_on_demand_test_node::CrashOnDemandTestNodeConfig,
)]
pub struct CrashOnDemandTestNode;

impl ManualProcessor for CrashOnDemandTestNode::Processor {
    fn start(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }
}

/// Register the node on the process-wide registry.
pub(crate) fn register_the_crash_on_demand_test_node() {
    PROCESSOR_REGISTRY.register::<CrashOnDemandTestNode::Processor>();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_test_graph_names_the_node_by_its_class_import_path() {
        assert_eq!(
            CrashOnDemandTestNode::processor_class_import_path().as_str(),
            "tatolabd::crash_on_demand_test_node::CrashOnDemandTestNode"
        );
    }

    #[test]
    fn a_config_whose_file_is_absent_is_taken_without_a_crash() {
        let config: CrashOnDemandTestNodeConfig = serde_json::from_value(serde_json::json!({
            "crash_while_this_file_exists": "/no/such/crash/trigger",
            "crash_with": "SIGSEGV",
        }))
        .expect("a config whose file is absent is taken");

        assert_eq!(
            config.crash_with,
            CrashOnDemandSignal::RaiseSegmentationFault
        );
    }
}
