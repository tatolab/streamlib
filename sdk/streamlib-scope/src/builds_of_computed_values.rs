// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! How the builds of a scope's computed values went, as a node's diagnostics read them.

use std::fmt;
use std::panic::Location;
use std::sync::Arc;

use super::lock;
use super::sync::Mutex;

/// Why the last build of a computed value failed. The previous value keeps serving.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BuildFailure {
    /// The build returned this error.
    Returned(String),
    /// The build read a computed value that has no value yet; it builds again once that value
    /// is built.
    ReadAComputedValueNotYetBuilt,
    /// The build panicked with this message.
    Panicked(String),
    /// Comparing the built value with the one it would replace panicked with this message; the
    /// previous value keeps serving.
    ComparisonPanicked(String),
}

impl fmt::Display for BuildFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BuildFailure::Returned(why) => f.write_str(why),
            BuildFailure::ReadAComputedValueNotYetBuilt => {
                f.write_str("it reads a computed value that is not built yet")
            }
            BuildFailure::Panicked(message) => write!(f, "it panicked: {message}"),
            BuildFailure::ComparisonPanicked(message) => {
                write!(
                    f,
                    "comparing its value with the previous one panicked: {message}"
                )
            }
        }
    }
}

/// How the last build of a computed value went, for a node's diagnostics.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LastBuild {
    NotYetBuilt,
    Built { builds: u64 },
    Failed { builds: u64, why: BuildFailure },
}

impl LastBuild {
    pub(crate) fn builds(&self) -> u64 {
        match self {
            LastBuild::NotYetBuilt => 0,
            LastBuild::Built { builds } | LastBuild::Failed { builds, .. } => *builds,
        }
    }
}

/// A computed value as its node's diagnostics name it: the type it holds and where it is declared.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ComputedValueDeclared {
    /// The type the value holds, as [`std::any::type_name`] names it.
    pub holding: &'static str,
    /// Where its [`computed`] is called.
    pub at: &'static Location<'static>,
}

impl fmt::Display for ComputedValueDeclared {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "`{}` declared at {}", self.holding, self.at)
    }
}

/// How the builds of one computed value stand, for its node's diagnostics.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuildsOfAComputedValue {
    /// The type it holds and where it is declared.
    pub declared: ComputedValueDeclared,
    /// Whether it holds a value: its first build landed.
    pub has_a_value: bool,
    /// How its latest build went.
    pub last_build: LastBuild,
    /// Whether it is left to build again: it has no first value, or an input it read changed
    /// since its latest build.
    pub building_again: bool,
}

/// A reader of how the builds of each computed value of a scope stand. It holds the report
/// alone, never a value or a build, so it keeps nothing of the scope alive; once the scope
/// closes it reads none.
#[derive(Clone)]
pub struct BuildsOfAScope {
    pub(crate) reported: Arc<Mutex<BuildsReported>>,
}

/// How the builds of each computed value stand, kept apart from the slots so a reader of it
/// holds no value.
pub(crate) struct BuildsReported {
    /// Set as the scope closes.
    pub(crate) closed: bool,
    /// Each computed value's builds, by the slot it is declared in, in declaration order.
    pub(crate) of_each: Vec<(usize, BuildsOfAComputedValue)>,
}

impl BuildsOfAScope {
    /// How the builds of each computed value stand, in declaration order; none once the scope
    /// closes.
    #[must_use]
    pub fn of_each_computed_value(&self) -> Vec<BuildsOfAComputedValue> {
        let reported = lock(&self.reported);
        if reported.closed {
            return Vec::new();
        }
        reported
            .of_each
            .iter()
            .map(|(_, builds)| builds.clone())
            .collect()
    }
}
