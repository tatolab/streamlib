// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use schemars::r#gen::SchemaGenerator;
use schemars::schema::{InstanceType, Schema, SchemaObject};
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::borrow::Cow;
use std::fmt;

use crate::error::{IdentError, IdentResult};
use crate::semver::{SemVer, SemVerRange};

/// Org segment of an identifier (the `@org` part).
///
/// Constructed via [`Org::new`] (validating) or typed deserialization. No
/// `parse` API — gated below with a `compile_fail` doctest.
///
/// ```compile_fail
/// use streamlib_idents::Org;
/// let _ = Org::parse("tatolab");
/// ```
///
/// ```compile_fail
/// use streamlib_idents::Org;
/// let _: Org = "tatolab".parse().unwrap();
/// ```
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

impl JsonSchema for Org {
    fn schema_name() -> String {
        "Org".into()
    }
    fn schema_id() -> Cow<'static, str> {
        Cow::Borrowed("streamlib_idents::Org")
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        ident_string_schema(
            "Org segment of an identifier (the @org part).",
            r"^[a-z][a-z0-9-]*$",
        )
    }
}

/// Package segment.
///
/// Constructed via [`Package::new`] (validating) or typed deserialization.
/// No `parse` API — gated below with a `compile_fail` doctest.
///
/// ```compile_fail
/// use streamlib_idents::Package;
/// let _ = Package::parse("core");
/// ```
///
/// ```compile_fail
/// use streamlib_idents::Package;
/// let _: Package = "core".parse().unwrap();
/// ```
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

impl JsonSchema for Package {
    fn schema_name() -> String {
        "Package".into()
    }
    fn schema_id() -> Cow<'static, str> {
        Cow::Borrowed("streamlib_idents::Package")
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        ident_string_schema(
            "Package segment of an identifier (the `package` part of `@org/package/Type@version`).",
            r"^[a-z][a-z0-9-]*$",
        )
    }
}

/// Canonical package reference — `@org/name` (no type, no version).
///
/// Structured pair of [`Org`] and [`Package`]. Wire form is the joined
/// `"@org/name"` string (used as YAML map keys and in user-facing output)
/// — `Display` emits it, `Deserialize` reads it. Constructed via
/// [`PackageRef::new`] (validating the underlying `Org` / `Package`) or
/// via typed YAML/JSON deserialization. No `parse` API — gated below
/// with a `compile_fail` doctest.
///
/// ```compile_fail
/// use streamlib_idents::PackageRef;
/// let _ = PackageRef::parse("@tatolab/core");
/// ```
///
/// ```compile_fail
/// use streamlib_idents::PackageRef;
/// let _: PackageRef = "@tatolab/core".parse().unwrap();
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PackageRef {
    pub org: Org,
    pub name: Package,
}

impl PackageRef {
    /// Construct from already-validated [`Org`] and [`Package`].
    pub fn new(org: Org, name: Package) -> Self {
        Self { org, name }
    }
}

impl fmt::Display for PackageRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "@{}/{}", self.org, self.name)
    }
}

impl Serialize for PackageRef {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for PackageRef {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        let stripped = raw.strip_prefix('@').ok_or_else(|| {
            serde::de::Error::custom(format!(
                "PackageRef must start with '@', got '{}'",
                raw
            ))
        })?;
        let (org_str, name_str) = stripped.split_once('/').ok_or_else(|| {
            serde::de::Error::custom(format!(
                "PackageRef must have shape '@org/name', got '{}'",
                raw
            ))
        })?;
        let org = Org::new(org_str).map_err(serde::de::Error::custom)?;
        let name = Package::new(name_str).map_err(serde::de::Error::custom)?;
        Ok(Self { org, name })
    }
}

impl JsonSchema for PackageRef {
    fn schema_name() -> String {
        "PackageRef".into()
    }
    fn schema_id() -> Cow<'static, str> {
        Cow::Borrowed("streamlib_idents::PackageRef")
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        ident_string_schema(
            "Canonical package reference of the form `@org/name`.",
            r"^@[a-z][a-z0-9-]*/[a-z][a-z0-9-]*$",
        )
    }
}

