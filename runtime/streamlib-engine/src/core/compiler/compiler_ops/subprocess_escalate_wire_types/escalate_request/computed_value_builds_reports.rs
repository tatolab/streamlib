// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde::{Deserialize, Serialize};

/// How the builds of each computed value of the helper's node function stand, in declaration
/// order, sent whenever that changes: the graph shows a build still running or failed on the
/// node, as it does for a node function in the host.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EscalateRequestSetComputedValueBuilds {
    /// Each computed value's builds, in the order the node's body declared them.
    pub(crate) builds: Vec<ComputedValueBuildsOnTheEscalateWire>,

    /// Correlates request with response. UUID string.
    pub(crate) request_id: String,
}

/// How the builds of one computed value stand.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ComputedValueBuildsOnTheEscalateWire {
    /// The computed value, as the node's diagnostics name it: its build function and where it
    /// is declared.
    pub(crate) declared: String,
    /// Whether its first build landed.
    pub(crate) has_a_value: bool,
    /// Whether it is left to build again.
    pub(crate) building_again: bool,
    /// Why its latest build failed; `null` while none did.
    pub(crate) latest_build_failed: Option<ComputedValueBuildFailureOnTheEscalateWire>,
}

/// Why a computed value's latest build failed, each failure its own kind.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum ComputedValueBuildFailureOnTheEscalateWire {
    /// The build raised; `why` is the exception as Python words it.
    Raised { why: String },
    /// The build read a computed value that has no value yet.
    ReadAComputedValueNotYetBuilt,
    /// The build panicked in the wheel.
    Panicked { why: String },
    /// Comparing the built value with the one it would replace panicked.
    ComparisonPanicked { why: String },
}
