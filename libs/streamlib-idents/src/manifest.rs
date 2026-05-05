// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::ident::{Org, Package};
use crate::semver::{SemVer, SemVerRange};

/// Package-flavor `streamlib.yaml` — declares a publishable package.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageManifest {
    pub package: PackageMetadata,

    #[serde(default)]
    pub dependencies: BTreeMap<String, DependencySpec>,
}

/// Project-flavor `streamlib.yaml` — declares a consumer project.
///
/// No `package:` block; only declares `dependencies`. Use this for
/// applications and examples that depend on packages but aren't themselves
/// publishable.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectManifest {
    #[serde(default)]
    pub dependencies: BTreeMap<String, DependencySpec>,
}

/// Package metadata. `version` lives here and ONLY here — per the
/// package-as-publication-unit rule (CI lint rejects per-schema versions).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageMetadata {
    pub org: Org,
    pub name: Package,
    pub version: SemVer,
    #[serde(default)]
    pub description: Option<String>,
}

/// Dependency declaration. Three sources:
///
/// - String form `^1.2.3` → registry dependency with a semver range
/// - `{ path: ../foo }` → path dependency
/// - `{ git: ..., rev: ... }` → git dependency
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum DependencySpec {
    Registry(RegistryDependency),
    Path(PathDependency),
    Git(GitDependency),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryDependency {
    pub version: SemVerRange,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PathDependency {
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitDependency {
    pub git: String,
    /// Pinned commit. Branch / tag refs are deliberately not supported —
    /// pinning is required for reproducible resolution. Mirrors the
    /// workspace rule from `CLAUDE.md` (`Conventions → Dependencies`).
    pub rev: String,
}

// Custom Deserialize: a bare string `^1.2.3` is sugar for a Registry
// dependency. A map is one of the structured variants.
impl<'de> Deserialize<'de> for DependencySpec {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            // Bare string: `dep-name: ^1.2.3`.
            Range(SemVerRange),
            // Structured map: `dep-name: { ... }`.
            Map(StructuredRepr),
        }

        #[derive(Deserialize)]
        #[serde(untagged, deny_unknown_fields)]
        enum StructuredRepr {
            Registry(RegistryDependency),
            Path(PathDependency),
            Git(GitDependency),
        }

        let repr = Repr::deserialize(d)?;
        Ok(match repr {
            Repr::Range(v) => Self::Registry(RegistryDependency { version: v }),
            Repr::Map(StructuredRepr::Registry(r)) => Self::Registry(r),
            Repr::Map(StructuredRepr::Path(p)) => Self::Path(p),
            Repr::Map(StructuredRepr::Git(g)) => Self::Git(g),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_manifest_round_trip() {
        let yaml = "
package:
  org: tatolab
  name: core
  version: 1.0.0
  description: Canonical wire vocabulary
dependencies: {}
";
        let m: PackageManifest = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(m.package.org.as_str(), "tatolab");
        assert_eq!(m.package.name.as_str(), "core");
        assert_eq!(m.package.version, SemVer::new(1, 0, 0));
        assert!(m.dependencies.is_empty());
    }

    #[test]
    fn project_manifest_with_three_dep_flavors() {
        let yaml = r#"
dependencies:
  "@tatolab/core": "^1.0.0"
  "@tatolab/h264":
    path: ../h264
  "@tatolab/moq":
    git: https://github.com/tatolab/moq
    rev: abc123def456
"#;
        let m: ProjectManifest = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(m.dependencies.len(), 3);

        match m.dependencies.get("@tatolab/core").unwrap() {
            DependencySpec::Registry(r) => assert_eq!(r.version.to_string(), "^1.0.0"),
            other => panic!("expected Registry, got {:?}", other),
        }

        match m.dependencies.get("@tatolab/h264").unwrap() {
            DependencySpec::Path(p) => assert_eq!(p.path, PathBuf::from("../h264")),
            other => panic!("expected Path, got {:?}", other),
        }

        match m.dependencies.get("@tatolab/moq").unwrap() {
            DependencySpec::Git(g) => {
                assert_eq!(g.git, "https://github.com/tatolab/moq");
                assert_eq!(g.rev, "abc123def456");
            }
            other => panic!("expected Git, got {:?}", other),
        }
    }

    #[test]
    fn package_manifest_rejects_invalid_org() {
        let yaml = "
package:
  org: Tatolab
  name: core
  version: 1.0.0
";
        let res: Result<PackageManifest, _> = serde_yaml::from_str(yaml);
        assert!(res.is_err());
    }

    #[test]
    fn package_manifest_rejects_invalid_version() {
        let yaml = "
package:
  org: tatolab
  name: core
  version: not-a-version
";
        let res: Result<PackageManifest, _> = serde_yaml::from_str(yaml);
        assert!(res.is_err());
    }

    #[test]
    fn manifest_rejects_unknown_field() {
        // The `deny_unknown_fields` invariant — typos at the manifest top
        // level should fail loudly, not silently turn into no-ops.
        let yaml = "
package:
  org: tatolab
  name: core
  version: 1.0.0
unknown_top_level_field: oops
";
        let res: Result<PackageManifest, _> = serde_yaml::from_str(yaml);
        assert!(res.is_err());
    }

    #[test]
    fn git_dependency_requires_rev() {
        let yaml = r#"
dependencies:
  "@tatolab/moq":
    git: https://github.com/tatolab/moq
"#;
        let res: Result<ProjectManifest, _> = serde_yaml::from_str(yaml);
        assert!(res.is_err(), "git dep without rev must fail");
    }
}
