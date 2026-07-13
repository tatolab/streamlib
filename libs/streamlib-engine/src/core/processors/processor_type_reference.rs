// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::core::descriptors::{Org, Package, SchemaIdent, SemVer, TypeName};

/// How a [`ProcessorSpec`](crate::core::processors::ProcessorSpec) names its
/// processor type: either a fully version-pinned identity, or a version-free
/// reference resolved to the single installed provider at add time.
///
/// The version-free [`Self::ResolveToInstalled`] form is what makes lazy
/// plugin discovery reach the runtime hook: it carries only `(org, package,
/// type)` and defers version resolution to `add_processor` time, so a
/// reference to a not-yet-loaded package triggers the lazy load instead of
/// failing at the call site. The version-pinned [`Self::VersionPinned`] form
/// is the exact-`SchemaIdent` path.
///
/// Wire compatibility: `#[serde(untagged)]` with [`Self::VersionPinned`]
/// first means a version-pinned reference serializes byte-identically to a
/// bare [`SchemaIdent`] (a four-key `{org, package, type, version}` map), and
/// a version-free reference serializes as the three-key `{org, package,
/// type}` map. Deserialization tries [`Self::VersionPinned`] first (it
/// requires `version`) and falls to [`Self::ResolveToInstalled`] when
/// `version` is absent — order alone disambiguates.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ProcessorTypeReference {
    /// Version-pinned — the exact [`SchemaIdent`] (including its version) must
    /// be registered for the reference to resolve.
    VersionPinned(SchemaIdent),
    /// Version-free — resolve `(org, package, type)` to the single installed
    /// provider's registered descriptor at add time.
    ResolveToInstalled {
        org: Org,
        package: Package,
        #[serde(rename = "type")]
        r#type: TypeName,
    },
}

impl ProcessorTypeReference {
    /// The referenced org, common to both forms.
    pub fn org(&self) -> &Org {
        match self {
            Self::VersionPinned(ident) => &ident.org,
            Self::ResolveToInstalled { org, .. } => org,
        }
    }

    /// The referenced package, common to both forms.
    pub fn package(&self) -> &Package {
        match self {
            Self::VersionPinned(ident) => &ident.package,
            Self::ResolveToInstalled { package, .. } => package,
        }
    }

    /// The referenced processor type short name, common to both forms.
    pub fn r#type(&self) -> &TypeName {
        match self {
            Self::VersionPinned(ident) => &ident.r#type,
            Self::ResolveToInstalled { r#type, .. } => r#type,
        }
    }

    /// The pinned [`SchemaIdent`] when this is a [`Self::VersionPinned`]
    /// reference; `None` for the version-free form.
    pub fn as_version_pinned(&self) -> Option<&SchemaIdent> {
        match self {
            Self::VersionPinned(ident) => Some(ident),
            Self::ResolveToInstalled { .. } => None,
        }
    }

    /// A concrete [`SchemaIdent`] for diagnostics (error messages, the failed
    /// node's identity in the graph). The pinned form yields its real ident;
    /// the version-free form renders `(org, package, type)@0.0.0` — the same
    /// version-free placeholder convention
    /// [`ProcessorInstanceFactory::resolve_any_version`](crate::core::processors::ProcessorInstanceFactory::resolve_any_version)
    /// already uses. Never stored as a real registration key.
    pub fn to_diagnostic_ident(&self) -> SchemaIdent {
        match self {
            Self::VersionPinned(ident) => ident.clone(),
            Self::ResolveToInstalled {
                org,
                package,
                r#type,
            } => SchemaIdent::new(
                org.clone(),
                package.clone(),
                r#type.clone(),
                SemVer::new(0, 0, 0),
            ),
        }
    }
}

impl From<SchemaIdent> for ProcessorTypeReference {
    fn from(ident: SchemaIdent) -> Self {
        Self::VersionPinned(ident)
    }
}

