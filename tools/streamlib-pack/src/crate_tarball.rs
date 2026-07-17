// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Integrity-verified emission + byte-stable normalization of `cargo package`
//! crate tarballs.
//!
//! - [`verify_crate_tarball`] structurally validates a `.crate`;
//!   [`obtain_crate_tarball`] always repackages via `cargo package` — the
//!   source of truth for crate bytes — and verifies the fresh artifact, so a
//!   content-stale `target/package` leftover is never emitted.
//! - [`normalize_crate_tarball`] rewrites a `.crate` into a form whose bytes are
//!   a pure function of source content, and [`finalize_crate_tarball`] pairs
//!   that with the normalized tarball's sha256 — the `package` checksum a
//!   consumer's `Cargo.lock` records for the directory-source entry. `cargo
//!   package` is already byte-deterministic *except* for the
//!   `{name}-{version}/.cargo_vcs_info.json` entry, whose embedded git-HEAD
//!   sha1 makes the checksum a function of the commit rather than the source;
//!   normalization strips it so the recorded checksum tracks source content,
//!   not the commit the emit ran at.

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
}

/// Structurally verify a `cargo package` `.crate` at `path` for
/// `(name, version)`: the gzip stream fully decodes (truncation trips the
/// trailer check), every tar entry enumerates to EOF, and the crate's
/// `{name}-{version}/Cargo.toml` entry is present. This is a structural check
/// only — the byte-stable `package` checksum a consumer's lock records is
/// produced downstream by [`finalize_crate_tarball`].
pub fn verify_crate_tarball(
    path: &Path,
    name: &str,
    version: &str,
) -> Result<(), CrateTarballIntegrityError> {
    let bytes = std::fs::read(path).map_err(|source| CrateTarballIntegrityError::Unreadable {
        path: path.display().to_string(),
        source,
    })?;

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

/// Produce a fresh, structurally-verified `.crate` at `candidate` for
/// `(name, version)` by always invoking `repackage` (`cargo package`).
///
/// `cargo package` is the single source of truth for crate bytes:
/// `target/package/<crate>-<version>.crate` is cargo scratch, **not** a
/// streamlib-managed content cache. Any pre-existing artifact is dropped up
/// front and `repackage` always runs, so the emitted `.crate` always reflects
/// current source at that version — a structurally-valid but content-stale
/// leftover (e.g. an old-ABI tarball cached under a version whose source has
/// since moved) can never be handed back verbatim. Dropping the leftover first
/// also means a `repackage` that returns `Ok` without writing surfaces as a
/// hard verification error rather than silently verifying stale bytes.
///
/// A freshly-packaged artifact that fails verification is a hard error (a
/// `cargo package` / toolchain bug), surfaced loudly rather than emitted into
/// the tree.
pub fn obtain_crate_tarball(
    candidate: &Path,
    name: &str,
    version: &str,
    repackage: impl FnOnce() -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    // `target/package` is cargo scratch, never a trusted cache: drop any
    // pre-existing artifact so `repackage` owns the bytes we verify and a
    // no-write repackage can't be masked by a stale leftover.
    match std::fs::remove_file(candidate) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| {
                format!("remove stale crate scratch tarball {}", candidate.display())
            });
        }
    }

    repackage().with_context(|| format!("repackage crate {name} {version}"))?;

    verify_crate_tarball(candidate, name, version).with_context(|| {
        format!(
            "freshly-packaged crate tarball at {} failed integrity verification",
            candidate.display()
        )
    })?;

    Ok(())
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
        let entry_path = entry
            .path()
            .context("read crate tar entry path")?
            .into_owned();
        if entry_path.to_string_lossy() == vcs_entry {
            continue;
        }
        // Clone cargo's header verbatim (inherits its canonical mtime / mode /
        // uid / gid / entry type / size), then re-emit via `append_data` so the
        // long-name (GNU extension) handling is applied for the resolved path.
        let mut header = entry.header().clone();
        let mut data = Vec::new();
        entry
            .read_to_end(&mut data)
            .context("read crate tar entry data")?;
        builder
            .append_data(&mut header, &entry_path, &data[..])
            .context("re-append crate tar entry")?;
    }
    builder.into_inner().context("finalize canonical crate tar")
}

