// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Integrity-verified reuse of `cargo package` crate tarballs:
//! [`verify_crate_tarball`] structurally validates a `.crate` before reuse,
//! [`obtain_crate_tarball`] reuses a verified one or repackages on failure.

use std::io::Read;
use std::path::Path;

use anyhow::Context;
use flate2::read::GzDecoder;
use sha2::{Digest, Sha256};

/// Why a `.crate` tarball failed integrity verification.
#[derive(Debug, thiserror::Error)]
pub enum CrateTarballIntegrityError {
    /// The tarball bytes could not be read from disk.
    #[error("read crate tarball {path}: {source}")]
    Unreadable {
        path: String,
        source: std::io::Error,
    },
    /// The gzip container is invalid or truncated (decode failed before the
    /// gzip trailer's CRC + length could be validated).
    #[error("crate tarball gzip stream invalid or truncated: {0}")]
    GzipInvalid(std::io::Error),
    /// The tar archive is invalid or truncated (a short entry / missing block
    /// surfaces here once the gzip layer decodes).
    #[error("crate tarball tar archive invalid or truncated: {0}")]
    TarInvalid(std::io::Error),
    /// The required `{name}-{version}/Cargo.toml` entry is absent.
    #[error("crate tarball missing manifest entry `{expected}`")]
    MissingManifestEntry { expected: String },
    /// The recorded sha256 did not match the tarball bytes.
    #[error("crate tarball sha256 mismatch: expected {expected}, got {actual}")]
    ChecksumMismatch { expected: String, actual: String },
}

/// Structurally verify a `cargo package` `.crate` at `path` for
/// `(name, version)`: the gzip stream fully decodes (truncation trips the
/// trailer check), every tar entry enumerates to EOF, the crate's
/// `{name}-{version}/Cargo.toml` entry is present, and — when `expected_sha256`
/// is given — the tarball bytes hash to it. The checksum arm is exercised by
/// the tests and reserved for the byte-stable-emission follow-up; the emit
/// reuse path calls with `None`.
pub fn verify_crate_tarball(
    path: &Path,
    name: &str,
    version: &str,
    expected_sha256: Option<&str>,
) -> Result<(), CrateTarballIntegrityError> {
    let bytes = std::fs::read(path).map_err(|source| CrateTarballIntegrityError::Unreadable {
        path: path.display().to_string(),
        source,
    })?;

    if let Some(expected) = expected_sha256 {
        let mut hasher = Sha256::new();
        hasher.update(&bytes);
        let actual = format!("{:x}", hasher.finalize());
        if !actual.eq_ignore_ascii_case(expected) {
            return Err(CrateTarballIntegrityError::ChecksumMismatch {
                expected: expected.to_string(),
                actual,
            });
        }
    }

    // Fully decode the gzip stream. A truncated tarball trips here: the
    // decoder reaches an unexpected EOF before the gzip trailer (CRC + ISIZE)
    // can be validated. Reading to EOF — rather than stopping at the first
    // manifest match — is what makes a truncation *after* the manifest entry
    // still fail.
    let mut decoded = Vec::new();
    GzDecoder::new(&bytes[..])
        .read_to_end(&mut decoded)
        .map_err(CrateTarballIntegrityError::GzipInvalid)?;

    // Walk every tar entry to EOF, requiring the crate's Cargo.toml. A tar
    // truncated after the manifest header (but the gzip layer still complete)
    // surfaces as a short read here.
    let expected_entry = format!("{name}-{version}/Cargo.toml");
    let mut archive = tar::Archive::new(&decoded[..]);
    let mut found_manifest = false;
    let entries = archive
        .entries()
        .map_err(CrateTarballIntegrityError::TarInvalid)?;
    for entry in entries {
        let entry = entry.map_err(CrateTarballIntegrityError::TarInvalid)?;
        let entry_path = entry
            .path()
            .map_err(CrateTarballIntegrityError::TarInvalid)?;
        if entry_path.to_string_lossy() == expected_entry {
            found_manifest = true;
        }
    }
    if !found_manifest {
        return Err(CrateTarballIntegrityError::MissingManifestEntry {
            expected: expected_entry,
        });
    }
    Ok(())
}

