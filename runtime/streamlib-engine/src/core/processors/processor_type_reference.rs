// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::core::descriptors::{Org, Package, SchemaIdent, SemVer, TypeName};

/// How a [`ProcessorSpec`](crate::core::processors::ProcessorSpec) names its
/// processor type: `(org, package, type)`, resolved against whatever the
/// installed set provides.
///
/// Carries no version, and has no field one could be written into. A version
/// belongs to package *resolution* — the lockfile records what `add` / `link` /
/// `install` selected, and the module walker checks the installed slot against
/// it. By the time code names a processor the question is already settled, so a
/// reference is an import: ask by name, get whatever is installed.
///
/// A version at the reference site could only disagree with that resolution.
/// It used to be expressible, and the disagreement resolved version-exact
/// against a registry whose entries are version-free — so a pinned reference
/// missed a processor that was loaded and registered, and the caller saw
/// "unknown processor type".
///
/// Wire form: the three-key `{org, package, type}` map. A `version` key from an
/// older payload deserializes and is dropped, which is the same answer
/// resolution would have given.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcessorTypeReference {
    pub org: Org,
    pub package: Package,
    #[serde(rename = "type")]
    pub r#type: TypeName,
}

impl ProcessorTypeReference {
    /// Build a reference from its three parts.
    pub fn new(org: Org, package: Package, r#type: TypeName) -> Self {
        Self {
            org,
            package,
            r#type,
        }
    }

    /// The referenced org.
    pub fn org(&self) -> &Org {
        &self.org
    }

    /// The referenced package.
    pub fn package(&self) -> &Package {
        &self.package
    }

    /// The referenced processor type short name.
    pub fn r#type(&self) -> &TypeName {
        &self.r#type
    }

    /// A concrete [`SchemaIdent`] for diagnostics (error messages, the failed
    /// node's identity in the graph), rendering `(org, package, type)@0.0.0` —
    /// the same version-free placeholder convention
    /// [`ProcessorInstanceFactory::resolve_any_version`](crate::core::processors::ProcessorInstanceFactory::resolve_any_version)
    /// uses. Never stored as a real registration key.
    pub fn to_diagnostic_ident(&self) -> SchemaIdent {
        SchemaIdent::new(
            self.org.clone(),
            self.package.clone(),
            self.r#type.clone(),
            SemVer::new(0, 0, 0),
        )
    }
}

/// Narrow a resolved identity to a reference. The version is dropped: it
/// described which package was selected at resolution time, which is not a
/// question the reference site gets to reopen.
impl From<SchemaIdent> for ProcessorTypeReference {
    fn from(ident: SchemaIdent) -> Self {
        Self::new(ident.org, ident.package, ident.r#type)
    }
}

impl fmt::Display for ProcessorTypeReference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "@{org}/{package}/{type}",
            org = self.org,
            package = self.package,
            type = self.r#type
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reference(org: &str, pkg: &str, ty: &str) -> ProcessorTypeReference {
        ProcessorTypeReference::new(
            Org::new(org).unwrap(),
            Package::new(pkg).unwrap(),
            TypeName::new(ty).unwrap(),
        )
    }

    #[test]
    fn accessors_project_the_triple() {
        let r = reference("tatolab", "camera", "Camera");
        assert_eq!(r.org().as_str(), "tatolab");
        assert_eq!(r.package().as_str(), "camera");
        assert_eq!(r.r#type().as_str(), "Camera");
    }

    /// Narrowing keeps the triple and drops the version — the reference site
    /// does not get to reopen what resolution already decided.
    #[test]
    fn from_schema_ident_drops_the_resolved_version() {
        let ident = SchemaIdent::new(
            Org::new("tatolab").unwrap(),
            Package::new("camera").unwrap(),
            TypeName::new("Camera").unwrap(),
            SemVer::new(4, 5, 6),
        );
        assert_eq!(
            ProcessorTypeReference::from(ident),
            reference("tatolab", "camera", "Camera")
        );
    }

    #[test]
    fn diagnostic_ident_uses_the_zero_version_placeholder() {
        assert_eq!(
            reference("tatolab", "camera", "Camera")
                .to_diagnostic_ident()
                .version,
            SemVer::new(0, 0, 0)
        );
    }

    #[test]
    fn serializes_as_a_three_key_object_without_version() {
        let value = serde_json::to_value(reference("tatolab", "camera", "Camera")).unwrap();
        assert!(value.is_object());
        assert_eq!(value["org"], "tatolab");
        assert_eq!(value["package"], "camera");
        assert_eq!(value["type"], "Camera");
        assert!(
            value.get("version").is_none(),
            "a reference must not carry a version key"
        );
    }

    /// A payload from before references dropped their version still parses —
    /// the key is ignored, which is the answer resolution would have given.
    #[test]
    fn a_versioned_payload_parses_and_drops_the_version() {
        let json = r#"{"org":"tatolab","package":"camera","type":"Camera","version":"1.2.3"}"#;
        let parsed: ProcessorTypeReference = serde_json::from_str(json).unwrap();
        assert_eq!(parsed, reference("tatolab", "camera", "Camera"));
    }

    #[test]
    fn round_trips_over_json_and_msgpack() {
        let r = reference("tatolab", "camera", "Camera");
        let back: ProcessorTypeReference =
            serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
        assert_eq!(r, back, "lost equality over JSON");
        // msgpack is the plugin-ABI wire.
        let back: ProcessorTypeReference =
            rmp_serde::from_slice(&rmp_serde::to_vec_named(&r).unwrap()).unwrap();
        assert_eq!(r, back, "lost equality over msgpack");
    }

    #[test]
    fn display_renders_the_version_free_identity() {
        assert_eq!(
            reference("tatolab", "camera", "Camera").to_string(),
            "@tatolab/camera/Camera"
        );
    }
}
