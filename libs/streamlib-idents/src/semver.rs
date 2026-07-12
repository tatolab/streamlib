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

/// Prerelease channel of a [`SemVer`]. Ordered `Dev` < `Rc`, matching the
/// SemVer 2.0 ASCII ordering of the `dev` / `rc` identifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum PrereleaseKind {
    Dev,
    Rc,
}

impl PrereleaseKind {
    /// The identifier as it appears in the version string (`dev` / `rc`).
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Dev => "dev",
            Self::Rc => "rc",
        }
    }
}

/// Prerelease component of a [`SemVer`] — a channel plus an ordinal, e.g.
/// `-dev.4` or `-rc.1`. Ordered by `(kind, n)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Prerelease {
    pub kind: PrereleaseKind,
    pub n: u32,
}

/// Three-part semantic version with an optional `-dev.N` / `-rc.N` prerelease.
/// The prerelease grammar is deliberately closed: only `dev` and `rc` channels
/// are accepted, and `+build` metadata is rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SemVer {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
    pub prerelease: Option<Prerelease>,
}

impl SemVer {
    /// A release version (no prerelease).
    pub const fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self {
            major,
            minor,
            patch,
            prerelease: None,
        }
    }

    /// A prerelease version (`X.Y.Z-<kind>.N`).
    pub const fn new_prerelease(
        major: u32,
        minor: u32,
        patch: u32,
        kind: PrereleaseKind,
        n: u32,
    ) -> Self {
        Self {
            major,
            minor,
            patch,
            prerelease: Some(Prerelease { kind, n }),
        }
    }

    /// This version with any prerelease stripped — the release core `X.Y.Z`.
    /// Used to project a package's (possibly prerelease) version onto the
    /// schema-ident axis, which is release-only by invariant.
    pub const fn release_core(self) -> Self {
        Self {
            major: self.major,
            minor: self.minor,
            patch: self.patch,
            prerelease: None,
        }
    }

    /// Internal string conversion. Public on the crate boundary so YAML/JSON
    /// deserialization (the typed-deserialization pathway) can construct a
    /// `SemVer` from `"1.2.3"` / `"1.2.3-dev.4"`. Not a workaround of the
    /// `SchemaIdent` "no parse API" rule — `SchemaIdent` glues multiple
    /// structured fields with punctuation; `SemVer` has a single canonical
    /// string form universally agreed across cargo / npm / pip.
    pub(crate) fn from_dotted(s: &str) -> IdentResult<Self> {
        // Split the `MAJOR.MINOR.PATCH` core from an optional `-<prerelease>`
        // on the first `-`. A `+build` suffix has no `-`, so it stays in the
        // core and fails the integer parse below — rejecting build metadata.
        let (core, prerelease) = match s.split_once('-') {
            Some((core, suffix)) => (core, Some(parse_prerelease(s, suffix)?)),
            None => (s, None),
        };
        let mut parts = core.split('.');
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
            prerelease,
        })
    }
}

impl std::str::FromStr for SemVer {
    type Err = IdentError;

    fn from_str(s: &str) -> IdentResult<Self> {
        Self::from_dotted(s)
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

/// Parse a `-` suffix into a [`Prerelease`]. Only `dev.N` / `rc.N` are valid.
fn parse_prerelease(full: &str, suffix: &str) -> IdentResult<Prerelease> {
    let (kind_str, n_str) = suffix.split_once('.').ok_or_else(|| {
        IdentError::InvalidSemVer(
            full.to_string(),
            "prerelease must be `-dev.N` or `-rc.N` (e.g. `1.2.3-dev.4`)".into(),
        )
    })?;
    let kind = match kind_str {
        "dev" => PrereleaseKind::Dev,
        "rc" => PrereleaseKind::Rc,
        other => {
            return Err(IdentError::InvalidSemVer(
                full.to_string(),
                format!("unknown prerelease channel `{other}` (only `dev` and `rc` are valid)"),
            ))
        }
    };
    let n = n_str.parse::<u32>().map_err(|_| {
        IdentError::InvalidSemVer(
            full.to_string(),
            format!("prerelease ordinal `{n_str}` is not a non-negative integer"),
        )
    })?;
    Ok(Prerelease { kind, n })
}

impl fmt::Display for SemVer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if let Some(pre) = self.prerelease {
            write!(f, "-{}.{}", pre.kind.as_str(), pre.n)?;
        }
        Ok(())
    }
}

