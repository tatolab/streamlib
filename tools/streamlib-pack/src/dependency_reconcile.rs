// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Build-time dependency reconciliation.
//!
//! A package's dependency identity is derivable from what its code references
//! — the owning `@org/package` of every schema it imports (declared in the
//! manifest's `schemas:` map as `External { package }`) plus any fully-qualified
//! port schema id resolved on a processor. [`reconcile_package_dependencies`]
//! derives that referenced set and reconciles it against the hand-declared
//! `dependencies:` table:
//!
//! - **Undeclared** — a referenced package that is not declared. This is a hard
//!   error at `pkg build`: the manifest lies about what the package needs. The
//!   fix is to declare it (`streamlib add @org/name@<version>`).
//! - **Pruned** — a declared package referenced by none of the code, and not
//!   marked `runtime: true`. Reported as prunable (dead-weight dependency).
//! - **Retained** — every other declared package (referenced, or a deliberate
//!   `runtime: true` composition dependency).
//!
//! The referenced set is derived from the manifest — the `schemas:` map is the
//! language-uniform source of the bare-name → owning-package resolution the
//! catalog itself uses, and (unlike a per-language code scan) it carries the
//! `@org/package` a bare `schema: VideoFrame` port reference resolves to. The
//! committed `processors:` block is already pinned to code by the
//! processor-manifest drift gate, so deriving from the manifest is deriving
//! from code.

use std::collections::BTreeSet;

use streamlib_idents::{Manifest, PackageRef, SchemaEntry};
use streamlib_processor_schema::{PortSchemaSpec, ProcessorSchema};

/// The outcome of reconciling a package's declared `dependencies:` against the
/// dependency set derived from its code/schema references.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DependencyReconciliation {
    /// Referenced packages that are not declared — a hard error at build.
    pub undeclared: Vec<PackageRef>,
    /// Declared packages referenced by nothing and not marked `runtime: true`.
    pub pruned: Vec<PackageRef>,
    /// Declared packages that are kept — referenced, or `runtime: true`.
    pub retained: Vec<PackageRef>,
}

/// The `@org/package` set a package's code/schema references resolve to, minus
/// the package itself. Union of every `schemas: External { package }` import
/// and every fully-qualified [`PortSchemaSpec::Specific`] port id.
pub fn derive_referenced_packages(
    manifest: &Manifest,
    processors: &[ProcessorSchema],
) -> BTreeSet<PackageRef> {
    let mut referenced = BTreeSet::new();

    if let Some(schemas) = manifest.schemas.as_ref() {
        for entry in schemas.values() {
            if let SchemaEntry::External { package } = entry {
                referenced.insert(package.clone());
            }
        }
    }

    for proc in processors {
        for port in proc.inputs.iter().chain(proc.outputs.iter()) {
            if let PortSchemaSpec::Specific(ident) = &port.schema {
                referenced.insert(PackageRef::new(ident.org.clone(), ident.package.clone()));
            }
        }
    }

    if let Some(owner) = manifest.package_ref() {
        referenced.remove(&owner);
    }
    referenced
}

/// Reconcile a package's declared `dependencies:` against the set derived from
/// its code/schema references. See the module docs for the three outcomes.
pub fn reconcile_package_dependencies(
    manifest: &Manifest,
    processors: &[ProcessorSchema],
) -> DependencyReconciliation {
    let referenced = derive_referenced_packages(manifest, processors);
    let declared: BTreeSet<&PackageRef> = manifest.dependencies.keys().collect();

    let undeclared = referenced
        .iter()
        .filter(|pkg| !declared.contains(pkg))
        .cloned()
        .collect();

    let mut pruned = Vec::new();
    let mut retained = Vec::new();
    for (pkg, spec) in &manifest.dependencies {
        if referenced.contains(pkg) || spec.is_runtime() {
            retained.push(pkg.clone());
        } else {
            pruned.push(pkg.clone());
        }
    }

    DependencyReconciliation {
        undeclared,
        pruned,
        retained,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use streamlib_idents::{DependencySpec, VersionDependency, SemVerRange};

    fn manifest_from_yaml(yaml: &str) -> Manifest {
        serde_yaml::from_str(yaml).expect("manifest parses")
    }

    fn pkg_ref(spec: &str) -> PackageRef {
        serde_yaml::from_value(serde_yaml::Value::String(spec.to_string())).expect("valid ref")
    }

    /// The referenced set (schema imports) is exactly the declared set: nothing
    /// undeclared, nothing pruned.
    #[test]
    fn derive_matches_declared_is_clean() {
        let manifest = manifest_from_yaml(
            "package:\n  org: tatolab\n  name: camera\n  version: 2.1.0\n\
             dependencies:\n  '@tatolab/core':\n    version: ^1.0.0\n\
             schemas:\n  CameraConfig:\n    file: schemas/camera_config.yaml\n  VideoFrame:\n    package: '@tatolab/core'\n",
        );
        let out = reconcile_package_dependencies(&manifest, &[]);
        assert!(out.undeclared.is_empty(), "no undeclared: {out:?}");
        assert!(out.pruned.is_empty(), "no pruned: {out:?}");
        assert_eq!(out.retained, vec![pkg_ref("@tatolab/core")]);
    }

    /// A schema imported from a package that is not declared is undeclared —
    /// the hard-error input.
    #[test]
    fn undeclared_schema_import_is_flagged() {
        let manifest = manifest_from_yaml(
            "package:\n  org: tatolab\n  name: camera\n  version: 2.1.0\n\
             schemas:\n  VideoFrame:\n    package: '@tatolab/core'\n",
        );
        let out = reconcile_package_dependencies(&manifest, &[]);
        assert_eq!(out.undeclared, vec![pkg_ref("@tatolab/core")]);
    }

    /// A declared dep referenced by nothing is pruned — unless it carries
    /// `runtime: true`, which keeps it.
    #[test]
    fn unreferenced_dep_is_pruned_unless_runtime() {
        let manifest = manifest_from_yaml(
            "package:\n  org: tatolab\n  name: app\n  version: 1.0.0\n\
             dependencies:\n  '@tatolab/core':\n    version: ^1.0.0\n  '@tatolab/audio':\n    version: ^1.0.0\n    runtime: true\n",
        );
        let out = reconcile_package_dependencies(&manifest, &[]);
        assert_eq!(out.pruned, vec![pkg_ref("@tatolab/core")]);
        assert_eq!(out.retained, vec![pkg_ref("@tatolab/audio")]);
        assert!(out.undeclared.is_empty());
    }

    /// The self-package is never counted as its own dependency even when it
    /// imports its own type through an `External` self-reference.
    #[test]
    fn self_reference_is_dropped() {
        let manifest = manifest_from_yaml(
            "package:\n  org: tatolab\n  name: core\n  version: 1.0.0\n\
             schemas:\n  VideoFrame:\n    package: '@tatolab/core'\n",
        );
        let referenced = derive_referenced_packages(&manifest, &[]);
        assert!(referenced.is_empty(), "self dropped: {referenced:?}");
    }

    #[test]
    fn runtime_flag_round_trips_through_dependency_spec() {
        let spec: DependencySpec = serde_yaml::from_str("version: ^1.0.0\nruntime: true\n").unwrap();
        assert!(spec.is_runtime());
        assert_eq!(
            spec,
            DependencySpec::Version(VersionDependency {
                version: SemVerRange::from_str("^1.0.0").unwrap(),
                runtime: true,
            })
        );
        // The bare-string shorthand is never a runtime dependency.
        let plain: DependencySpec = serde_yaml::from_str("^1.0.0").unwrap();
        assert!(!plain.is_runtime());
    }
}