/// Type segment (PascalCase).
///
/// Constructed via [`TypeName::new`] (validating) or typed deserialization.
/// No `parse` API — gated below with a `compile_fail` doctest.
///
/// ```compile_fail
/// use streamlib_idents::TypeName;
/// let _ = TypeName::parse("VideoFrame");
/// ```
///
/// ```compile_fail
/// use streamlib_idents::TypeName;
/// let _: TypeName = "VideoFrame".parse().unwrap();
/// ```
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

impl JsonSchema for TypeName {
    fn schema_name() -> String {
        "TypeName".into()
    }
    fn schema_id() -> Cow<'static, str> {
        Cow::Borrowed("streamlib_idents::TypeName")
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        ident_string_schema(
            "Type segment of a schema identifier (PascalCase, the `Type` part of `@org/package/Type@version`).",
            r"^[A-Z][A-Za-z0-9]*$",
        )
    }
}

/// Structured schema identifier — `@org/package/Type@version`.
///
/// Constructed via codegen-emitted const literals or via typed YAML/JSON
/// deserialization (each field as its own YAML key). There is no `parse`
/// constructor on this type or any of its segments, by deliberate design —
/// see `docs/architecture/schema-identity-and-packaging.md`.
///
/// # No `parse` API — compile-fail doctest gate
///
/// Calling `SchemaIdent::parse` must not compile. If a `parse` method is
/// ever added, the doctest below would compile, the `compile_fail`
/// assertion would flip, and `cargo test --doc` would surface the
/// regression. Same gate as [`Org`], [`Package`], [`TypeName`].
///
/// ```compile_fail
/// use streamlib_idents::SchemaIdent;
/// let _ = SchemaIdent::parse("@tatolab/core/VideoFrame@1.0.0");
/// ```
///
/// `FromStr` would also let `"…".parse::<SchemaIdent>()` work — locked too:
///
/// ```compile_fail
/// use streamlib_idents::SchemaIdent;
/// let _: SchemaIdent = "@tatolab/core/VideoFrame@1.0.0".parse().unwrap();
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[schemars(
    description = "Structured schema identifier — the four-field map form of `@org/package/Type@version`."
)]
pub struct SchemaIdent {
    pub org: Org,
    pub package: Package,
    #[serde(rename = "type")]
    pub r#type: TypeName,
    #[serde(deserialize_with = "deserialize_release_only_semver")]
    pub version: SemVer,
}

impl SchemaIdent {
    /// Construct from already-validated structured fields. Callers in
    /// non-codegen positions should use [`Org::new`], [`Package::new`],
    /// [`TypeName::new`] to validate the inputs first.
    ///
    /// Schema idents are release-only by invariant: `version` is projected
    /// onto its release core (`-dev.N` / `-rc.N` stripped). The flat global
    /// schema registry is version-lookup-blind and the IPC wire form carries
    /// only major/minor/patch, so prerelease fidelity here has no meaning —
    /// packages iterate prereleases; their schema identities do not.
    pub fn new(org: Org, package: Package, r#type: TypeName, version: SemVer) -> Self {
        Self {
            org,
            package,
            r#type,
            version: version.release_core(),
        }
    }
}

