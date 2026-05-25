// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use schemars::r#gen::SchemaGenerator;
use schemars::schema::{InstanceType, Schema, SchemaObject};
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::borrow::Cow;
use std::cmp::Ordering;
use std::fmt;

use crate::error::{IdentError, IdentResult};

/// Three-part semantic version. Pre-release / build metadata not supported in
/// v1 — re-introduce when a real consumer needs them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SemVer {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl SemVer {
    pub const fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }

    /// Internal string conversion. Public on the crate boundary so YAML/JSON
    /// deserialization (the typed-deserialization pathway) can construct a
    /// `SemVer` from `"1.2.3"`. Not a workaround of the `SchemaIdent`
    /// "no parse API" rule — `SchemaIdent` glues multiple structured fields
    /// with punctuation; `SemVer` has a single canonical string form
    /// universally agreed across cargo / npm / pip.
    pub(crate) fn from_dotted(s: &str) -> IdentResult<Self> {
        let mut parts = s.split('.');
        let major = parse_part(s, parts.next())?;
        let minor = parse_part(s, parts.next())?;
        let patch = parse_part(s, parts.next())?;
        if parts.next().is_some() {
            return Err(IdentError::InvalidSemVer(
                s.to_string(),
                "expected exactly three dot-separated integers".into(),
            ));
        }
        Ok(Self {
            major,
            minor,
            patch,
        })
    }
}

fn parse_part(full: &str, part: Option<&str>) -> IdentResult<u32> {
    let p = part.ok_or_else(|| {
        IdentError::InvalidSemVer(
            full.to_string(),
            "expected exactly three dot-separated integers".into(),
        )
    })?;
    p.parse::<u32>()
        .map_err(|e| IdentError::InvalidSemVer(full.to_string(), e.to_string()))
}

impl fmt::Display for SemVer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

impl PartialOrd for SemVer {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for SemVer {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.major, self.minor, self.patch).cmp(&(other.major, other.minor, other.patch))
    }
}

impl Serialize for SemVer {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for SemVer {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        Self::from_dotted(&raw).map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for SemVer {
    fn schema_name() -> String {
        "SemVer".into()
    }
    fn schema_id() -> Cow<'static, str> {
        Cow::Borrowed("streamlib_idents::SemVer")
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        semver_string_schema(
            "Three-part semantic version `MAJOR.MINOR.PATCH` (no pre-release / build metadata).",
            r"^[0-9]+\.[0-9]+\.[0-9]+$",
        )
    }
}

/// SemVer range matcher. Supported operators (npm-flavoured subset chosen for
/// what `streamlib.yaml` actually needs):
///
/// - `*` wildcard — matches any version
/// - `=1.2.3` exact
/// - `>=1.2.3` lower bound
/// - `^1.2.3` caret — same major, version >= input (npm semantics)
/// - `~1.2.3` tilde — same major.minor, version >= input
///
/// Pre-1.0 caret (`^0.x.y`) follows npm: `^0.2.3` matches `>=0.2.3 <0.3.0`,
/// and `^0.0.3` matches exactly `0.0.3`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SemVerRange {
    /// `*` — matches every version. The imperative module API uses this when
    /// the caller doesn't pin a version range.
    Any,
    Exact(SemVer),
    AtLeast(SemVer),
    Caret(SemVer),
    Tilde(SemVer),
}

impl SemVerRange {
    /// Parse a range string. Accepts `*` (any), `=1.2.3` (exact),
    /// `>=1.2.3` (lower bound), `^1.2.3` (caret, npm), `~1.2.3` (tilde),
    /// and bare `1.2.3` (exact). Surfaces [`IdentError::InvalidSemVer`]
    /// on malformed inputs.
    pub fn from_str(s: &str) -> IdentResult<Self> {
        let s = s.trim();
        if s == "*" {
            return Ok(Self::Any);
        }
        if let Some(rest) = s.strip_prefix("^") {
            return Ok(Self::Caret(SemVer::from_dotted(rest.trim())?));
        }
        if let Some(rest) = s.strip_prefix("~") {
            return Ok(Self::Tilde(SemVer::from_dotted(rest.trim())?));
        }
        if let Some(rest) = s.strip_prefix(">=") {
            return Ok(Self::AtLeast(SemVer::from_dotted(rest.trim())?));
        }
        if let Some(rest) = s.strip_prefix("=") {
            return Ok(Self::Exact(SemVer::from_dotted(rest.trim())?));
        }
        // Bare version → exact match.
        Ok(Self::Exact(SemVer::from_dotted(s)?))
    }

    pub fn matches(&self, v: SemVer) -> bool {
        match *self {
            Self::Any => true,
            Self::Exact(req) => v == req,
            Self::AtLeast(req) => v >= req,
            Self::Caret(req) => caret_matches(req, v),
            Self::Tilde(req) => v >= req && v.major == req.major && v.minor == req.minor,
        }
    }
}

fn caret_matches(req: SemVer, v: SemVer) -> bool {
    if v < req {
        return false;
    }
    if req.major > 0 {
        v.major == req.major
    } else if req.minor > 0 {
        // ^0.minor.patch → same minor
        v.major == 0 && v.minor == req.minor
    } else {
        // ^0.0.patch → exact
        v == req
    }
}

