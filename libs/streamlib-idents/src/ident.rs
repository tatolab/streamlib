// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

use crate::error::{IdentError, IdentResult};
use crate::semver::SemVer;

/// Org segment of an identifier (the `@org` part).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Org(String);

impl Org {
    pub fn new(s: impl Into<String>) -> IdentResult<Self> {
        let s = s.into();
        validate_org(&s)?;
        Ok(Self(s))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Org {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for Org {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for Org {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        Self::new(raw).map_err(serde::de::Error::custom)
    }
}

/// Package segment.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Package(String);

impl Package {
    pub fn new(s: impl Into<String>) -> IdentResult<Self> {
        let s = s.into();
        validate_package(&s)?;
        Ok(Self(s))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Package {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for Package {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for Package {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        Self::new(raw).map_err(serde::de::Error::custom)
    }
}

/// Type segment (PascalCase).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TypeName(String);

impl TypeName {
    pub fn new(s: impl Into<String>) -> IdentResult<Self> {
        let s = s.into();
        validate_type(&s)?;
        Ok(Self(s))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for TypeName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for TypeName {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for TypeName {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        Self::new(raw).map_err(serde::de::Error::custom)
    }
}

/// Structured schema identifier — `@org/package/Type@version`.
///
/// Constructed via codegen-emitted const literals or via typed YAML/JSON
/// deserialization (each field as its own YAML key). There is no `parse`
/// constructor on this type or any of its segments, by deliberate design —
/// see `docs/architecture/schema-identity-and-packaging.md`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SchemaIdent {
    pub org: Org,
    pub package: Package,
    #[serde(rename = "type")]
    pub r#type: TypeName,
    pub version: SemVer,
}

impl SchemaIdent {
    /// Construct from already-validated structured fields. Callers in
    /// non-codegen positions should use [`Org::new`], [`Package::new`],
    /// [`TypeName::new`] to validate the inputs first.
    pub fn new(org: Org, package: Package, r#type: TypeName, version: SemVer) -> Self {
        Self {
            org,
            package,
            r#type,
            version,
        }
    }
}

impl fmt::Display for SchemaIdent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "@{}/{}/{}@{}",
            self.org, self.package, self.r#type, self.version
        )
    }
}

/// Validate an org segment per the grammar: `[a-z][a-z0-9-]*`.
pub fn validate_org(s: &str) -> IdentResult<()> {
    if s.is_empty() {
        return Err(IdentError::EmptyOrg);
    }
    let mut chars = s.chars();
    let first = chars.next().expect("non-empty");
    if !first.is_ascii_lowercase() {
        return Err(IdentError::OrgMustStartWithLowercase(s.to_string()));
    }
    for c in chars {
        if !is_lower_alnum_or_hyphen(c) {
            return Err(IdentError::InvalidOrgCharacter(s.to_string(), c));
        }
    }
    Ok(())
}

/// Validate a package segment per the grammar: `[a-z][a-z0-9-]*`.
pub fn validate_package(s: &str) -> IdentResult<()> {
    if s.is_empty() {
        return Err(IdentError::EmptyPackage);
    }
    let mut chars = s.chars();
    let first = chars.next().expect("non-empty");
    if !first.is_ascii_lowercase() {
        return Err(IdentError::PackageMustStartWithLowercase(s.to_string()));
    }
    for c in chars {
        if !is_lower_alnum_or_hyphen(c) {
            return Err(IdentError::InvalidPackageCharacter(s.to_string(), c));
        }
    }
    Ok(())
}

/// Validate a type segment per the grammar: `[A-Z][A-Za-z0-9]*` (PascalCase).
pub fn validate_type(s: &str) -> IdentResult<()> {
    if s.is_empty() {
        return Err(IdentError::EmptyType);
    }
    let mut chars = s.chars();
    let first = chars.next().expect("non-empty");
    if !first.is_ascii_uppercase() {
        return Err(IdentError::TypeMustStartWithUppercase(s.to_string()));
    }
    for c in chars {
        if !c.is_ascii_alphanumeric() {
            return Err(IdentError::InvalidTypeCharacter(s.to_string(), c));
        }
    }
    Ok(())
}

fn is_lower_alnum_or_hyphen(c: char) -> bool {
    matches!(c, 'a'..='z' | '0'..='9' | '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ident(org: &str, pkg: &str, ty: &str, v: SemVer) -> SchemaIdent {
        SchemaIdent::new(
            Org::new(org).unwrap(),
            Package::new(pkg).unwrap(),
            TypeName::new(ty).unwrap(),
            v,
        )
    }

    #[test]
    fn display_round_trip_via_yaml() {
        let id = ident("tatolab", "core", "VideoFrame", SemVer::new(1, 0, 0));
        assert_eq!(id.to_string(), "@tatolab/core/VideoFrame@1.0.0");

        // Round-trip via typed YAML (the structured-everywhere wire shape):
        // each field is its own YAML key, no joined string.
        let yaml = serde_yaml::to_string(&id).unwrap();
        let back: SchemaIdent = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(id, back);
        assert_eq!(id.to_string(), back.to_string());
    }

    #[test]
    fn display_round_trip_via_json() {
        let id = ident("tatolab", "h264", "EncodedVideoFrame", SemVer::new(2, 5, 1));
        let json = serde_json::to_string(&id).unwrap();
        let back: SchemaIdent = serde_json::from_str(&json).unwrap();
        assert_eq!(id, back);
        assert_eq!(id.to_string(), back.to_string());
    }

    #[test]
    fn typed_yaml_keys_are_separate_fields() {
        // Asserts the wire shape — fields, not a joined string. If this ever
        // breaks because someone added a custom Deserialize impl that
        // accepts `"@org/pkg/Type@1.0.0"`, the structured-everywhere rule has
        // been violated. The architecture doc forbids this.
        let yaml = "
org: tatolab
package: core
type: VideoFrame
version: 1.0.0
";
        let id: SchemaIdent = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(id.org.as_str(), "tatolab");
        assert_eq!(id.package.as_str(), "core");
        assert_eq!(id.r#type.as_str(), "VideoFrame");
        assert_eq!(id.version, SemVer::new(1, 0, 0));
    }

    #[test]
    fn validators_accept_canonical() {
        validate_org("tatolab").unwrap();
        validate_org("acme-co").unwrap();
        validate_org("a1b2").unwrap();
        validate_package("core").unwrap();
        validate_package("vulkan-video").unwrap();
        validate_type("VideoFrame").unwrap();
        validate_type("H264").unwrap();
        validate_type("X").unwrap();
    }

    #[test]
    fn validators_reject_empty() {
        assert!(matches!(validate_org(""), Err(IdentError::EmptyOrg)));
        assert!(matches!(validate_package(""), Err(IdentError::EmptyPackage)));
        assert!(matches!(validate_type(""), Err(IdentError::EmptyType)));
    }

    #[test]
    fn org_rejects_uppercase_start() {
        assert!(matches!(
            validate_org("Tatolab"),
            Err(IdentError::OrgMustStartWithLowercase(_))
        ));
    }

    #[test]
    fn org_rejects_leading_digit() {
        assert!(matches!(
            validate_org("1tatolab"),
            Err(IdentError::OrgMustStartWithLowercase(_))
        ));
    }

    #[test]
    fn org_rejects_invalid_chars() {
        assert!(matches!(
            validate_org("tato_lab"),
            Err(IdentError::InvalidOrgCharacter(_, '_'))
        ));
        assert!(matches!(
            validate_org("tato.lab"),
            Err(IdentError::InvalidOrgCharacter(_, '.'))
        ));
        assert!(matches!(
            validate_org("tato/lab"),
            Err(IdentError::InvalidOrgCharacter(_, '/'))
        ));
    }

    #[test]
    fn package_rejects_uppercase_start() {
        assert!(matches!(
            validate_package("Core"),
            Err(IdentError::PackageMustStartWithLowercase(_))
        ));
    }

    #[test]
    fn type_rejects_lowercase_start() {
        assert!(matches!(
            validate_type("videoFrame"),
            Err(IdentError::TypeMustStartWithUppercase(_))
        ));
    }

    #[test]
    fn type_rejects_underscores_and_hyphens() {
        assert!(matches!(
            validate_type("Video_Frame"),
            Err(IdentError::InvalidTypeCharacter(_, '_'))
        ));
        assert!(matches!(
            validate_type("Video-Frame"),
            Err(IdentError::InvalidTypeCharacter(_, '-'))
        ));
    }

    #[test]
    fn deserialize_rejects_invalid_org() {
        let yaml = "
org: Tatolab
package: core
type: VideoFrame
version: 1.0.0
";
        let res: Result<SchemaIdent, _> = serde_yaml::from_str(yaml);
        assert!(res.is_err());
    }

    #[test]
    fn deserialize_rejects_invalid_type() {
        let yaml = "
org: tatolab
package: core
type: video_frame
version: 1.0.0
";
        let res: Result<SchemaIdent, _> = serde_yaml::from_str(yaml);
        assert!(res.is_err());
    }
}