/// Deserialize a [`SchemaIdent`] version, rejecting prereleases. Input text
/// carrying `-dev.N` / `-rc.N` in a schema-ident position is an upstream bug
/// (the producing side should have projected via [`SemVer::release_core`]) —
/// surface it rather than silently projecting.
fn deserialize_release_only_semver<'de, D: Deserializer<'de>>(d: D) -> Result<SemVer, D::Error> {
    let version = SemVer::deserialize(d)?;
    if version.prerelease.is_some() {
        return Err(serde::de::Error::custom(format!(
            "schema-ident version `{version}` must be a release `MAJOR.MINOR.PATCH`; \
             prerelease (`-dev.N` / `-rc.N`) versions are not valid for schema idents"
        )));
    }
    Ok(version)
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

/// Imperative-API identifier for a `streamlib.yaml`-packaged module
/// (`@org/name@<range>`).
///
/// Carries the same `@org/name` pair as [`PackageRef`] plus a semver
/// [`SemVerRange`] for version resolution. Constructed via [`ModuleIdent::new`]
/// (already-typed segments), the `module_ident!` proc-macro
/// (compile-time-validated string literals), or typed deserialization. No
/// `parse` API by design — see `docs/architecture/schema-identity-and-packaging.md`.
///
/// Wire form is `@org/name@<range>` (e.g. `@tatolab/core@^1.0.0`,
/// `@tatolab/audio@*`); `Display` emits it, `Deserialize` reads it. The
/// version-suffix is required in the joined wire form — bare `@org/name`
/// strings parse as a [`PackageRef`], not a `ModuleIdent`.
///
/// ```compile_fail
/// use streamlib_idents::ModuleIdent;
/// let _ = ModuleIdent::parse("@tatolab/core@^1.0.0");
/// ```
///
/// ```compile_fail
/// use streamlib_idents::ModuleIdent;
/// let _: ModuleIdent = "@tatolab/core@^1.0.0".parse().unwrap();
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleIdent {
    pub org: Org,
    pub name: Package,
    pub version: SemVerRange,
}

impl ModuleIdent {
    /// Construct from already-validated segments.
    pub fn new(org: Org, name: Package, version: SemVerRange) -> Self {
        Self { org, name, version }
    }

    /// Convenience: any-version constructor (`@org/name@*`). Equivalent to
    /// `ModuleIdent::new(org, name, SemVerRange::Any)`.
    pub fn any(org: Org, name: Package) -> Self {
        Self::new(org, name, SemVerRange::Any)
    }

    /// `@org/name` projection — the canonical [`PackageRef`] without the
    /// version range.
    pub fn package_ref(&self) -> PackageRef {
        PackageRef::new(self.org.clone(), self.name.clone())
    }
}

impl fmt::Display for ModuleIdent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "@{}/{}@{}", self.org, self.name, self.version)
    }
}

