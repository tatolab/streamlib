// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::semver::SemVer;

/// `streamlib.lock` — content-hash-pinned resolved package set.
///
/// Wire shape: a single `version: 1` followed by a `packages` map keyed by
/// the canonical `"@org/name"` string. Each entry is the resolved
/// concrete location + content hash so a fresh checkout reconstructs the
/// same generated bindings byte-for-byte. `BTreeMap` (not `HashMap`)
/// keeps the lockfile diff-stable across regenerations.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Lockfile {
    /// Lockfile schema version. Currently `1`.
    pub version: u32,

    pub packages: BTreeMap<String, LockfileEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LockfileEntry {
    pub version: SemVer,
    pub source: LockfileSource,
    /// Content hash of the resolved package contents (typically sha256:hex).
    /// Includes the namespacing prefix so future hash algorithms can land
    /// without breaking lockfile parsing.
    pub content_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LockfileSource {
    Registry { url: String },
    Path { path: PathBuf },
    Git { url: String, rev: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lockfile_round_trip() {
        let yaml = r#"
version: 1
packages:
  "@tatolab/core":
    version: 1.0.0
    source:
      kind: registry
      url: https://packages.streamlib.dev
    content_hash: "sha256:0123456789abcdef"
  "@tatolab/h264":
    version: 0.4.2
    source:
      kind: path
      path: ../h264
    content_hash: "sha256:fedcba9876543210"
  "@tatolab/moq":
    version: 0.2.0
    source:
      kind: git
      url: https://github.com/tatolab/moq
      rev: abc123def456
    content_hash: "sha256:1111222233334444"
"#;
        let lock: Lockfile = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(lock.version, 1);
        assert_eq!(lock.packages.len(), 3);

        let core = lock.packages.get("@tatolab/core").unwrap();
        assert_eq!(core.version, SemVer::new(1, 0, 0));
        assert!(matches!(core.source, LockfileSource::Registry { .. }));

        let h264 = lock.packages.get("@tatolab/h264").unwrap();
        assert!(matches!(h264.source, LockfileSource::Path { .. }));

        let moq = lock.packages.get("@tatolab/moq").unwrap();
        match &moq.source {
            LockfileSource::Git { url, rev } => {
                assert_eq!(url, "https://github.com/tatolab/moq");
                assert_eq!(rev, "abc123def456");
            }
            other => panic!("expected Git source, got {:?}", other),
        }

        // Re-serialize and re-parse — same shape.
        let s = serde_yaml::to_string(&lock).unwrap();
        let back: Lockfile = serde_yaml::from_str(&s).unwrap();
        assert_eq!(back.packages.len(), 3);
        assert_eq!(back.packages.get("@tatolab/core").unwrap().version, SemVer::new(1, 0, 0));
    }

    #[test]
    fn lockfile_rejects_unknown_source_kind() {
        let yaml = r#"
version: 1
packages:
  "@tatolab/core":
    version: 1.0.0
    source:
      kind: ftp
      url: ftp://example.com
    content_hash: "sha256:abcd"
"#;
        let res: Result<Lockfile, _> = serde_yaml::from_str(yaml);
        assert!(res.is_err());
    }

    #[test]
    fn lockfile_keys_are_diff_stable() {
        // BTreeMap iteration is sorted; ensure two builds produce the same
        // textual lockfile no matter what order packages were inserted.
        let mut a = Lockfile {
            version: 1,
            packages: BTreeMap::new(),
        };
        a.packages.insert(
            "@tatolab/zzz".into(),
            LockfileEntry {
                version: SemVer::new(1, 0, 0),
                source: LockfileSource::Path { path: "z".into() },
                content_hash: "sha256:1".into(),
            },
        );
        a.packages.insert(
            "@tatolab/aaa".into(),
            LockfileEntry {
                version: SemVer::new(1, 0, 0),
                source: LockfileSource::Path { path: "a".into() },
                content_hash: "sha256:2".into(),
            },
        );

        let yaml_a = serde_yaml::to_string(&a).unwrap();
        // The "aaa" key must come before "zzz" in the serialized form.
        let aaa_pos = yaml_a.find("@tatolab/aaa").unwrap();
        let zzz_pos = yaml_a.find("@tatolab/zzz").unwrap();
        assert!(aaa_pos < zzz_pos, "BTreeMap must produce sorted output");
    }
}
