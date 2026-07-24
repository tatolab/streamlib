// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::error::{ResolverError, ResolverResult};
use crate::semver::SemVer;

/// Content-hash-pinned resolved package set — the wire shape shared by the
/// codegen ([`CODEGEN_LOCKFILE_NAME`]), application ([`APP_LOCKFILE_NAME`]),
/// and per-app modules ([`MODULES_LOCKFILE_NAME`]) lockfiles.
///
/// Wire shape: a single `version: 1` followed by a `packages` map keyed by
/// the canonical `"@org/name"` string. Each entry is the resolved
/// concrete location + content hash. `BTreeMap` (not `HashMap`) keeps the
/// lockfile diff-stable across regenerations.
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
    /// Resolved by version from a package source (a static `.slpkg` tree
    /// reached over `file://` or an HTTP mount). `url` is the concrete
    /// `.slpkg` location the selected version was fetched from. The wire tag
    /// is `by-version` — there is no central registry, only a package source
    /// location.
    #[serde(rename = "by-version")]
    ByVersion {
        url: String,
    },
    Path {
        path: PathBuf,
    },
    /// A local package checkout symlinked into `streamlib_modules/@org/name`
    /// by `streamlib link` — edits in the checkout are live on the next run.
    /// `path` is the canonical symlink target (the linked checkout root).
    Link {
        path: PathBuf,
    },
    Git {
        url: String,
        rev: String,
    },
    /// A remote archive fetched over the wire (`file://` / `http(s)://`).
    /// `archive_sha256` is the lowercase hex SHA-256 of the fetched bytes.
    Url {
        url: String,
        archive_sha256: String,
    },
    /// A local archive file (`.slpkg` / `.zip` / `.tar.gz`).
    /// `archive_sha256` is the lowercase hex SHA-256 of the archive bytes.
    Archive {
        path: PathBuf,
        archive_sha256: String,
    },
}

/// Conventional file name for the **codegen** lockfile — the schema-input
/// reproducibility pin written next to a `streamlib.yaml` by
/// `streamlib generate` / jtd-codegen. Distinct lifecycle from the
/// application lockfile ([`APP_LOCKFILE_NAME`]) and the per-app modules
/// lockfile ([`MODULES_LOCKFILE_NAME`]): codegen pins the *schema* set
/// that reconstructs generated bindings byte-for-byte.
pub const CODEGEN_LOCKFILE_NAME: &str = "streamlib-codegen.lock";

/// Conventional file name for the **per-app modules** lockfile — written
/// next to an app's `streamlib_modules/` folder by `streamlib add` /
/// [`crate::app_modules::AppModulesDir`]. Records each materialized
/// package's identity, source, and content hash.
pub const MODULES_LOCKFILE_NAME: &str = "streamlib.lock";

/// Conventional file name for the **application** lockfile — the runtime
/// package pin set written by `streamlib install` and consumed by a
/// locked run (`Runner::add_modules_from_lockfile`). Shares the on-disk
/// [`Lockfile`] wire shape with the codegen lockfile ([`CODEGEN_LOCKFILE_NAME`])
/// but is a distinct file with a distinct lifecycle — the app lockfile is
/// what a deploy ships and what makes an offline run reproducible.
pub const APP_LOCKFILE_NAME: &str = "streamlib-app.lock";

/// Compute the content hash of a package: SHA-256 over the manifest body
/// plus every schema YAML keyed by relative path.
///
/// `schemas` is `(relative_path, content)` pairs. The function sorts by path
/// before hashing so the result is stable regardless of caller order.
pub fn compute_content_hash(manifest_body: &str, schemas: &[(String, String)]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"streamlib-package-content-hash-v1\n");
    hasher.update(b"manifest:\n");
    hasher.update(manifest_body.as_bytes());
    hasher.update(b"\n");

    let mut sorted: Vec<&(String, String)> = schemas.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    for (path, content) in sorted {
        hasher.update(b"schema:");
        hasher.update(path.as_bytes());
        hasher.update(b"\n");
        hasher.update(content.as_bytes());
        hasher.update(b"\n");
    }

    let digest = hasher.finalize();
    format!("sha256:{:x}", digest)
}