impl PartialOrd for SemVer {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for SemVer {
    fn cmp(&self, other: &Self) -> Ordering {
        // Cores compare numerically; on equal cores a release (None) outranks
        // any prerelease (Some), and two prereleases compare by `(kind, n)`.
        (self.major, self.minor, self.patch)
            .cmp(&(other.major, other.minor, other.patch))
            .then_with(|| match (self.prerelease, other.prerelease) {
                (None, None) => Ordering::Equal,
                (None, Some(_)) => Ordering::Greater,
                (Some(_), None) => Ordering::Less,
                (Some(a), Some(b)) => a.cmp(&b),
            })
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
            "Three-part semantic version `MAJOR.MINOR.PATCH` with an optional `-dev.N` / `-rc.N` prerelease (no `+build` metadata).",
            r"^[0-9]+\.[0-9]+\.[0-9]+(-(dev|rc)\.[0-9]+)?$",
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
///
/// Prerelease policy (npm semantics): a prerelease candidate is selected only
/// when the range is written against a prerelease. `*` and release-req ranges
/// match releases only; a prerelease req (`>=1.2.0-dev.3`, `^0.4.33-dev.2`)
/// additionally admits same-core prereleases at or above it.
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
            // npm prerelease policy: a prerelease is only ever selected when
            // the range explicitly asks for one. `Any` and release-req ranges
            // match releases only; a prerelease req additionally admits
            // same-core prereleases at or above it.
            Self::Any => v.prerelease.is_none(),
            Self::Exact(req) => v == req,
            Self::AtLeast(req) => prerelease_gated(req, v, |req, v| v >= req),
            Self::Caret(req) => prerelease_gated(req, v, caret_matches),
            Self::Tilde(req) => prerelease_gated(req, v, |req, v| {
                v >= req && v.major == req.major && v.minor == req.minor
            }),
        }
    }
}

/// Same `(major, minor, patch)` core, ignoring prerelease.
fn same_core(a: SemVer, b: SemVer) -> bool {
    a.major == b.major && a.minor == b.minor && a.patch == b.patch
}

