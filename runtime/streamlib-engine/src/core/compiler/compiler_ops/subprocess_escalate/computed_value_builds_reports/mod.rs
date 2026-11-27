// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A helper-placed node function reporting how the builds of its computed values stand: held
//! from the helper's report, which the node's health reads, until the next report replaces it.

use streamlib_scope::BuildFailure;

use super::port_stream_facts::WhatAHelperPlacedNodeReaches;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::EscalateResponse;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_request::{
    ComputedValueBuildFailureOnTheEscalateWire, ComputedValueBuildsOnTheEscalateWire,
    EscalateRequestSetComputedValueBuilds,
};
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_response::EscalateResponseOk;
use crate::core::graph::BuildsOfAComputedValueShown;

impl From<ComputedValueBuildFailureOnTheEscalateWire> for BuildFailure {
    fn from(failed: ComputedValueBuildFailureOnTheEscalateWire) -> Self {
        match failed {
            ComputedValueBuildFailureOnTheEscalateWire::Raised { why } => {
                BuildFailure::Returned(why)
            }
            ComputedValueBuildFailureOnTheEscalateWire::ReadAComputedValueNotYetBuilt => {
                BuildFailure::ReadAComputedValueNotYetBuilt
            }
            ComputedValueBuildFailureOnTheEscalateWire::Panicked { why } => {
                BuildFailure::Panicked(why)
            }
            ComputedValueBuildFailureOnTheEscalateWire::ComparisonPanicked { why } => {
                BuildFailure::ComparisonPanicked(why)
            }
        }
    }
}

impl From<ComputedValueBuildsOnTheEscalateWire> for BuildsOfAComputedValueShown {
    fn from(builds: ComputedValueBuildsOnTheEscalateWire) -> Self {
        Self {
            declared: builds.declared,
            has_a_value: builds.has_a_value,
            latest_build_failed: builds.latest_build_failed.map(BuildFailure::from),
            building_again: builds.building_again,
        }
    }
}

/// Keep how the builds of each computed value stand, in place of the report before.
pub(super) fn handle_set_computed_value_builds(
    what_the_node_reaches: &WhatAHelperPlacedNodeReaches,
    rid: String,
    request: EscalateRequestSetComputedValueBuilds,
) -> EscalateResponse {
    what_the_node_reaches
        .computed_value_builds_its_helper_reports
        .record(
            request
                .builds
                .into_iter()
                .map(BuildsOfAComputedValueShown::from)
                .collect(),
        );
    EscalateResponse::Ok(EscalateResponseOk {
        request_id: rid,
        ..Default::default()
    })
}

#[cfg(test)]
mod tests;