/// SHA-256 over an arbitrary byte slice, namespace-prefixed `sha256:`. Used
/// for one-off content hashing where caller supplies an already-canonical
/// representation.
pub fn hash_content(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("sha256:{:x}", hasher.finalize())
}

/// Codegen-lockfile header (schema-input reproducibility pin).
const CODEGEN_LOCKFILE_HEADER: &str = "\
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
#
# AUTOGENERATED BY `streamlib generate`. DO NOT EDIT BY HAND.
#
# This lockfile pins resolved package versions + content hashes so a fresh
# checkout reconstructs the same generated bindings byte-for-byte.
# Commit it in applications and examples; don't commit it in publishable
# libraries (they inherit their consumer's lock).
";

/// Modules-lockfile header (per-app `streamlib_modules/` record).
const MODULES_LOCKFILE_HEADER: &str = "\
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
#
# AUTOGENERATED BY `streamlib add` / `streamlib link`. DO NOT EDIT BY HAND.
#
# This lockfile records every package materialized into this app's
# streamlib_modules/ folder — identity, source (a copy via `add` or a
# symlink via `link`), and content hash — so the folder's provenance is
# auditable and reproducible. Commit it alongside the app.
";

/// Application-lockfile header (runtime package pin set).
const APP_LOCKFILE_HEADER: &str = "\
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
#
# AUTOGENERATED BY `streamlib install`. DO NOT EDIT BY HAND.
#
# This lockfile pins the exact resolved package versions + content hashes
# an installed application loads. A locked run
# (`Runner::add_modules_from_lockfile`) consumes it strictly from the app's
# co-located streamlib_modules slots and performs NO live re-resolution — so
# the run works offline and is byte-reproducible. Commit it in applications
# and examples; a deploy ships it.
";

