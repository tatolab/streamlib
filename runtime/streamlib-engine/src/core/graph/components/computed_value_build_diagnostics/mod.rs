// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The diagnostic a computed value of a node written as one function gives its node while it has
//! no first value yet or its latest build failed, and the health that diagnostic gives the node:
//! read from the node's own scope for a node in this process, and from what its helper reports
//! for a node in a helper process.

use std::sync::Arc;

use parking_lot::Mutex;
use streamlib_runtime_client_contract::local_api_wire_contract::{
    DiagnosticKindOnTheWire, NodeHealthOnTheWire,
};
use streamlib_scope::{BuildFailure, BuildsOfAComputedValue, BuildsOfAScope, LastBuild};

/// How the builds of one computed value stand, as its node's health words them: the value named
/// as its declaration names it, which a helper's report carries as words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BuildsOfAComputedValueShown {
    /// The computed value, as its node's diagnostics name it.
    pub(crate) declared: String,
    /// Whether its first build landed.
    pub(crate) has_a_value: bool,
    /// Why its latest build failed; `None` while none did.
    pub(crate) latest_build_failed: Option<BuildFailure>,
    /// Whether it is left to build again.
    pub(crate) building_again: bool,
}

impl From<BuildsOfAComputedValue> for BuildsOfAComputedValueShown {
    fn from(builds: BuildsOfAComputedValue) -> Self {
        Self {
            declared: builds.declared.to_string(),
            has_a_value: builds.has_a_value,
            latest_build_failed: match builds.last_build {
                LastBuild::Failed { why, .. } => Some(why),
                LastBuild::NotYetBuilt | LastBuild::Built { .. } => None,
            },
            building_again: builds.building_again,
        }
    }
}

/// How the builds of each computed value of a node in a helper process stand, as its helper last
/// reported them; none before its first report and once its helper is replaced.
#[derive(Debug, Default)]
pub(crate) struct ComputedValueBuildsItsHelperReports {
    of_each: Mutex<Vec<BuildsOfAComputedValueShown>>,
}

impl ComputedValueBuildsItsHelperReports {
    /// Keep `of_each`, in declaration order, in place of the report before.
    pub(crate) fn record(&self, of_each: Vec<BuildsOfAComputedValueShown>) {
        *self.of_each.lock() = of_each;
    }

    fn of_each_computed_value(&self) -> Vec<BuildsOfAComputedValueShown> {
        self.of_each.lock().clone()
    }
}

/// Where a node's health reads how the builds of its computed values stand.
#[derive(Clone)]
pub(crate) enum BuildsOfItsComputedValues {
    /// The scope of a node running in this process.
    OfItsScope(BuildsOfAScope),
    /// What the helper process the node runs in reports.
    AsItsHelperReports(Arc<ComputedValueBuildsItsHelperReports>),
}

impl BuildsOfItsComputedValues {
    /// How the builds of each computed value stand, in declaration order.
    pub(crate) fn of_each_computed_value(&self) -> Vec<BuildsOfAComputedValueShown> {
        match self {
            Self::OfItsScope(builds) => builds
                .of_each_computed_value()
                .into_iter()
                .map(BuildsOfAComputedValueShown::from)
                .collect(),
            Self::AsItsHelperReports(reports) => reports.of_each_computed_value(),
        }
    }
}

/// The health each computed value of `builds`, a node's, whose builds leave something to report
/// gives its node, and the diagnostic kind and words: not ready while it has no first value and
/// none of its builds failed, raising while its latest build failed and no build runs again.
/// A value whose build waits on another's first value reports nothing, since that one reports.
pub(super) fn the_diagnostics_of_the_builds_of(
    builds: &[BuildsOfAComputedValueShown],
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
    builds: &BuildsOfAComputedValueShown,
    meanwhile: &str,
) -> Option<(NodeHealthOnTheWire, (DiagnosticKindOnTheWire, String))> {
    let declared = &builds.declared;
    let building_its_first_value = || {
        (
            NodeHealthOnTheWire::NotReady,
            (
                DiagnosticKindOnTheWire::ComputedValueNotYetBuilt,
                format!("its computed value {declared} is building its first value; {meanwhile}"),
            ),
        )
    };
    let Some(why) = &builds.latest_build_failed else {
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
