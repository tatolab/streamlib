// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The diagnostic a computed value of a node written as one function gives its node while it has
//! no first value yet or its latest build failed, and the health that diagnostic gives the node.

use streamlib_runtime_client_contract::local_api_wire_contract::{
    DiagnosticKindOnTheWire, NodeHealthOnTheWire,
};
use streamlib_scope::{BuildFailure, BuildsOfAComputedValue, LastBuild};

/// The health a computed value whose builds leave something to report gives its node, and the
/// diagnostic kind and words: not ready with no first value yet, raising with a latest build
/// that failed; `None` while its latest build landed.
pub(super) fn the_diagnostic_of_the_builds_of(
    builds: &BuildsOfAComputedValue,
) -> Option<(NodeHealthOnTheWire, (DiagnosticKindOnTheWire, String))> {
    let declared = builds.declared;
    let meanwhile = if builds.has_a_value {
        "the node runs on its previous value"
    } else {
        "the node runs no frame until it has one"
    };
    let LastBuild::Failed { why, .. } = &builds.last_build else {
        return (!builds.has_a_value).then(|| {
            (
                NodeHealthOnTheWire::NotReady,
                (
                    DiagnosticKindOnTheWire::ComputedValueNotYetBuilt,
                    format!(
                        "its computed value {declared} is building its first value; {meanwhile}"
                    ),
                ),
            )
        });
    };
    if *why == BuildFailure::ReadAComputedValueNotYetBuilt {
        return Some((
            NodeHealthOnTheWire::NotReady,
            (
                DiagnosticKindOnTheWire::ComputedValueNotYetBuilt,
                format!(
                    "its computed value {declared} waits on another computed value's first \
                     value; {meanwhile}"
                ),
            ),
        ));
    }
    let failed = match why {
        BuildFailure::ReadAComputedValueNotYetBuilt => unreachable!("reported as not yet built"),
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