/// Write a [`Lockfile`] to disk as YAML behind `header`. The wire shape is
/// identical regardless of header; only the leading comment block differs.
fn write_lockfile_with_header(
    path: &Path,
    lockfile: &Lockfile,
    header: &str,
) -> ResolverResult<()> {
    let body = serde_yaml::to_string(lockfile).map_err(|e| ResolverError::ManifestParse {
        path: path.to_path_buf(),
        source: e,
    })?;
    let mut out = String::with_capacity(header.len() + body.len());
    out.push_str(header);
    out.push_str(&body);
    std::fs::write(path, out).map_err(|e| ResolverError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    Ok(())
}

/// Write the **codegen** lockfile ([`CODEGEN_LOCKFILE_NAME`]) to disk as YAML.
pub fn write_lockfile(path: &Path, lockfile: &Lockfile) -> ResolverResult<()> {
    write_lockfile_with_header(path, lockfile, CODEGEN_LOCKFILE_HEADER)
}

/// Write the **application** lockfile ([`APP_LOCKFILE_NAME`]) to disk as
/// YAML. Same wire shape as [`write_lockfile`]; distinct header + file.
pub fn write_app_lockfile(path: &Path, lockfile: &Lockfile) -> ResolverResult<()> {
    write_lockfile_with_header(path, lockfile, APP_LOCKFILE_HEADER)
}

/// Write the **per-app modules** lockfile ([`MODULES_LOCKFILE_NAME`]) to disk
/// as YAML, atomically (temp sibling + rename) so a concurrent reader never
/// observes a half-written lock.
pub fn write_modules_lockfile(path: &Path, lockfile: &Lockfile) -> ResolverResult<()> {
    let body = serde_yaml::to_string(lockfile).map_err(|e| ResolverError::ManifestParse {
        path: path.to_path_buf(),
        source: e,
    })?;
    let mut out = String::with_capacity(MODULES_LOCKFILE_HEADER.len() + body.len());
    out.push_str(MODULES_LOCKFILE_HEADER);
    out.push_str(&body);

    let io_err = |source: std::io::Error| ResolverError::Io {
        path: path.to_path_buf(),
        source,
    };
    // pid + a process-local counter so two concurrent writers (including two
    // threads in the same process) never share a temp path.
    use std::sync::atomic::{AtomicU64, Ordering};
    static WRITE_SEQ: AtomicU64 = AtomicU64::new(0);
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| MODULES_LOCKFILE_NAME.to_string());
    let tmp = path.with_file_name(format!(
        ".{file_name}.partial-{}-{}",
        std::process::id(),
        WRITE_SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::write(&tmp, &out).map_err(io_err)?;
    std::fs::rename(&tmp, path).map_err(io_err)?;
    Ok(())
}

/// Read a lockfile from disk.
pub fn read_lockfile(path: &Path) -> ResolverResult<Lockfile> {
    let content = std::fs::read_to_string(path).map_err(|e| ResolverError::ManifestRead {
        path: path.to_path_buf(),
        source: e,
    })?;
    serde_yaml::from_str(&content).map_err(|e| ResolverError::ManifestParse {
        path: path.to_path_buf(),
        source: e,
    })
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
      kind: by-version
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
  "@tatolab/camera":
    version: 0.4.33-dev.2
    source:
      kind: by-version
      url: https://packages.streamlib.dev
    content_hash: "sha256:5555666677778888"
  "@tatolab/net":
    version: 0.3.0
    source:
      kind: url
      url: https://example.com/net.slpkg
      archive_sha256: "aabbccdd"
    content_hash: "sha256:9999aaaabbbbcccc"
  "@tatolab/vision":
    version: 0.2.0
    source:
      kind: archive
      path: ./vision.tar.gz
      archive_sha256: "eeff0011"
    content_hash: "sha256:ddddeeeeffff0000"
"#;
        let lock: Lockfile = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(lock.version, 1);
        assert_eq!(lock.packages.len(), 6);

        let core = lock.packages.get("@tatolab/core").unwrap();
        assert_eq!(core.version, SemVer::new(1, 0, 0));
        assert!(matches!(core.source, LockfileSource::ByVersion { .. }));

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

        // A prerelease package version survives the lockfile round-trip.
        use crate::semver::PrereleaseKind;
        let camera = lock.packages.get("@tatolab/camera").unwrap();
        assert_eq!(
            camera.version,
            SemVer::new_prerelease(0, 4, 33, PrereleaseKind::Dev, 2)
        );

        // The archive-flavored sources carry the archive digest.
        let net = lock.packages.get("@tatolab/net").unwrap();
        match &net.source {
            LockfileSource::Url {
                url,
                archive_sha256,
            } => {
                assert_eq!(url, "https://example.com/net.slpkg");
                assert_eq!(archive_sha256, "aabbccdd");
            }
            other => panic!("expected Url source, got {:?}", other),
        }
        let vision = lock.packages.get("@tatolab/vision").unwrap();
        match &vision.source {
            LockfileSource::Archive {
                path,
                archive_sha256,
            } => {
                assert_eq!(path, &PathBuf::from("./vision.tar.gz"));
                assert_eq!(archive_sha256, "eeff0011");
            }
            other => panic!("expected Archive source, got {:?}", other),
        }

        let s = serde_yaml::to_string(&lock).unwrap();
        assert!(s.contains("0.4.33-dev.2"), "serialized: {s}");
        let back: Lockfile = serde_yaml::from_str(&s).unwrap();
        assert_eq!(back.packages.len(), 6);
        assert_eq!(
            back.packages.get("@tatolab/camera").unwrap().version,
            SemVer::new_prerelease(0, 4, 33, PrereleaseKind::Dev, 2)
        );
    }

    #[test]
    fn lockfile_link_source_round_trips() {
        // A `streamlib link` entry serializes as `kind: link` with a `path`
        // and survives the read/write round-trip alongside the other sources.
        let yaml = r#"
version: 1
packages:
  "@tatolab/camera":
    version: 2.1.0
    source:
      kind: link
      path: /home/dev/checkouts/camera
    content_hash: "sha256:abc"
"#;
        let lock: Lockfile = serde_yaml::from_str(yaml).unwrap();
        let camera = lock.packages.get("@tatolab/camera").unwrap();
        match &camera.source {
            LockfileSource::Link { path } => {
                assert_eq!(path, &PathBuf::from("/home/dev/checkouts/camera"));
            }
            other => panic!("expected Link source, got {other:?}"),
        }
        let serialized = serde_yaml::to_string(&lock).unwrap();
        assert!(serialized.contains("kind: link"), "serialized: {serialized}");
        let back: Lockfile = serde_yaml::from_str(&serialized).unwrap();
        assert!(matches!(
            back.packages.get("@tatolab/camera").unwrap().source,
            LockfileSource::Link { .. }
        ));
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
        let aaa_pos = yaml_a.find("@tatolab/aaa").unwrap();
        let zzz_pos = yaml_a.find("@tatolab/zzz").unwrap();
        assert!(aaa_pos < zzz_pos, "BTreeMap must produce sorted output");
    }

    #[test]
    fn content_hash_is_deterministic_across_input_orders() {
        let manifest = "package:\n  org: tatolab\n  name: core\n  version: 1.0.0\n";
        let schemas_a = vec![
            ("schemas/a.yaml".into(), "type: a\n".into()),
            ("schemas/b.yaml".into(), "type: b\n".into()),
        ];
        let schemas_b = vec![
            ("schemas/b.yaml".into(), "type: b\n".into()),
            ("schemas/a.yaml".into(), "type: a\n".into()),
        ];
        let h1 = compute_content_hash(manifest, &schemas_a);
        let h2 = compute_content_hash(manifest, &schemas_b);
        assert_eq!(h1, h2, "input order must not affect content hash");
        assert!(h1.starts_with("sha256:"));
    }

    #[test]
    fn content_hash_changes_on_manifest_change() {
        let m1 = "package:\n  org: tatolab\n  name: core\n  version: 1.0.0\n";
        let m2 = "package:\n  org: tatolab\n  name: core\n  version: 1.0.1\n";
        let schemas: Vec<(String, String)> = vec![];
        let h1 = compute_content_hash(m1, &schemas);
        let h2 = compute_content_hash(m2, &schemas);
        assert_ne!(h1, h2);
    }

    #[test]
    fn content_hash_changes_on_schema_change() {
        let manifest = "package:\n  org: tatolab\n  name: core\n  version: 1.0.0\n";
        let s1 = vec![("schemas/a.yaml".into(), "type: a\n".into())];
        let s2 = vec![("schemas/a.yaml".into(), "type: A\n".into())];
        let h1 = compute_content_hash(manifest, &s1);
        let h2 = compute_content_hash(manifest, &s2);
        assert_ne!(h1, h2);
    }

    #[test]
    fn write_and_read_lockfile_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let mut lock = Lockfile {
            version: 1,
            packages: BTreeMap::new(),
        };
        lock.packages.insert(
            "@tatolab/core".into(),
            LockfileEntry {
                version: SemVer::new(1, 0, 0),
                source: LockfileSource::Path {
                    path: "../core".into(),
                },
                content_hash: "sha256:abc".into(),
            },
        );

        let path = tmp.path().join(CODEGEN_LOCKFILE_NAME);
        write_lockfile(&path, &lock).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains("AUTOGENERATED"));

        let back = read_lockfile(&path).unwrap();
        assert_eq!(back.version, 1);
        assert_eq!(back.packages.len(), 1);
    }

    #[test]
    fn app_lockfile_shares_wire_shape_but_distinct_header() {
        // The app lockfile reuses the `Lockfile` wire shape (round-trips
        // through `read_lockfile`) but carries a distinct, install-scoped
        // header — the codegen-vs-app split. Two writes are byte-identical.
        let tmp = tempfile::tempdir().unwrap();
        let mut lock = Lockfile {
            version: 1,
            packages: BTreeMap::new(),
        };
        lock.packages.insert(
            "@tatolab/core".into(),
            LockfileEntry {
                version: SemVer::new(1, 0, 0),
                source: LockfileSource::ByVersion {
                    url: "file:///m".into(),
                },
                content_hash: "sha256:abc".into(),
            },
        );

        let app = tmp.path().join(APP_LOCKFILE_NAME);
        let codegen = tmp.path().join(CODEGEN_LOCKFILE_NAME);
        write_app_lockfile(&app, &lock).unwrap();
        write_lockfile(&codegen, &lock).unwrap();

        let app_body = std::fs::read_to_string(&app).unwrap();
        let codegen_body = std::fs::read_to_string(&codegen).unwrap();
        assert!(
            app_body.contains("streamlib install"),
            "app header: {app_body}"
        );
        assert!(
            codegen_body.contains("streamlib generate"),
            "codegen header: {codegen_body}"
        );
        assert_ne!(app_body, codegen_body, "headers must differ");

        // The wire payload (below the header) round-trips identically.
        let back = read_lockfile(&app).unwrap();
        assert_eq!(back.packages.len(), 1);
        assert_eq!(
            back.packages.get("@tatolab/core").unwrap().version,
            SemVer::new(1, 0, 0)
        );

        // Byte-determinism: a second write of the same lockfile is identical.
        let app2 = tmp.path().join("streamlib-app-2.lock");
        write_app_lockfile(&app2, &lock).unwrap();
        assert_eq!(std::fs::read(&app).unwrap(), std::fs::read(&app2).unwrap());
    }

    #[test]
    fn modules_lockfile_writes_atomically_with_distinct_header() {
        let tmp = tempfile::tempdir().unwrap();
        let mut lock = Lockfile {
            version: 1,
            packages: BTreeMap::new(),
        };
        lock.packages.insert(
            "@tatolab/camera".into(),
            LockfileEntry {
                version: SemVer::new(2, 0, 0),
                source: LockfileSource::Archive {
                    path: "./camera.slpkg".into(),
                    archive_sha256: "ab".repeat(32),
                },
                content_hash: "sha256:abc".into(),
            },
        );

        let path = tmp.path().join(MODULES_LOCKFILE_NAME);
        write_modules_lockfile(&path, &lock).unwrap();

        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains("streamlib add"), "modules header: {body}");
        assert!(body.contains("streamlib_modules/"), "modules header: {body}");

        // No temp sibling survives the atomic write.
        let leftovers: Vec<_> = std::fs::read_dir(tmp.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n != MODULES_LOCKFILE_NAME)
            .collect();
        assert!(leftovers.is_empty(), "leftover temp files: {leftovers:?}");

        // Round-trips through the shared reader, and re-writing is
        // byte-identical (diff-stable).
        let back = read_lockfile(&path).unwrap();
        assert_eq!(back.packages.len(), 1);
        let path2 = tmp.path().join("second.lock");
        write_modules_lockfile(&path2, &lock).unwrap();
        assert_eq!(
            std::fs::read(&path).unwrap(),
            std::fs::read(&path2).unwrap()
        );
    }

    #[test]
    fn write_lockfile_byte_identical_for_same_input() {
        // Two writes from the same Lockfile must produce byte-identical files.
        let tmp = tempfile::tempdir().unwrap();
        let lock = Lockfile {
            version: 1,
            packages: BTreeMap::new(),
        };
        let p1 = tmp.path().join("a.lock");
        let p2 = tmp.path().join("b.lock");
        write_lockfile(&p1, &lock).unwrap();
        write_lockfile(&p2, &lock).unwrap();
        let a = std::fs::read(&p1).unwrap();
        let b = std::fs::read(&p2).unwrap();
        assert_eq!(a, b);
    }
}