/// Rewrite the `.crate` at `path` into its byte-stable canonical form for
/// `(name, version)`: strip `.cargo_vcs_info.json` and re-gzip with a fixed
/// header (MTIME 0, no embedded filename). Idempotent — re-normalizing an
/// already-normalized crate reproduces identical bytes, so re-emitting a crate
/// at an unchanged version yields byte-identical output.
pub fn normalize_crate_tarball(path: &Path, name: &str, version: &str) -> anyhow::Result<()> {
    let canonical = canonical_tar_bytes(path, name, version)?;
    let mut encoder = GzBuilder::new()
        .mtime(0)
        .write(Vec::new(), Compression::default());
    encoder
        .write_all(&canonical)
        .context("gzip canonical crate tar")?;
    let gzipped = encoder
        .finish()
        .context("finish gzip canonical crate tar")?;
    std::fs::write(path, &gzipped)
        .with_context(|| format!("write normalized crate tarball {}", path.display()))?;
    Ok(())
}

/// Normalize the `.crate` at `path` into byte-stable form and return the
/// normalized tarball's sha256 — the `package` checksum a consumer's
/// `Cargo.lock` records for the directory-source entry.
///
/// Normalization strips the git-HEAD vcs-info and fixes the gzip header, so the
/// checksum is a pure function of source content: a benign commit bump
/// (identical source, new git HEAD) yields the same checksum, so it neither
/// churns the index nor depends on the commit the emit ran at. There is no
/// prior-`.crate` immutability diff in the mirror path — the served tree stores
/// unpacked directory-source entries, not `.crate` files, so version-bump
/// discipline plus the atomic whole-tree swap, not an in-emit guard, keep an
/// engine crate's bytes immutable across re-emits at a fixed version.
pub fn finalize_crate_tarball(path: &Path, name: &str, version: &str) -> anyhow::Result<String> {
    normalize_crate_tarball(path, name, version)?;

    let bytes = std::fs::read(path)
        .with_context(|| format!("read normalized crate tarball {}", path.display()))?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Ok(format!("{:x}", hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::Compression;
    use flate2::write::GzEncoder;
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

    /// Write a valid `.crate` (Cargo.toml + a source file) at `path` whose
    /// `src/lib.rs` carries `lib_contents` — the distinguishing marker used to
    /// prove which source a `.crate` was built from.
    fn write_valid_crate_with_lib(path: &Path, name: &str, version: &str, lib_contents: &[u8]) {
        let manifest = manifest_bytes(name, version);
        let manifest_entry = format!("{name}-{version}/Cargo.toml");
        let lib_entry = format!("{name}-{version}/src/lib.rs");
        let raw = build_tar_bytes(&[
            (manifest_entry.as_str(), manifest.as_bytes()),
            (lib_entry.as_str(), lib_contents),
        ]);
        gzip_to_file(path, &raw);
    }

    /// Write a valid `.crate` (Cargo.toml + a source file) at `path`.
    fn write_valid_crate(path: &Path, name: &str, version: &str) {
        write_valid_crate_with_lib(path, name, version, b"// lib\n");
    }

    /// Read back the `{name}-{version}/src/lib.rs` bytes from a `.crate` — the
    /// source marker, for asserting which source a `.crate` was built from.
    fn crate_lib_contents(path: &Path, name: &str, version: &str) -> Vec<u8> {
        let want = format!("{name}-{version}/src/lib.rs");
        let bytes = std::fs::read(path).unwrap();
        let mut decoded = Vec::new();
        GzDecoder::new(&bytes[..])
            .read_to_end(&mut decoded)
            .unwrap();
        let mut archive = tar::Archive::new(&decoded[..]);
        for entry in archive.entries().unwrap() {
            let mut entry = entry.unwrap();
            if entry.path().unwrap().to_string_lossy() == want {
                let mut data = Vec::new();
                entry.read_to_end(&mut data).unwrap();
                return data;
            }
        }
        panic!("crate {} missing lib entry {want}", path.display());
    }

    fn candidate_path(dir: &Path, name: &str, version: &str) -> PathBuf {
        dir.join(format!("{name}-{version}.crate"))
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
        let vcs_json = format!(
            "{{\n  \"git\": {{\n    \"sha1\": \"{vcs_sha1}\"\n  }},\n  \"path_in_vcs\": \"\"\n}}"
        );
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
        GzDecoder::new(&bytes[..])
            .read_to_end(&mut decoded)
            .unwrap();
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
        verify_crate_tarball(&path, "streamlib-x", "0.5.0").unwrap();
    }

    /// KEY (exit criterion): a structurally-VALID but content-STALE cached
    /// tarball is NOT trusted — `repackage` always runs and the final artifact
    /// is the fresh source, never the stale cache. This is the exact defect
    /// that turned `pack → load` red (an ABI-4 `.crate` cached under a version
    /// whose source moved to ABI 5, re-emitted verbatim).
    ///
    /// Mental revert: restore the `if candidate.is_file() { verify → return }`
    /// fast-path and the closure never runs — `repackaged` stays false and the
    /// final bytes equal the stale cache — so both assertions fail.
    #[test]
    fn obtain_always_repackages_ignoring_valid_cached_tarball() {
        let dir = tempdir().unwrap();
        let path = candidate_path(dir.path(), "streamlib-x", "0.5.0");
        // A pre-existing candidate that is structurally VALID (verifies clean)
        // but built from STALE source.
        write_valid_crate_with_lib(&path, "streamlib-x", "0.5.0", b"// STALE source\n");
        verify_crate_tarball(&path, "streamlib-x", "0.5.0").expect(
            "sanity: the stale cache is structurally valid, so the old fast-path WOULD reuse it",
        );
        let stale_bytes = std::fs::read(&path).unwrap();

        let repackaged = Cell::new(false);
        obtain_crate_tarball(&path, "streamlib-x", "0.5.0", || {
            repackaged.set(true);
            // Fresh `cargo package` writes DIFFERENT bytes at the same version.
            write_valid_crate_with_lib(&path, "streamlib-x", "0.5.0", b"// FRESH source\n");
            Ok(())
        })
        .unwrap();

        assert!(
            repackaged.get(),
            "a structurally-valid cached tarball must NOT be trusted — repackage always runs"
        );
        let final_bytes = std::fs::read(&path).unwrap();
        assert_ne!(
            final_bytes, stale_bytes,
            "the emitted artifact must be the fresh package, never the stale cache"
        );
        assert_eq!(
            crate_lib_contents(&path, "streamlib-x", "0.5.0"),
            b"// FRESH source\n",
            "the emitted artifact must carry current source"
        );
        verify_crate_tarball(&path, "streamlib-x", "0.5.0").unwrap();
    }

    /// A corrupt pre-existing candidate is likewise dropped and repackaged into
    /// a valid fresh artifact (no error from the leftover).
    #[test]
    fn obtain_repackages_truncated_cached_tarball() {
        let dir = tempdir().unwrap();
        let path = candidate_path(dir.path(), "streamlib-x", "0.5.0");
        write_valid_crate(&path, "streamlib-x", "0.5.0");

        // Truncate to a small prefix — mid gzip stream, unmistakably corrupt.
        let full = std::fs::metadata(&path).unwrap().len();
        assert!(
            full > 40,
            "sanity: a real crate tarball is larger than the gzip header"
        );
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(20)
            .unwrap();
        assert!(verify_crate_tarball(&path, "streamlib-x", "0.5.0").is_err());

        let repackaged = Cell::new(false);
        obtain_crate_tarball(&path, "streamlib-x", "0.5.0", || {
            repackaged.set(true);
            write_valid_crate(&path, "streamlib-x", "0.5.0");
            Ok(())
        })
        .unwrap();

        assert!(
            repackaged.get(),
            "repackage MUST run for a corrupt cached tarball"
        );
        // Final artifact is valid.
        verify_crate_tarball(&path, "streamlib-x", "0.5.0").unwrap();
    }

    #[test]
    fn zero_byte_tarball_fails_verification() {
        let dir = tempdir().unwrap();
        let path = candidate_path(dir.path(), "streamlib-x", "0.5.0");
        std::fs::write(&path, b"").unwrap();
        let err = verify_crate_tarball(&path, "streamlib-x", "0.5.0").unwrap_err();
        assert!(
            matches!(err, CrateTarballIntegrityError::GzipInvalid(_)),
            "got {err:?}"
        );
    }

    #[test]
    fn tarball_missing_manifest_entry_fails() {
        let dir = tempdir().unwrap();
        let path = candidate_path(dir.path(), "streamlib-x", "0.5.0");
        // A well-formed gzip-tar with a source file but NO Cargo.toml.
        let raw = build_tar_bytes(&[("streamlib-x-0.5.0/src/lib.rs", b"// lib\n")]);
        gzip_to_file(&path, &raw);
        let err = verify_crate_tarball(&path, "streamlib-x", "0.5.0").unwrap_err();
        match err {
            CrateTarballIntegrityError::MissingManifestEntry { expected } => {
                assert_eq!(expected, "streamlib-x-0.5.0/Cargo.toml");
            }
            other => panic!("expected MissingManifestEntry, got {other:?}"),
        }
    }

    #[test]
    fn absent_candidate_repackages_fresh() {
        let dir = tempdir().unwrap();
        let path = candidate_path(dir.path(), "streamlib-x", "0.5.0");
        assert!(!path.exists());

        let repackaged = Cell::new(false);
        obtain_crate_tarball(&path, "streamlib-x", "0.5.0", || {
            repackaged.set(true);
            write_valid_crate(&path, "streamlib-x", "0.5.0");
            Ok(())
        })
        .unwrap();

        assert!(repackaged.get());
        verify_crate_tarball(&path, "streamlib-x", "0.5.0").unwrap();
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
        assert!(
            result.is_err(),
            "a still-corrupt repackage must be a hard error"
        );
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

        let err = verify_crate_tarball(&path, "streamlib-x", "0.5.0").unwrap_err();
        assert!(
            matches!(err, CrateTarballIntegrityError::TarInvalid(_)),
            "got {err:?}"
        );
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
        let err = verify_crate_tarball(&path, "streamlib-x", "0.5.0").unwrap_err();
        assert!(
            matches!(err, CrateTarballIntegrityError::GzipInvalid(_)),
            "got {err:?}"
        );
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
        verify_crate_tarball(&path, "streamlib-x", "0.5.0").unwrap();

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

    /// `finalize_crate_tarball` normalizes the `.crate` in place and returns the
    /// normalized tarball's sha256 (the `package` checksum a consumer's lock
    /// records). The returned digest matches the on-disk normalized bytes, and a
    /// benign commit bump (identical source, different git HEAD) yields the same
    /// digest — the checksum tracks source, not the commit.
    #[test]
    fn finalize_normalizes_and_returns_stable_checksum() {
        let dir = tempdir().unwrap();
        let fresh = candidate_path(dir.path(), "streamlib-x", "0.5.0");
        write_crate_with_vcs(&fresh, "streamlib-x", "0.5.0", b"// lib\n", "aaaaaaaa");

        let cksum = finalize_crate_tarball(&fresh, "streamlib-x", "0.5.0").unwrap();
        assert_eq!(cksum, sha256_of_file(&fresh));
        verify_crate_tarball(&fresh, "streamlib-x", "0.5.0").unwrap();

        // A second crate: same source, different git HEAD → same finalized digest.
        let bumped = dir.path().join("streamlib-x-0.5.0-bumped.crate");
        write_crate_with_vcs(&bumped, "streamlib-x", "0.5.0", b"// lib\n", "bbbbbbbb");
        let bumped_cksum = finalize_crate_tarball(&bumped, "streamlib-x", "0.5.0").unwrap();
        assert_eq!(
            cksum, bumped_cksum,
            "a benign commit bump must yield the same normalized checksum (no index churn)"
        );
    }
}