impl fmt::Display for SemVerRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Any => f.write_str("*"),
            Self::Exact(v) => write!(f, "={}", v),
            Self::AtLeast(v) => write!(f, ">={}", v),
            Self::Caret(v) => write!(f, "^{}", v),
            Self::Tilde(v) => write!(f, "~{}", v),
        }
    }
}

impl Serialize for SemVerRange {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for SemVerRange {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        Self::from_str(&raw).map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for SemVerRange {
    fn schema_name() -> String {
        "SemVerRange".into()
    }
    fn schema_id() -> Cow<'static, str> {
        Cow::Borrowed("streamlib_idents::SemVerRange")
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        semver_string_schema(
            "SemVer range matcher: `*` (any), `=1.2.3` exact, `>=1.2.3` lower bound, `^1.2.3` caret (npm), `~1.2.3` tilde, or bare `1.2.3` (exact).",
            r"^(\*|(\^|~|>=|=)?[0-9]+\.[0-9]+\.[0-9]+)$",
        )
    }
}

fn semver_string_schema(description: &str, pattern: &str) -> Schema {
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

    #[test]
    fn semver_round_trip() {
        let v = SemVer::new(1, 2, 3);
        assert_eq!(v.to_string(), "1.2.3");
        assert_eq!(SemVer::from_dotted("1.2.3").unwrap(), v);
    }

    #[test]
    fn semver_rejects_garbage() {
        assert!(SemVer::from_dotted("1.2").is_err());
        assert!(SemVer::from_dotted("1.2.3.4").is_err());
        assert!(SemVer::from_dotted("1.x.3").is_err());
        assert!(SemVer::from_dotted("").is_err());
    }

    #[test]
    fn semver_ordering() {
        assert!(SemVer::new(1, 0, 0) < SemVer::new(1, 0, 1));
        assert!(SemVer::new(1, 0, 0) < SemVer::new(1, 1, 0));
        assert!(SemVer::new(1, 0, 0) < SemVer::new(2, 0, 0));
        assert!(SemVer::new(0, 9, 9) < SemVer::new(1, 0, 0));
    }

    #[test]
    fn range_exact() {
        let r = SemVerRange::from_str("=1.2.3").unwrap();
        assert!(r.matches(SemVer::new(1, 2, 3)));
        assert!(!r.matches(SemVer::new(1, 2, 4)));

        let bare = SemVerRange::from_str("1.2.3").unwrap();
        assert_eq!(bare, SemVerRange::Exact(SemVer::new(1, 2, 3)));
    }

    #[test]
    fn range_at_least() {
        let r = SemVerRange::from_str(">=1.2.3").unwrap();
        assert!(r.matches(SemVer::new(1, 2, 3)));
        assert!(r.matches(SemVer::new(2, 0, 0)));
        assert!(!r.matches(SemVer::new(1, 2, 2)));
    }

    #[test]
    fn range_caret_post_1_0() {
        let r = SemVerRange::from_str("^1.2.3").unwrap();
        assert!(r.matches(SemVer::new(1, 2, 3)));
        assert!(r.matches(SemVer::new(1, 9, 0)));
        assert!(!r.matches(SemVer::new(2, 0, 0)));
        assert!(!r.matches(SemVer::new(1, 2, 2)));
    }

    #[test]
    fn range_caret_pre_1_0_minor() {
        let r = SemVerRange::from_str("^0.2.3").unwrap();
        assert!(r.matches(SemVer::new(0, 2, 3)));
        assert!(r.matches(SemVer::new(0, 2, 9)));
        assert!(!r.matches(SemVer::new(0, 3, 0)));
        assert!(!r.matches(SemVer::new(1, 0, 0)));
    }

    #[test]
    fn range_caret_pre_1_0_patch() {
        let r = SemVerRange::from_str("^0.0.3").unwrap();
        assert!(r.matches(SemVer::new(0, 0, 3)));
        assert!(!r.matches(SemVer::new(0, 0, 4)));
    }

    #[test]
    fn range_tilde() {
        let r = SemVerRange::from_str("~1.2.3").unwrap();
        assert!(r.matches(SemVer::new(1, 2, 3)));
        assert!(r.matches(SemVer::new(1, 2, 9)));
        assert!(!r.matches(SemVer::new(1, 3, 0)));
    }

    #[test]
    fn range_round_trip_through_yaml() {
        let r = SemVerRange::from_str("^1.2.3").unwrap();
        let s = serde_yaml::to_string(&r).unwrap();
        let back: SemVerRange = serde_yaml::from_str(&s).unwrap();
        assert_eq!(r, back);
    }

    #[test]
    fn range_any_matches_every_version() {
        let r = SemVerRange::from_str("*").unwrap();
        assert_eq!(r, SemVerRange::Any);
        assert!(r.matches(SemVer::new(0, 0, 0)));
        assert!(r.matches(SemVer::new(1, 2, 3)));
        assert!(r.matches(SemVer::new(u32::MAX, u32::MAX, u32::MAX)));
        assert_eq!(r.to_string(), "*");
    }

    #[test]
    fn range_any_round_trips_through_yaml() {
        let r = SemVerRange::Any;
        let s = serde_yaml::to_string(&r).unwrap();
        let back: SemVerRange = serde_yaml::from_str(&s).unwrap();
        assert_eq!(r, back);
    }
}