/// Apply the npm-style prerelease policy on top of an operator's release-core
/// rule. `core_rule` decides membership per the operator's existing semantics.
///
/// - Release req: releases per `core_rule`; all prereleases excluded.
/// - Prerelease req, release candidate: `core_rule` (a release outranks the
///   prerelease req at equal core, and higher cores pass too).
/// - Prerelease req, prerelease candidate: only same-core and `>= req`.
fn prerelease_gated(req: SemVer, v: SemVer, core_rule: impl Fn(SemVer, SemVer) -> bool) -> bool {
    match (req.prerelease, v.prerelease) {
        (None, _) => v.prerelease.is_none() && core_rule(req, v),
        (Some(_), None) => core_rule(req, v),
        (Some(_), Some(_)) => same_core(req, v) && v >= req,
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
            "SemVer range matcher: `*` (any), `=1.2.3` exact, `>=1.2.3` lower bound, `^1.2.3` caret (npm), `~1.2.3` tilde, or bare `1.2.3` (exact). Versions may carry an optional `-dev.N` / `-rc.N` prerelease.",
            r"^(\*|(\^|~|>=|=)?[0-9]+\.[0-9]+\.[0-9]+(-(dev|rc)\.[0-9]+)?)$",
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

    // ---- prerelease support (#1215) ----

    #[test]
    fn prerelease_parse_display_round_trip() {
        for (s, expected) in [
            ("1.2.3-dev.4", SemVer::new_prerelease(1, 2, 3, PrereleaseKind::Dev, 4)),
            ("0.4.33-rc.1", SemVer::new_prerelease(0, 4, 33, PrereleaseKind::Rc, 1)),
            ("1.2.3", SemVer::new(1, 2, 3)),
            ("1.1.3-dev.0", SemVer::new_prerelease(1, 1, 3, PrereleaseKind::Dev, 0)),
        ] {
            let parsed = SemVer::from_dotted(s).unwrap();
            assert_eq!(parsed, expected, "parse `{s}`");
            assert_eq!(parsed.to_string(), s, "display round-trip `{s}`");
            // FromStr is the public canonical parse and must agree.
            assert_eq!(s.parse::<SemVer>().unwrap(), expected, "FromStr `{s}`");
        }
    }

    #[test]
    fn prerelease_grammar_is_closed() {
        // Only `dev` / `rc`, exactly one dotted ordinal, no `+build`.
        for bad in [
            "1.2.3-alpha.1", // unknown channel
            "1.2.3-dev",     // missing ordinal
            "1.2.3-dev.x",   // non-numeric ordinal
            "1.2.3+build",   // build metadata unsupported
            "1.2.3-dev.1.2", // ordinal must be a single integer
            "1.2.3-rc",      // missing ordinal
            "1.2.3-",        // empty suffix
        ] {
            assert!(
                SemVer::from_dotted(bad).is_err(),
                "`{bad}` must be rejected"
            );
        }
    }

    #[test]
    fn release_core_strips_prerelease() {
        let v = SemVer::new_prerelease(1, 2, 3, PrereleaseKind::Rc, 5);
        assert_eq!(v.release_core(), SemVer::new(1, 2, 3));
        // Idempotent on a release.
        assert_eq!(SemVer::new(1, 2, 3).release_core(), SemVer::new(1, 2, 3));
    }

    #[test]
    fn prerelease_ordering_matrix() {
        // The load-bearing ordering: dev < rc, ordinal ascending, and a
        // release outranks every prerelease of the same core. Mentally revert
        // the hand-written Ord's None/Some arms and this fails.
        let dev2 = SemVer::new_prerelease(1, 1, 3, PrereleaseKind::Dev, 2);
        let dev10 = SemVer::new_prerelease(1, 1, 3, PrereleaseKind::Dev, 10);
        let rc1 = SemVer::new_prerelease(1, 1, 3, PrereleaseKind::Rc, 1);
        let release = SemVer::new(1, 1, 3);
        assert!(dev2 < dev10, "dev.2 < dev.10");
        assert!(dev10 < rc1, "dev.10 < rc.1");
        assert!(rc1 < release, "rc.1 < release");
        // Full chain.
        assert!(dev2 < dev10 && dev10 < rc1 && rc1 < release);
        // A prerelease of a lower core is below a prerelease of a higher core.
        let rc9 = SemVer::new_prerelease(1, 1, 3, PrereleaseKind::Rc, 9);
        let next_dev1 = SemVer::new_prerelease(1, 1, 4, PrereleaseKind::Dev, 1);
        assert!(rc9 < next_dev1, "1.1.3-rc.9 < 1.1.4-dev.1");
        // Sorting yields the spec order.
        let mut all = vec![release, rc1, dev10, dev2];
        all.sort();
        assert_eq!(all, vec![dev2, dev10, rc1, release]);
    }

    #[test]
    fn range_any_excludes_prereleases() {
        let r = SemVerRange::from_str("*").unwrap();
        assert!(r.matches(SemVer::new(1, 2, 3)));
        assert!(!r.matches(SemVer::new_prerelease(1, 2, 3, PrereleaseKind::Dev, 1)));
    }

    #[test]
    fn range_release_req_excludes_all_prereleases() {
        // `^1.2.0` must not select `1.2.5-dev.1` (release req, npm policy).
        let caret = SemVerRange::from_str("^1.2.0").unwrap();
        assert!(caret.matches(SemVer::new(1, 2, 5)));
        assert!(!caret.matches(SemVer::new_prerelease(1, 2, 5, PrereleaseKind::Dev, 1)));
        // `>=1.2.0` likewise excludes prereleases of higher cores.
        let at_least = SemVerRange::from_str(">=1.2.0").unwrap();
        assert!(at_least.matches(SemVer::new(1, 3, 0)));
        assert!(!at_least.matches(SemVer::new_prerelease(1, 3, 0, PrereleaseKind::Rc, 1)));
    }

    #[test]
    fn range_prerelease_req_admits_same_core_prereleases() {
        // `>=1.2.0-dev.3`: same-core prereleases >= req, and releases per the
        // core rule; prereleases of a different core are excluded.
        let r = SemVerRange::from_str(">=1.2.0-dev.3").unwrap();
        assert!(r.matches(SemVer::new_prerelease(1, 2, 0, PrereleaseKind::Dev, 5)));
        assert!(r.matches(SemVer::new(1, 2, 0)));
        assert!(r.matches(SemVer::new(1, 2, 1)));
        assert!(!r.matches(SemVer::new_prerelease(1, 2, 0, PrereleaseKind::Dev, 1))); // below req
        assert!(!r.matches(SemVer::new_prerelease(1, 3, 0, PrereleaseKind::Dev, 1))); // other core
        assert!(!r.matches(SemVer::new(1, 1, 9))); // below core
    }

    #[test]
    fn range_caret_prerelease_req() {
        // `^0.4.33-dev.2`: pre-1.0 caret pins the minor; same-core dev/rc >=
        // req match, release 0.4.33 matches, 0.5.0 does not.
        let r = SemVerRange::from_str("^0.4.33-dev.2").unwrap();
        assert!(r.matches(SemVer::new_prerelease(0, 4, 33, PrereleaseKind::Dev, 2)));
        assert!(r.matches(SemVer::new_prerelease(0, 4, 33, PrereleaseKind::Rc, 1)));
        assert!(r.matches(SemVer::new(0, 4, 33)));
        assert!(!r.matches(SemVer::new_prerelease(0, 5, 0, PrereleaseKind::Dev, 1)));
        assert!(!r.matches(SemVer::new_prerelease(0, 4, 33, PrereleaseKind::Dev, 1)));
    }

    #[test]
    fn range_exact_prerelease_matches_only_itself() {
        let r = SemVerRange::from_str("1.2.3-rc.1").unwrap();
        assert_eq!(
            r,
            SemVerRange::Exact(SemVer::new_prerelease(1, 2, 3, PrereleaseKind::Rc, 1))
        );
        assert!(r.matches(SemVer::new_prerelease(1, 2, 3, PrereleaseKind::Rc, 1)));
        assert!(!r.matches(SemVer::new(1, 2, 3)));
        assert!(!r.matches(SemVer::new_prerelease(1, 2, 3, PrereleaseKind::Rc, 2)));
        assert!(!r.matches(SemVer::new_prerelease(1, 2, 3, PrereleaseKind::Dev, 1)));
    }

    #[test]
    fn range_operator_prefixes_parse_with_prerelease() {
        assert_eq!(
            SemVerRange::from_str(">=1.2.0-dev.3").unwrap(),
            SemVerRange::AtLeast(SemVer::new_prerelease(1, 2, 0, PrereleaseKind::Dev, 3))
        );
        assert_eq!(
            SemVerRange::from_str("^0.4.33-dev.2").unwrap(),
            SemVerRange::Caret(SemVer::new_prerelease(0, 4, 33, PrereleaseKind::Dev, 2))
        );
        assert_eq!(
            SemVerRange::from_str("~1.2.3-rc.4").unwrap(),
            SemVerRange::Tilde(SemVer::new_prerelease(1, 2, 3, PrereleaseKind::Rc, 4))
        );
        // Round-trips through Display + YAML.
        let r = SemVerRange::from_str(">=1.2.0-dev.3").unwrap();
        assert_eq!(r.to_string(), ">=1.2.0-dev.3");
        let back: SemVerRange = serde_yaml::from_str(&serde_yaml::to_string(&r).unwrap()).unwrap();
        assert_eq!(r, back);
    }
}