impl Serialize for ModuleIdent {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for ModuleIdent {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        parse_module_ident_wire(&raw).map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for ModuleIdent {
    fn schema_name() -> String {
        "ModuleIdent".into()
    }
    fn schema_id() -> Cow<'static, str> {
        Cow::Borrowed("streamlib_idents::ModuleIdent")
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        ident_string_schema(
            "Imperative-API module identifier of the form `@org/name@<range>` \
             where `<range>` is a SemVerRange (`*`, `=1.2.3`, `>=1.2.3`, `^1.2.3`, \
             `~1.2.3`, or bare `1.2.3`; versions may carry an optional `-dev.N` / \
             `-rc.N` prerelease).",
            r"^@[a-z][a-z0-9-]*/[a-z][a-z0-9-]*@(\*|(\^|~|>=|=)?[0-9]+\.[0-9]+\.[0-9]+(-(dev|rc)\.[0-9]+)?)$",
        )
    }
}

/// Parse `@org/name@<range>` into a [`ModuleIdent`]. Used by typed
/// deserialization and the `module_ident!` macro's joined-form arm —
/// callers building from already-typed segments go through
/// [`ModuleIdent::new`] / [`ModuleIdent::any`] instead, so no `parse`
/// surface leaks into general use.
pub(crate) fn parse_module_ident_wire(raw: &str) -> IdentResult<ModuleIdent> {
    let stripped = raw.strip_prefix('@').ok_or_else(|| {
        IdentError::InvalidModuleIdent(
            raw.to_string(),
            "must start with '@'".into(),
        )
    })?;
    let (org_str, rest) = stripped.split_once('/').ok_or_else(|| {
        IdentError::InvalidModuleIdent(
            raw.to_string(),
            "must contain '/' between org and name".into(),
        )
    })?;
    let (name_str, range_str) = rest.split_once('@').ok_or_else(|| {
        IdentError::InvalidModuleIdent(
            raw.to_string(),
            "must contain '@' before the version range (e.g. `@org/name@^1.0.0`)".into(),
        )
    })?;
    let org = Org::new(org_str)?;
    let name = Package::new(name_str)?;
    let version = SemVerRange::from_str(range_str)?;
    Ok(ModuleIdent { org, name, version })
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

fn ident_string_schema(description: &str, pattern: &str) -> Schema {
    Schema::Object(SchemaObject {
        metadata: Some(Box::new(schemars::schema::Metadata {
            description: Some(description.into()),
            ..Default::default()
        })),
        instance_type: Some(InstanceType::String.into()),
        string: Some(Box::new(schemars::schema::StringValidation {
            pattern: Some(pattern.into()),
            ..Default::default()
        })),
        ..Default::default()
    })
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
    fn schema_ident_constructor_projects_prerelease_to_release_core() {
        // The release-only invariant lives in the constructor — a prerelease
        // version cannot survive construction.
        use crate::semver::PrereleaseKind;
        let id = SchemaIdent::new(
            Org::new("tatolab").unwrap(),
            Package::new("camera").unwrap(),
            TypeName::new("Camera").unwrap(),
            SemVer::new_prerelease(0, 4, 33, PrereleaseKind::Dev, 2),
        );
        assert_eq!(id.version, SemVer::new(0, 4, 33));
        assert_eq!(id.to_string(), "@tatolab/camera/Camera@0.4.33");
    }

    #[test]
    fn schema_ident_deserialize_rejects_prerelease_version() {
        // Parse paths REJECT (not project) — prerelease text in a schema-ident
        // position is an upstream bug that must surface.
        let yaml = "
org: tatolab
package: camera
type: Camera
version: 0.4.33-dev.2
";
        let err = serde_yaml::from_str::<SchemaIdent>(yaml).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("release") && msg.contains("dev"),
            "error must name the release-only rule: {msg}"
        );
    }

    #[test]
    fn module_ident_wire_parses_prerelease_range() {
        use crate::semver::PrereleaseKind;
        let wire = "\"@tatolab/camera@>=0.4.33-dev.2\"";
        let id: ModuleIdent = serde_yaml::from_str(wire).unwrap();
        assert_eq!(
            id.version,
            SemVerRange::AtLeast(SemVer::new_prerelease(0, 4, 33, PrereleaseKind::Dev, 2))
        );
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

    #[test]
    fn package_ref_round_trips_canonical_form() {
        let r = PackageRef::new(Org::new("tatolab").unwrap(), Package::new("core").unwrap());
        assert_eq!(r.to_string(), "@tatolab/core");
        let yaml = serde_yaml::to_string(&r).unwrap();
        // Serialize emits the canonical string (with implicit YAML quoting if any).
        let back: PackageRef = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(r, back);
        assert_eq!(r.org.as_str(), "tatolab");
        assert_eq!(r.name.as_str(), "core");
    }

    #[test]
    fn package_ref_deserializes_from_canonical_string() {
        let r: PackageRef = serde_yaml::from_str(r#""@tatolab/core""#).unwrap();
        assert_eq!(r.org.as_str(), "tatolab");
        assert_eq!(r.name.as_str(), "core");
    }

    #[test]
    fn package_ref_rejects_missing_at_prefix() {
        let res: Result<PackageRef, _> = serde_yaml::from_str(r#""tatolab/core""#);
        assert!(res.is_err());
    }

    #[test]
    fn package_ref_rejects_missing_slash() {
        let res: Result<PackageRef, _> = serde_yaml::from_str(r#""@tatolab""#);
        assert!(res.is_err());
    }

    #[test]
    fn package_ref_rejects_invalid_org_segment() {
        // Uppercase first char in org is invalid.
        let res: Result<PackageRef, _> = serde_yaml::from_str(r#""@Tatolab/core""#);
        assert!(res.is_err());
    }

    #[test]
    fn package_ref_rejects_invalid_package_segment() {
        // Uppercase first char in package is invalid.
        let res: Result<PackageRef, _> = serde_yaml::from_str(r#""@tatolab/Core""#);
        assert!(res.is_err());
    }

    #[test]
    fn package_ref_rejects_extra_segments() {
        // `@org/name/extra` must not parse as a PackageRef — that's the
        // SchemaIdent shape (which has its own typed deserialization).
        let res: Result<PackageRef, _> = serde_yaml::from_str(r#""@tatolab/core/extra""#);
        assert!(res.is_err());
    }

    fn module_ident(org: &str, pkg: &str, range: &str) -> ModuleIdent {
        ModuleIdent::new(
            Org::new(org).unwrap(),
            Package::new(pkg).unwrap(),
            SemVerRange::from_str(range).unwrap(),
        )
    }

    #[test]
    fn module_ident_display_round_trips_wire_form() {
        let id = module_ident("tatolab", "core", "^1.0.0");
        assert_eq!(id.to_string(), "@tatolab/core@^1.0.0");

        let yaml = serde_yaml::to_string(&id).unwrap();
        let back: ModuleIdent = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(id, back);
    }

    #[test]
    fn module_ident_any_renders_star_suffix() {
        let id = ModuleIdent::any(Org::new("tatolab").unwrap(), Package::new("core").unwrap());
        assert_eq!(id.to_string(), "@tatolab/core@*");
        assert_eq!(id.version, SemVerRange::Any);
    }

    #[test]
    fn module_ident_parses_each_range_flavor() {
        // `(input wire form, expected typed range, canonical Display form)` —
        // bare `1.2.3` normalizes to `=1.2.3` on Display (existing SemVerRange
        // behavior).
        let cases = [
            ("@tatolab/core@*", SemVerRange::Any, "@tatolab/core@*"),
            (
                "@tatolab/core@1.2.3",
                SemVerRange::Exact(SemVer::new(1, 2, 3)),
                "@tatolab/core@=1.2.3",
            ),
            (
                "@tatolab/core@=1.2.3",
                SemVerRange::Exact(SemVer::new(1, 2, 3)),
                "@tatolab/core@=1.2.3",
            ),
            (
                "@tatolab/core@>=1.0.0",
                SemVerRange::AtLeast(SemVer::new(1, 0, 0)),
                "@tatolab/core@>=1.0.0",
            ),
            (
                "@tatolab/core@^1.0.0",
                SemVerRange::Caret(SemVer::new(1, 0, 0)),
                "@tatolab/core@^1.0.0",
            ),
            (
                "@tatolab/core@~1.2.0",
                SemVerRange::Tilde(SemVer::new(1, 2, 0)),
                "@tatolab/core@~1.2.0",
            ),
        ];
        for (wire, expected_range, expected_display) in cases {
            let id: ModuleIdent = serde_yaml::from_str(&format!("'{}'", wire)).unwrap();
            assert_eq!(id.version, expected_range, "wire form: {wire}");
            assert_eq!(id.to_string(), expected_display, "display for: {wire}");
        }
    }

    #[test]
    fn module_ident_rejects_missing_at_prefix() {
        let res: Result<ModuleIdent, _> = serde_yaml::from_str(r#""tatolab/core@^1.0.0""#);
        assert!(res.is_err());
    }

    #[test]
    fn module_ident_rejects_missing_version_suffix() {
        // Bare `@org/name` parses as a PackageRef, not a ModuleIdent — the
        // version segment is mandatory in the joined wire form.
        let res: Result<ModuleIdent, _> = serde_yaml::from_str(r#""@tatolab/core""#);
        assert!(res.is_err());
    }

    #[test]
    fn module_ident_rejects_invalid_org() {
        let res: Result<ModuleIdent, _> = serde_yaml::from_str(r#""@Tatolab/core@^1.0.0""#);
        assert!(res.is_err());
    }

    #[test]
    fn module_ident_rejects_invalid_range() {
        let res: Result<ModuleIdent, _> = serde_yaml::from_str(r#""@tatolab/core@^1.2.x""#);
        assert!(res.is_err());
    }

    #[test]
    fn module_ident_package_ref_projection_drops_version() {
        let id = module_ident("tatolab", "core", "^1.0.0");
        let pr = id.package_ref();
        assert_eq!(pr.to_string(), "@tatolab/core");
        assert_eq!(pr.org.as_str(), "tatolab");
        assert_eq!(pr.name.as_str(), "core");
    }

    #[test]
    fn package_ref_works_as_btreemap_key() {
        // YAML map keys deserialize through the same Deserialize impl, so
        // declaring `BTreeMap<PackageRef, _>` and reading a yaml map
        // works seamlessly.
        use std::collections::BTreeMap;
        let yaml = r#"
"@tatolab/core": 1
"@tatolab/h264": 2
"#;
        let m: BTreeMap<PackageRef, u32> = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(m.len(), 2);
        let core = PackageRef::new(Org::new("tatolab").unwrap(), Package::new("core").unwrap());
        assert_eq!(m.get(&core), Some(&1));
    }
}
