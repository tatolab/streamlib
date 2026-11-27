// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The diagnostic a computed value of a node written as one function gives its node while it has
//! no first value yet or its latest build failed, and the health that diagnostic gives the node.

use streamlib_runtime_client_contract::local_api_wire_contract::{
    DiagnosticKindOnTheWire, NodeHealthOnTheWire,
};
use streamlib_scope::{BuildFailure, BuildsOfAComputedValue, LastBuild};

/// The health each computed value of `builds`, a node's, whose builds leave something to report
/// gives its node, and the diagnostic kind and words: not ready while it has no first value and
/// none of its builds failed, raising while its latest build failed and no build runs again.
/// A value whose build waits on another's first value reports nothing, since that one reports.
pub(super) fn the_diagnostics_of_the_builds_of(
    builds: &[BuildsOfAComputedValue],
) -> Vec<(NodeHealthOnTheWire, (DiagnosticKindOnTheWire, String))> {
    let meanwhile = if builds.iter().all(|builds| builds.has_a_value) {
        "the node runs on its previous value"
    } else {
        "the node runs no frame until every computed value has one"
    };
    builds
        .iter()
        .filter_map(|builds| the_diagnostic_of_the_builds_of(builds, meanwhile))
        .collect()
}

fn the_diagnostic_of_the_builds_of(
    builds: &BuildsOfAComputedValue,
    meanwhile: &str,
) -> Option<(NodeHealthOnTheWire, (DiagnosticKindOnTheWire, String))> {
    let declared = builds.declared;
    let building_its_first_value = || {
        (
            NodeHealthOnTheWire::NotReady,
            (
                DiagnosticKindOnTheWire::ComputedValueNotYetBuilt,
                format!("its computed value {declared} is building its first value; {meanwhile}"),
            ),
        )
    };
    let LastBuild::Failed { why, .. } = &builds.last_build else {
        return (!builds.has_a_value).then(building_its_first_value);
    };
    if builds.building_again {
        return (!builds.has_a_value).then(building_its_first_value);
    }
    let failed = match why {
        BuildFailure::ReadAComputedValueNotYetBuilt => return None,
        BuildFailure::Returned(error) => (
            DiagnosticKindOnTheWire::ComputedValueBuildFailed,
            format!("the build of its computed value {declared} failed: {error}; {meanwhile}"),
        ),
        BuildFailure::Panicked(message) => (
            DiagnosticKindOnTheWire::ComputedValueBuildPanicked,
            format!("the build of its computed value {declared} panicked: {message}; {meanwhile}"),
        ),
        BuildFailure::ComparisonPanicked(message) => (
            DiagnosticKindOnTheWire::ComputedValueComparisonPanicked,
            format!(
                "comparing a new build of its computed value {declared} with its previous value \
                 panicked: {message}; {meanwhile}"
            ),
        ),
    };
    Some((NodeHealthOnTheWire::Raising, failed))
}

#[cfg(test)]
mod tests;