/// How [`obtain_crate_tarball`] produced the verified tarball.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrateTarballProvenance {
    /// A pre-existing candidate verified and was reused — `repackage` never ran.
    ReusedVerified,
    /// The tarball was (re)packaged via `repackage`. `discarded_corrupt` is
    /// true when a pre-existing candidate failed verification and was deleted
    /// first.
    Repackaged { discarded_corrupt: bool },
}

/// Ensure a verified `.crate` exists at `candidate` for `(name, version)`,
/// reusing a structurally-valid pre-existing tarball or invoking `repackage`
/// to produce a fresh one.
///
/// - candidate verifies → reuse, `repackage` is skipped.
/// - candidate exists but fails → log the typed reason, delete it, `repackage`,
///   and verify the fresh artifact.
/// - candidate absent → `repackage` and verify the fresh artifact.
///
/// A freshly-packaged artifact that still fails verification is a hard error
/// (a `cargo package` / toolchain bug), surfaced loudly rather than emitted
/// into the tree.
pub fn obtain_crate_tarball(
    candidate: &Path,
    name: &str,
    version: &str,
    repackage: impl FnOnce() -> anyhow::Result<()>,
) -> anyhow::Result<CrateTarballProvenance> {
    let discarded_corrupt = if candidate.is_file() {
        match verify_crate_tarball(candidate, name, version, None) {
            Ok(()) => {
                tracing::debug!(
                    crate_name = name,
                    version,
                    path = %candidate.display(),
                    "reusing verified crate tarball"
                );
                return Ok(CrateTarballProvenance::ReusedVerified);
            }
            Err(error) => {
                tracing::warn!(
                    crate_name = name,
                    version,
                    path = %candidate.display(),
                    %error,
                    "cached crate tarball failed integrity verification; repackaging"
                );
                std::fs::remove_file(candidate).with_context(|| {
                    format!("remove corrupt crate tarball {}", candidate.display())
                })?;
                true
            }
        }
    } else {
        false
    };

    repackage().with_context(|| format!("repackage crate {name} {version}"))?;

    verify_crate_tarball(candidate, name, version, None).with_context(|| {
        format!(
            "freshly-packaged crate tarball at {} failed integrity verification",
            candidate.display()
        )
    })?;

    Ok(CrateTarballProvenance::Repackaged { discarded_corrupt })
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::write::GzEncoder;
    use flate2::Compression;
    use std::cell::Cell;
    use std::io::Write;
    use std::path::PathBuf;
    use tempfile::tempdir;

    /// Serialize tar entries into raw (uncompressed) tar bytes.
    fn build_tar_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (name, data) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            builder.append_data(&mut header, name, *data).unwrap();
        }
        builder.into_inner().unwrap()
    }

    /// gzip `raw` into a `.crate`-shaped file at `path`.
    fn gzip_to_file(path: &Path, raw: &[u8]) {
        let file = std::fs::File::create(path).unwrap();
        let mut enc = GzEncoder::new(file, Compression::default());
        enc.write_all(raw).unwrap();
        enc.finish().unwrap();
    }

    fn manifest_bytes(name: &str, version: &str) -> String {
        format!("[package]\nname = \"{name}\"\nversion = \"{version}\"\n")
    }

    /// Write a valid `.crate` (Cargo.toml + a source file) at `path`.
    fn write_valid_crate(path: &Path, name: &str, version: &str) {
        let manifest = manifest_bytes(name, version);
        let manifest_entry = format!("{name}-{version}/Cargo.toml");
        let lib_entry = format!("{name}-{version}/src/lib.rs");
        let raw = build_tar_bytes(&[
            (manifest_entry.as_str(), manifest.as_bytes()),
            (lib_entry.as_str(), b"// lib\n"),
        ]);
        gzip_to_file(path, &raw);
    }

    fn candidate_path(dir: &Path, name: &str, version: &str) -> PathBuf {
        dir.join(format!("{name}-{version}.crate"))
    }

    #[test]
    fn valid_tarball_verifies() {
        let dir = tempdir().unwrap();
        let path = candidate_path(dir.path(), "streamlib-x", "0.5.0");
        write_valid_crate(&path, "streamlib-x", "0.5.0");
        verify_crate_tarball(&path, "streamlib-x", "0.5.0", None).unwrap();
    }

    #[test]
    fn obtain_reuses_verified_without_repackaging() {
        let dir = tempdir().unwrap();
        let path = candidate_path(dir.path(), "streamlib-x", "0.5.0");
        write_valid_crate(&path, "streamlib-x", "0.5.0");

        let repackaged = Cell::new(false);
        let provenance = obtain_crate_tarball(&path, "streamlib-x", "0.5.0", || {
            repackaged.set(true);
            Ok(())
        })
        .unwrap();

        assert_eq!(provenance, CrateTarballProvenance::ReusedVerified);
        assert!(!repackaged.get(), "repackage closure must NOT run for a verified reuse");
    }

    /// Core negative test: a truncated cached tarball is discarded and
    /// repackaged. Mentally reverting the fix (verify → `is_file()`) makes the
    /// truncated file "trusted", so `repackage` would never run and this test
    /// fails.
    #[test]
    fn obtain_repackages_truncated_cached_tarball() {
        let dir = tempdir().unwrap();
        let path = candidate_path(dir.path(), "streamlib-x", "0.5.0");
        write_valid_crate(&path, "streamlib-x", "0.5.0");

        // Truncate to a small prefix — mid gzip stream, unmistakably corrupt.
        let full = std::fs::metadata(&path).unwrap().len();
        assert!(full > 40, "sanity: a real crate tarball is larger than the gzip header");
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(20)
            .unwrap();

        // The truncated file is genuinely rejected by the verifier.
        assert!(verify_crate_tarball(&path, "streamlib-x", "0.5.0", None).is_err());

        let repackaged = Cell::new(false);
        let provenance = obtain_crate_tarball(&path, "streamlib-x", "0.5.0", || {
            repackaged.set(true);
            write_valid_crate(&path, "streamlib-x", "0.5.0");
            Ok(())
        })
        .unwrap();

        assert!(repackaged.get(), "repackage MUST run for a corrupt cached tarball");
        assert_eq!(
            provenance,
            CrateTarballProvenance::Repackaged { discarded_corrupt: true }
        );
        // Final artifact is valid.
        verify_crate_tarball(&path, "streamlib-x", "0.5.0", None).unwrap();
    }

    #[test]
    fn zero_byte_tarball_fails_verification() {
        let dir = tempdir().unwrap();
        let path = candidate_path(dir.path(), "streamlib-x", "0.5.0");
        std::fs::write(&path, b"").unwrap();
        let err = verify_crate_tarball(&path, "streamlib-x", "0.5.0", None).unwrap_err();
        assert!(matches!(err, CrateTarballIntegrityError::GzipInvalid(_)), "got {err:?}");
    }

    #[test]
    fn tarball_missing_manifest_entry_fails() {
        let dir = tempdir().unwrap();
        let path = candidate_path(dir.path(), "streamlib-x", "0.5.0");
        // A well-formed gzip-tar with a source file but NO Cargo.toml.
        let raw = build_tar_bytes(&[("streamlib-x-0.5.0/src/lib.rs", b"// lib\n")]);
        gzip_to_file(&path, &raw);
        let err = verify_crate_tarball(&path, "streamlib-x", "0.5.0", None).unwrap_err();
        match err {
            CrateTarballIntegrityError::MissingManifestEntry { expected } => {
                assert_eq!(expected, "streamlib-x-0.5.0/Cargo.toml");
            }
            other => panic!("expected MissingManifestEntry, got {other:?}"),
        }
    }

    #[test]
    fn wrong_expected_sha256_mismatches() {
        let dir = tempdir().unwrap();
        let path = candidate_path(dir.path(), "streamlib-x", "0.5.0");
        write_valid_crate(&path, "streamlib-x", "0.5.0");
        let err = verify_crate_tarball(
            &path,
            "streamlib-x",
            "0.5.0",
            Some("0000000000000000000000000000000000000000000000000000000000000000"),
        )
        .unwrap_err();
        assert!(matches!(err, CrateTarballIntegrityError::ChecksumMismatch { .. }), "got {err:?}");
    }

    #[test]
    fn correct_expected_sha256_verifies() {
        let dir = tempdir().unwrap();
        let path = candidate_path(dir.path(), "streamlib-x", "0.5.0");
        write_valid_crate(&path, "streamlib-x", "0.5.0");
        let bytes = std::fs::read(&path).unwrap();
        let mut hasher = Sha256::new();
        hasher.update(&bytes);
        let sha = format!("{:x}", hasher.finalize());
        verify_crate_tarball(&path, "streamlib-x", "0.5.0", Some(&sha)).unwrap();
    }

    #[test]
    fn absent_candidate_repackages_fresh() {
        let dir = tempdir().unwrap();
        let path = candidate_path(dir.path(), "streamlib-x", "0.5.0");
        assert!(!path.exists());

        let repackaged = Cell::new(false);
        let provenance = obtain_crate_tarball(&path, "streamlib-x", "0.5.0", || {
            repackaged.set(true);
            write_valid_crate(&path, "streamlib-x", "0.5.0");
            Ok(())
        })
        .unwrap();

        assert!(repackaged.get());
        assert_eq!(
            provenance,
            CrateTarballProvenance::Repackaged { discarded_corrupt: false }
        );
        verify_crate_tarball(&path, "streamlib-x", "0.5.0", None).unwrap();
    }

    #[test]
    fn repackage_producing_still_corrupt_is_hard_error() {
        let dir = tempdir().unwrap();
        let path = candidate_path(dir.path(), "streamlib-x", "0.5.0");
        assert!(!path.exists());

        let result = obtain_crate_tarball(&path, "streamlib-x", "0.5.0", || {
            // "Repackage" writes garbage — the freshly-packaged verify must
            // fail loud rather than emit a corrupt tarball.
            std::fs::write(&path, b"not a gzip tarball").unwrap();
            Ok(())
        });
        assert!(result.is_err(), "a still-corrupt repackage must be a hard error");
    }

    /// The verifier reads the tar layer to EOF: a tar truncated *after* the
    /// manifest entry (with the gzip layer intact) still fails. Guards against
    /// an implementation that returns early on the first manifest match.
    #[test]
    fn tar_truncation_after_manifest_entry_fails() {
        let dir = tempdir().unwrap();
        let path = candidate_path(dir.path(), "streamlib-x", "0.5.0");

        let manifest = manifest_bytes("streamlib-x", "0.5.0");
        let big = vec![0x41u8; 8192];
        let manifest_entry = "streamlib-x-0.5.0/Cargo.toml";
        let big_entry = "streamlib-x-0.5.0/src/big.rs";
        let raw = build_tar_bytes(&[
            (manifest_entry, manifest.as_bytes()),
            (big_entry, big.as_slice()),
        ]);
        // Layout (short paths fit the 512-byte name field, no long-name block):
        //   [0..512)     manifest header
        //   [512..1024)  manifest data block
        //   [1024..1536) big-entry header
        //   [1536..)     big-entry data (16 blocks)
        // Cut mid big-entry data — the manifest is fully present, but the walk
        // to EOF hits a short read after yielding the second entry.
        let keep = 1536 + 256;
        gzip_to_file(&path, &raw[..keep]);

        let err = verify_crate_tarball(&path, "streamlib-x", "0.5.0", None).unwrap_err();
        assert!(matches!(err, CrateTarballIntegrityError::TarInvalid(_)), "got {err:?}");
    }

    /// A gzip stream whose trailer is truncated fails even though every entry's
    /// data is present — the gzip `read_to_end` validates the CRC + length
    /// footer, so reuse can't trust a stream that was cut at the very end.
    #[test]
    fn gzip_trailer_truncation_fails() {
        let dir = tempdir().unwrap();
        let path = candidate_path(dir.path(), "streamlib-x", "0.5.0");
        write_valid_crate(&path, "streamlib-x", "0.5.0");
        let full = std::fs::metadata(&path).unwrap().len();
        // Drop the 8-byte gzip trailer (CRC32 + ISIZE).
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(full - 8)
            .unwrap();
        let err = verify_crate_tarball(&path, "streamlib-x", "0.5.0", None).unwrap_err();
        assert!(matches!(err, CrateTarballIntegrityError::GzipInvalid(_)), "got {err:?}");
    }
}