impl fmt::Display for ProcessorTypeReference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::VersionPinned(ident) => write!(f, "{ident}"),
            Self::ResolveToInstalled {
                org,
                package,
                r#type,
            } => write!(f, "@{org}/{package}/{type} (version-free)", type = r#type),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pinned(org: &str, pkg: &str, ty: &str, v: SemVer) -> ProcessorTypeReference {
        ProcessorTypeReference::VersionPinned(SchemaIdent::new(
            Org::new(org).unwrap(),
            Package::new(pkg).unwrap(),
            TypeName::new(ty).unwrap(),
            v,
        ))
    }

    fn free(org: &str, pkg: &str, ty: &str) -> ProcessorTypeReference {
        ProcessorTypeReference::ResolveToInstalled {
            org: Org::new(org).unwrap(),
            package: Package::new(pkg).unwrap(),
            r#type: TypeName::new(ty).unwrap(),
        }
    }

    #[test]
    fn accessors_project_both_variants_to_the_same_triple() {
        let p = pinned("tatolab", "camera", "Camera", SemVer::new(1, 2, 3));
        let f = free("tatolab", "camera", "Camera");
        for r in [&p, &f] {
            assert_eq!(r.org().as_str(), "tatolab");
            assert_eq!(r.package().as_str(), "camera");
            assert_eq!(r.r#type().as_str(), "Camera");
        }
    }

    #[test]
    fn from_schema_ident_is_version_pinned() {
        let ident = SchemaIdent::new(
            Org::new("tatolab").unwrap(),
            Package::new("camera").unwrap(),
            TypeName::new("Camera").unwrap(),
            SemVer::new(4, 5, 6),
        );
        let r: ProcessorTypeReference = ident.clone().into();
        assert_eq!(r.as_version_pinned(), Some(&ident));
    }

    #[test]
    fn diagnostic_ident_uses_zero_version_for_version_free() {
        assert_eq!(
            free("tatolab", "camera", "Camera")
                .to_diagnostic_ident()
                .version,
            SemVer::new(0, 0, 0)
        );
        // The pinned form keeps its real version in diagnostics.
        assert_eq!(
            pinned("tatolab", "camera", "Camera", SemVer::new(7, 0, 0))
                .to_diagnostic_ident()
                .version,
            SemVer::new(7, 0, 0)
        );
    }

    #[test]
    fn version_pinned_serializes_byte_identically_to_bare_schema_ident() {
        // Wire-compat lock: the untagged newtype variant must serialize
        // exactly as the inner SchemaIdent — a four-key object — so existing
        // engine-free plugins that send version-pinned specs are unaffected.
        let ident = SchemaIdent::new(
            Org::new("tatolab").unwrap(),
            Package::new("core").unwrap(),
            TypeName::new("VideoFrame").unwrap(),
            SemVer::new(1, 0, 0),
        );
        let via_ref = serde_json::to_value(ProcessorTypeReference::from(ident.clone())).unwrap();
        let via_ident = serde_json::to_value(&ident).unwrap();
        assert_eq!(via_ref, via_ident);
        assert!(via_ref.is_object());
        assert_eq!(via_ref["version"], "1.0.0");
    }

    #[test]
    fn version_free_serializes_as_three_key_object_without_version() {
        let value = serde_json::to_value(free("tatolab", "camera", "Camera")).unwrap();
        assert!(value.is_object());
        assert_eq!(value["org"], "tatolab");
        assert_eq!(value["package"], "camera");
        assert_eq!(value["type"], "Camera");
        assert!(
            value.get("version").is_none(),
            "the version-free form must not carry a version key"
        );
    }

    #[test]
    fn untagged_round_trip_preserves_each_variant() {
        for (label, r) in [
            (
                "pinned",
                pinned("tatolab", "camera", "Camera", SemVer::new(2, 1, 0)),
            ),
            ("free", free("tatolab", "camera", "Camera")),
        ] {
            // JSON
            let json = serde_json::to_string(&r).unwrap();
            let back: ProcessorTypeReference = serde_json::from_str(&json).unwrap();
            assert_eq!(r, back, "{label} lost equality over JSON");
            // msgpack (the plugin-ABI wire) — untagged relies on the
            // self-describing map, which msgpack is.
            let bytes = rmp_serde::to_vec_named(&r).unwrap();
            let back: ProcessorTypeReference = rmp_serde::from_slice(&bytes).unwrap();
            assert_eq!(r, back, "{label} lost equality over msgpack");
        }
    }

    #[test]
    fn four_key_input_deserializes_as_version_pinned_not_version_free() {
        // A wire object carrying `version` must resolve to VersionPinned —
        // the ordering guarantee that keeps the version-pinned wire stable.
        let json = r#"{"org":"tatolab","package":"camera","type":"Camera","version":"3.0.0"}"#;
        let r: ProcessorTypeReference = serde_json::from_str(json).unwrap();
        assert!(
            matches!(r, ProcessorTypeReference::VersionPinned(_)),
            "a four-key object must deserialize as VersionPinned, got {r:?}"
        );
    }
}
