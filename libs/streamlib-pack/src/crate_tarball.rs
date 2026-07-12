// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Integrity-verified reuse + byte-stable normalization of `cargo package`
//! crate tarballs.
//!
//! - [`verify_crate_tarball`] structurally validates a `.crate` before reuse,
//!   [`obtain_crate_tarball`] reuses a verified one or repackages on failure.
//! - [`normalize_crate_tarball`] rewrites a `.crate` into a form whose bytes are
//!   a pure function of source content, and [`finalize_crate_tarball`] pairs
//!   that with an immutability guard so re-emitting an unchanged release yields
//!   identical checksums while a source change under a published version is
//!   refused. `cargo package` is already byte-deterministic *except* for the
//!   `{name}-{version}/.cargo_vcs_info.json` entry, whose embedded git-HEAD
//!   sha1 makes the checksum a function of the commit rather than the source;
//!   normalization strips it (see
//!   `docs/learnings/cargo-crate-vcs-info-nondeterminism.md`).

use std::io::{Read, Write};
use std::path::Path;

use anyhow::Context;
use flate2::read::GzDecoder;
use flate2::{Compression, GzBuilder};
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

/// The vcs-info entry cargo embeds inside a `.crate` when packaging from a git
/// checkout with commits: `{name}-{version}/.cargo_vcs_info.json`. Its
/// `{"git":{"sha1":...}}` payload tracks git HEAD, not source, and is the sole
/// per-commit non-determinism vector in an otherwise byte-deterministic
/// `cargo package`.
fn vcs_info_entry_name(name: &str, version: &str) -> String {
    format!("{name}-{version}/.cargo_vcs_info.json")
}

/// Decode a `.crate` at `path`, drop the `.cargo_vcs_info.json` entry, and
/// re-serialize the surviving entries — headers cloned verbatim from cargo's
/// output, in cargo's original order — into canonical *uncompressed* tar bytes.
/// The result is a pure function of the crate's source content: independent of
/// git HEAD (vcs-info removed) and of gzip settings (uncompressed).
fn canonical_tar_bytes(path: &Path, name: &str, version: &str) -> anyhow::Result<Vec<u8>> {
    let bytes =
        std::fs::read(path).with_context(|| format!("read crate tarball {}", path.display()))?;
    let mut decoded = Vec::new();
    GzDecoder::new(&bytes[..])
        .read_to_end(&mut decoded)
        .with_context(|| format!("gzip-decode crate tarball {}", path.display()))?;

    let vcs_entry = vcs_info_entry_name(name, version);
    let mut archive = tar::Archive::new(&decoded[..]);
    let mut builder = tar::Builder::new(Vec::new());
    let entries = archive
        .entries()
        .with_context(|| format!("read crate tar entries {}", path.display()))?;
    for entry in entries {
        let mut entry = entry.context("read crate tar entry")?;
        let entry_path = entry.path().context("read crate tar entry path")?.into_owned();
        if entry_path.to_string_lossy() == vcs_entry {
            continue;
        }
        // Clone cargo's header verbatim (inherits its canonical mtime / mode /
        // uid / gid / entry type / size), then re-emit via `append_data` so the
        // long-name (GNU extension) handling is applied for the resolved path.
        let mut header = entry.header().clone();
        let mut data = Vec::new();
        entry.read_to_end(&mut data).context("read crate tar entry data")?;
        builder
            .append_data(&mut header, &entry_path, &data[..])
            .context("re-append crate tar entry")?;
    }
    builder.into_inner().context("finalize canonical crate tar")
}

/// Rewrite the `.crate` at `path` into its byte-stable canonical form for
/// `(name, version)`: strip `.cargo_vcs_info.json` and re-gzip with a fixed
/// header (MTIME 0, no embedded filename). Idempotent — re-normalizing an
/// already-normalized crate reproduces identical bytes, so the emit reuse path
/// can re-run it on a cached tarball.
pub fn normalize_crate_tarball(path: &Path, name: &str, version: &str) -> anyhow::Result<()> {
    let canonical = canonical_tar_bytes(path, name, version)?;
    let mut encoder = GzBuilder::new()
        .mtime(0)
        .write(Vec::new(), Compression::default());
    encoder
        .write_all(&canonical)
        .context("gzip canonical crate tar")?;
    let gzipped = encoder.finish().context("finish gzip canonical crate tar")?;
    std::fs::write(path, &gzipped)
        .with_context(|| format!("write normalized crate tarball {}", path.display()))?;
    Ok(())
}

/// The sha256 of the crate's canonical (vcs-stripped, uncompressed) tar bytes —
/// a compression-independent fingerprint of source content. Two crates built
/// from identical source but a different git HEAD (or a different gzip level)
/// share a fingerprint; a real source change is a different fingerprint.
pub fn crate_content_fingerprint(
    path: &Path,
    name: &str,
    version: &str,
) -> anyhow::Result<String> {
    let canonical = canonical_tar_bytes(path, name, version)?;
    let mut hasher = Sha256::new();
    hasher.update(&canonical);
    Ok(format!("{:x}", hasher.finalize()))
}

/// Normalize the `.crate` at `path` into byte-stable form, refuse a source
/// change under an already-published `(name, version)`, and return the
/// normalized tarball's sha256 for the sparse-index line.
///
/// `previously_served` is the same crate's `.crate` in the prior complete
/// served tree (still present during a staged emit). When it exists, its
/// content fingerprint — normalized on the fly, so a legacy un-normalized
/// served tree does not false-positive during the transition — must equal the
/// new crate's; a mismatch means the source changed without a version bump and
/// is refused. A benign commit bump (identical source, new git HEAD) passes and
/// yields the same checksum, so it neither churns the index nor trips the guard.
pub fn finalize_crate_tarball(
    path: &Path,
    name: &str,
    version: &str,
    previously_served: Option<&Path>,
) -> anyhow::Result<String> {
    normalize_crate_tarball(path, name, version)?;

    if let Some(served) = previously_served {
        let new_fingerprint = crate_content_fingerprint(path, name, version)?;
        let served_fingerprint = crate_content_fingerprint(served, name, version)?;
        if new_fingerprint != served_fingerprint {
            anyhow::bail!(
                "crate `{name}` `{version}` changed at the same version \
                 (content fingerprint {new_fingerprint} != published \
                 {served_fingerprint}) — bump the version; re-emitting different \
                 source under a published version is refused"
            );
        }
    }

    let bytes = std::fs::read(path)
        .with_context(|| format!("read normalized crate tarball {}", path.display()))?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Ok(format!("{:x}", hasher.finalize()))
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

    /// gzip `raw` into a `.crate`-shaped file at `path` at an explicit
    /// compression level (for the compression-independence fingerprint test).
    fn gzip_to_file_at_level(path: &Path, raw: &[u8], level: Compression) {
        let file = std::fs::File::create(path).unwrap();
        let mut enc = GzEncoder::new(file, level);
        enc.write_all(raw).unwrap();
        enc.finish().unwrap();
    }

    /// Write a `.crate` that mirrors what `cargo package` emits from a git
    /// checkout: a `.cargo_vcs_info.json` entry (carrying `vcs_sha1`) plus the
    /// manifest and a source file whose bytes are `lib_contents`.
    fn write_crate_with_vcs(
        path: &Path,
        name: &str,
        version: &str,
        lib_contents: &[u8],
        vcs_sha1: &str,
    ) {
        let manifest = manifest_bytes(name, version);
        let vcs_json =
            format!("{{\n  \"git\": {{\n    \"sha1\": \"{vcs_sha1}\"\n  }},\n  \"path_in_vcs\": \"\"\n}}");
        let vcs_entry = format!("{name}-{version}/.cargo_vcs_info.json");
        let manifest_entry = format!("{name}-{version}/Cargo.toml");
        let lib_entry = format!("{name}-{version}/src/lib.rs");
        // Match cargo's alphabetical entry order (.cargo_vcs_info.json sorts
        // before Cargo.toml sorts before src/lib.rs).
        let raw = build_tar_bytes(&[
            (vcs_entry.as_str(), vcs_json.as_bytes()),
            (manifest_entry.as_str(), manifest.as_bytes()),
            (lib_entry.as_str(), lib_contents),
        ]);
        gzip_to_file(path, &raw);
    }

    /// The set of entry paths inside a `.crate` (for asserting vcs-info removal).
    fn crate_entry_names(path: &Path) -> Vec<String> {
        let bytes = std::fs::read(path).unwrap();
        let mut decoded = Vec::new();
        GzDecoder::new(&bytes[..]).read_to_end(&mut decoded).unwrap();
        let mut archive = tar::Archive::new(&decoded[..]);
        archive
            .entries()
            .unwrap()
            .map(|e| e.unwrap().path().unwrap().to_string_lossy().into_owned())
            .collect()
    }

    fn sha256_of_file(path: &Path) -> String {
        let bytes = std::fs::read(path).unwrap();
        let mut hasher = Sha256::new();
        hasher.update(&bytes);
        format!("{:x}", hasher.finalize())
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

    /// Normalizing strips `.cargo_vcs_info.json`, leaves a still-valid `.crate`,
    /// and is idempotent (a second normalize reproduces byte-identical output).
    #[test]
    fn normalize_strips_vcs_info_and_is_idempotent() {
        let dir = tempdir().unwrap();
        let path = candidate_path(dir.path(), "streamlib-x", "0.5.0");
        write_crate_with_vcs(&path, "streamlib-x", "0.5.0", b"// lib A\n", "aaaaaaaa");
        assert!(
            crate_entry_names(&path)
                .iter()
                .any(|n| n.ends_with("/.cargo_vcs_info.json")),
            "sanity: the pre-normalize crate carries a vcs-info entry"
        );

        normalize_crate_tarball(&path, "streamlib-x", "0.5.0").unwrap();

        let names = crate_entry_names(&path);
        assert!(
            !names.iter().any(|n| n.ends_with("/.cargo_vcs_info.json")),
            "vcs-info entry must be stripped, got {names:?}"
        );
        // The normalized crate is still structurally valid (manifest present).
        verify_crate_tarball(&path, "streamlib-x", "0.5.0", None).unwrap();

        let after_first = std::fs::read(&path).unwrap();
        normalize_crate_tarball(&path, "streamlib-x", "0.5.0").unwrap();
        let after_second = std::fs::read(&path).unwrap();
        assert_eq!(
            after_first, after_second,
            "normalize must be idempotent — a second pass reproduces identical bytes"
        );
    }

    /// KEY (exit-criterion 1): two crates with identical source but different
    /// git HEAD sha1 normalize to byte-identical `.crate`s with equal checksums.
    /// Mentally revert the vcs-strip and the two would differ at the vcs entry.
    #[test]
    fn two_vcs_variants_normalize_identical() {
        let dir = tempdir().unwrap();
        let a = candidate_path(dir.path(), "streamlib-x", "0.5.0");
        let b = dir.path().join("streamlib-x-0.5.0-b.crate");
        write_crate_with_vcs(&a, "streamlib-x", "0.5.0", b"// same source\n", "aaaaaaaa");
        write_crate_with_vcs(&b, "streamlib-x", "0.5.0", b"// same source\n", "bbbbbbbb");
        // Sanity: the two raw crates differ (the vcs sha1 is baked in).
        assert_ne!(
            std::fs::read(&a).unwrap(),
            std::fs::read(&b).unwrap(),
            "sanity: differing git HEAD makes the raw crates differ"
        );

        normalize_crate_tarball(&a, "streamlib-x", "0.5.0").unwrap();
        normalize_crate_tarball(&b, "streamlib-x", "0.5.0").unwrap();

        assert_eq!(
            std::fs::read(&a).unwrap(),
            std::fs::read(&b).unwrap(),
            "identical source must normalize to byte-identical crates"
        );
        assert_eq!(sha256_of_file(&a), sha256_of_file(&b));
    }

    /// NEGATIVE (exit-criterion 2): a changed source under the same version is
    /// refused. Mentally revert the guard (drop the fingerprint compare) and
    /// this returns Ok instead of Err.
    #[test]
    fn guard_refuses_changed_source_same_version() {
        let dir = tempdir().unwrap();
        let served = candidate_path(dir.path(), "streamlib-x", "0.5.0");
        let fresh = dir.path().join("streamlib-x-0.5.0-fresh.crate");
        write_crate_with_vcs(&served, "streamlib-x", "0.5.0", b"// source A\n", "aaaaaaaa");
        write_crate_with_vcs(&fresh, "streamlib-x", "0.5.0", b"// source B - CHANGED\n", "bbbbbbbb");

        let err = finalize_crate_tarball(&fresh, "streamlib-x", "0.5.0", Some(served.as_path()))
            .unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("changed at the same version"),
            "expected the immutability-guard message, got: {msg}"
        );
    }

    /// A benign commit bump (identical source, different git HEAD) passes the
    /// guard even when the served tree is a legacy *un-normalized* crate, and
    /// the returned checksum is the stable normalized one.
    #[test]
    fn guard_allows_same_source_different_vcs() {
        let dir = tempdir().unwrap();
        let served = candidate_path(dir.path(), "streamlib-x", "0.5.0");
        let served_copy = dir.path().join("streamlib-x-0.5.0-servedcopy.crate");
        let fresh = dir.path().join("streamlib-x-0.5.0-fresh.crate");
        // Served tree is legacy (un-normalized: still carries vcs-info).
        write_crate_with_vcs(&served, "streamlib-x", "0.5.0", b"// stable source\n", "aaaaaaaa");
        std::fs::copy(&served, &served_copy).unwrap();
        write_crate_with_vcs(&fresh, "streamlib-x", "0.5.0", b"// stable source\n", "bbbbbbbb");

        // The normalized checksum the served tree *should* carry.
        normalize_crate_tarball(&served_copy, "streamlib-x", "0.5.0").unwrap();
        let served_normalized_cksum = sha256_of_file(&served_copy);

        let cksum = finalize_crate_tarball(&fresh, "streamlib-x", "0.5.0", Some(served.as_path()))
            .expect("identical source under a legacy served tree must pass the guard");
        assert_eq!(
            cksum, served_normalized_cksum,
            "a benign commit bump must yield the same normalized checksum (no index churn)"
        );
    }

    /// A first emit into a fresh tree (no prior served crate) is always allowed
    /// and still returns the normalized checksum.
    #[test]
    fn guard_absent_served_is_ok() {
        let dir = tempdir().unwrap();
        let fresh = candidate_path(dir.path(), "streamlib-x", "0.5.0");
        write_crate_with_vcs(&fresh, "streamlib-x", "0.5.0", b"// lib\n", "aaaaaaaa");

        let cksum = finalize_crate_tarball(&fresh, "streamlib-x", "0.5.0", None).unwrap();
        assert_eq!(cksum, sha256_of_file(&fresh));
        verify_crate_tarball(&fresh, "streamlib-x", "0.5.0", None).unwrap();
    }

    /// The content fingerprint ignores both the git-HEAD sha1 and the gzip
    /// compression level — it is a function of source content only.
    #[test]
    fn crate_content_fingerprint_ignores_vcs_and_compression() {
        let dir = tempdir().unwrap();
        // Same source, vcs "aaaa", default compression.
        let a = candidate_path(dir.path(), "streamlib-x", "0.5.0");
        write_crate_with_vcs(&a, "streamlib-x", "0.5.0", b"// identical source\n", "aaaaaaaa");
        // Same source, vcs "bbbb", a DIFFERENT compression level → different bytes.
        let raw_b = build_tar_bytes(&[
            (
                "streamlib-x-0.5.0/.cargo_vcs_info.json",
                b"{\n  \"git\": {\n    \"sha1\": \"bbbbbbbb\"\n  },\n  \"path_in_vcs\": \"\"\n}",
            ),
            (
                "streamlib-x-0.5.0/Cargo.toml",
                manifest_bytes("streamlib-x", "0.5.0").as_bytes(),
            ),
            ("streamlib-x-0.5.0/src/lib.rs", b"// identical source\n"),
        ]);
        let b = dir.path().join("streamlib-x-0.5.0-b.crate");
        gzip_to_file_at_level(&b, &raw_b, Compression::best());

        assert_ne!(
            std::fs::read(&a).unwrap(),
            std::fs::read(&b).unwrap(),
            "sanity: differing vcs + compression level make the raw crates differ"
        );
        assert_eq!(
            crate_content_fingerprint(&a, "streamlib-x", "0.5.0").unwrap(),
            crate_content_fingerprint(&b, "streamlib-x", "0.5.0").unwrap(),
            "the fingerprint must be independent of git HEAD and gzip level"
        );
    }
}
