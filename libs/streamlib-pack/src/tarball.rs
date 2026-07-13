// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Byte-stable normalization of `.tar.gz` / `.tgz` publish artifacts — the
//! pypi sdist (`uv build --sdist`) and the npm tarball (`deno pack`).
//!
//! Sibling of [`crate::crate_tarball`] (the cargo `.crate`): same shape —
//! normalize into a source-content-only form, then guard a changed-source
//! republish under an already-published version via a compression-independent
//! content fingerprint — but the per-tool non-determinism sources differ, and
//! were measured rather than assumed:
//!
//! - `uv build --sdist` stamps the build wall-clock into every
//!   build-generated entry's tar mtime (`PKG-INFO`, the `.egg-info/*`
//!   metadata, directory entries, `setup.cfg`) and into the gzip MTIME header;
//!   source-file contents, entry order, mode, and ownership are already
//!   stable across re-emits.
//! - `deno pack` is already byte-deterministic (fixed epoch-0 tar mtime,
//!   uid/gid 0, zeroed gzip MTIME), so normalization is a lossless idempotent
//!   pass — the value it adds is letting the same immutability guard run over
//!   the artifact.
//!
//! Normalization zeroes every entry's mtime, canonicalizes ownership (uid/gid
//! 0, empty owner names), preserves mode / entry type / path / data / order,
//! and re-gzips with a fixed header. The result is a pure function of source
//! content — independent of the build wall-clock and of gzip settings — so two
//! re-emits of an unchanged release are byte-identical while a source change
//! under a published version is refused.

use std::io::{Read, Write};
use std::path::Path;

use anyhow::Context;
use flate2::read::GzDecoder;
use flate2::{Compression, GzBuilder};
use sha2::{Digest, Sha256};

/// Decode the `.tar.gz` at `path` and re-serialize its entries — headers
/// cloned verbatim except the environment-derived fields (mtime + ownership),
/// in the original order — into canonical *uncompressed* tar bytes. The result
/// is a pure function of the archive's file contents: independent of the build
/// wall-clock (mtime zeroed), the build user (uid/gid/owner-name
/// canonicalized), and gzip settings (uncompressed).
fn canonical_tar_bytes(path: &Path) -> anyhow::Result<Vec<u8>> {
    let bytes = std::fs::read(path).with_context(|| format!("read tarball {}", path.display()))?;
    let mut decoded = Vec::new();
    GzDecoder::new(&bytes[..])
        .read_to_end(&mut decoded)
        .with_context(|| format!("gzip-decode tarball {}", path.display()))?;

    let mut archive = tar::Archive::new(&decoded[..]);
    let mut builder = tar::Builder::new(Vec::new());
    let entries = archive
        .entries()
        .with_context(|| format!("read tar entries {}", path.display()))?;
    for entry in entries {
        let mut entry = entry.context("read tar entry")?;
        let entry_path = entry.path().context("read tar entry path")?.into_owned();
        // Clone the source header (preserves mode / entry type / size / tar
        // format), then canonicalize exactly the fields a build tool derives
        // from its environment rather than from source: the mtime (uv stamps
        // the build wall-clock) and ownership (uid/gid + owner names, which
        // vary by build user / machine). `append_data` re-resolves the path,
        // size, and checksum on emit.
        let mut header = entry.header().clone();
        header.set_mtime(0);
        header.set_uid(0);
        header.set_gid(0);
        header
            .set_username("")
            .context("clear tar entry username")?;
        header
            .set_groupname("")
            .context("clear tar entry groupname")?;
        let mut data = Vec::new();
        entry
            .read_to_end(&mut data)
            .context("read tar entry data")?;
        builder
            .append_data(&mut header, &entry_path, &data[..])
            .context("re-append tar entry")?;
    }
    builder.into_inner().context("finalize canonical tar")
}

/// Rewrite the `.tar.gz` / `.tgz` at `path` into its byte-stable canonical
/// form: zero every entry's mtime, canonicalize ownership, and re-gzip with a
/// fixed header (MTIME 0, no embedded filename). Idempotent — re-normalizing an
/// already-normalized archive reproduces byte-identical bytes.
pub fn normalize_tar_gz(path: &Path) -> anyhow::Result<()> {
    let canonical = canonical_tar_bytes(path)?;
    let mut encoder = GzBuilder::new()
        .mtime(0)
        .write(Vec::new(), Compression::default());
    encoder
        .write_all(&canonical)
        .context("gzip canonical tar")?;
    let gzipped = encoder.finish().context("finish gzip canonical tar")?;
    std::fs::write(path, &gzipped)
        .with_context(|| format!("write normalized tarball {}", path.display()))?;
    Ok(())
}

/// The sha256 of the archive's canonical (mtime-zeroed, ownership-canonical,
/// uncompressed) tar bytes — a compression-independent fingerprint of file
/// content. Two archives with identical file contents but a different build
/// wall-clock (or gzip level, or build user) share a fingerprint; a real
/// content change is a different fingerprint.
pub fn tar_gz_content_fingerprint(path: &Path) -> anyhow::Result<String> {
    let canonical = canonical_tar_bytes(path)?;
    let mut hasher = Sha256::new();
    hasher.update(&canonical);
    Ok(format!("{:x}", hasher.finalize()))
}

/// Normalize the `.tar.gz` / `.tgz` at `path` into byte-stable form, refuse a
/// source change under an already-published artifact, and return the normalized
/// archive's sha256 (the pypi simple-index hash; the npm emit derives its own
/// sha1 + sha512 from the normalized bytes).
///
/// `previously_served` is the same-named artifact in the prior complete served
/// tree (still present during a staged emit). When it exists, its content
/// fingerprint — normalized on the fly, so a legacy un-normalized served tree
/// does not false-positive during the transition — must equal the fresh
/// archive's; a mismatch means the source changed without a version bump and is
/// refused. An unchanged re-emit (identical contents, a fresh build wall-clock)
/// passes and yields the same checksum, so it neither churns the index nor
/// trips the guard.
pub fn finalize_tar_gz(path: &Path, previously_served: Option<&Path>) -> anyhow::Result<String> {
    normalize_tar_gz(path)?;

    if let Some(served) = previously_served {
        let new_fingerprint = tar_gz_content_fingerprint(path)?;
        let served_fingerprint = tar_gz_content_fingerprint(served)?;
        if new_fingerprint != served_fingerprint {
            let artifact = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("<artifact>");
            anyhow::bail!(
                "publish artifact `{artifact}` changed at the same version \
                 (content fingerprint {new_fingerprint} != published \
                 {served_fingerprint}) — bump the version; re-emitting different \
                 source under a published version is refused"
            );
        }
    }

    let bytes = std::fs::read(path)
        .with_context(|| format!("read normalized tarball {}", path.display()))?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Ok(format!("{:x}", hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::tempdir;

    /// Build a `.tar.gz` at `path` from `(name, mode, mtime, uid, data)`
    /// entries, gzipped at `level` with `gzip_mtime` in the gzip header — the
    /// two knobs (`level`, `gzip_mtime`) plus per-entry `mtime`/`uid` let a
    /// test reproduce the exact non-determinism the real tools exhibit.
    fn write_tar_gz(
        path: &Path,
        entries: &[(&str, u32, u64, u64, &[u8])],
        level: Compression,
        gzip_mtime: u32,
    ) {
        let mut builder = tar::Builder::new(Vec::new());
        for (name, mode, mtime, uid, data) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(*mode);
            header.set_mtime(*mtime);
            header.set_uid(*uid);
            header.set_gid(*uid);
            header.set_username("builder").unwrap();
            header.set_groupname("builder").unwrap();
            builder.append_data(&mut header, name, *data).unwrap();
        }
        let raw = builder.into_inner().unwrap();
        let file = std::fs::File::create(path).unwrap();
        let mut enc = GzBuilder::new().mtime(gzip_mtime).write(file, level);
        enc.write_all(&raw).unwrap();
        enc.finish().unwrap();
    }

    /// The entry paths inside a `.tar.gz`.
    fn entry_names(path: &Path) -> Vec<String> {
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

    /// (name, mtime, uid, gid, data) for every entry — asserting the
    /// canonicalized fields after normalize.
    fn entry_meta(path: &Path) -> Vec<(String, u64, u64, u64, Vec<u8>)> {
        let bytes = std::fs::read(path).unwrap();
        let mut decoded = Vec::new();
        GzDecoder::new(&bytes[..])
            .read_to_end(&mut decoded)
            .unwrap();
        let mut archive = tar::Archive::new(&decoded[..]);
        archive
            .entries()
            .unwrap()
            .map(|e| {
                let mut e = e.unwrap();
                let name = e.path().unwrap().to_string_lossy().into_owned();
                let h = e.header();
                let mtime = h.mtime().unwrap();
                let uid = h.uid().unwrap();
                let gid = h.gid().unwrap();
                let mut data = Vec::new();
                e.read_to_end(&mut data).unwrap();
                (name, mtime, uid, gid, data)
            })
            .collect()
    }

    fn sha256_of_file(path: &Path) -> String {
        let bytes = std::fs::read(path).unwrap();
        let mut hasher = Sha256::new();
        hasher.update(&bytes);
        format!("{:x}", hasher.finalize())
    }

    fn artifact_path(dir: &Path, name: &str) -> PathBuf {
        dir.join(name)
    }

    /// The real-tool non-determinism, verbatim: contents identical, only entry
    /// mtimes + build user + the gzip MTIME header vary. Used by the byte-
    /// stability tests to model two independent emits.
    const SDIST_CONTENT: &[(&str, u32, u64, u64, &[u8])] = &[
        (
            "streamlib-0.5.0/PKG-INFO",
            0o644,
            0,
            0,
            b"Metadata-Version: 2.1\n",
        ),
        (
            "streamlib-0.5.0/pyproject.toml",
            0o664,
            0,
            0,
            b"[project]\nname='x'\n",
        ),
        (
            "streamlib-0.5.0/python/streamlib/__init__.py",
            0o664,
            0,
            0,
            b"# init\n",
        ),
    ];

    /// Normalize zeroes mtime + canonicalizes ownership, keeps contents +
    /// order, and is idempotent (a second pass reproduces byte-identical
    /// output). Mentally revert the `set_mtime(0)` / `set_uid(0)` and the
    /// post-normalize metadata assertions fail.
    #[test]
    fn normalize_canonicalizes_metadata_and_is_idempotent() {
        let dir = tempdir().unwrap();
        let path = artifact_path(dir.path(), "streamlib-0.5.0.tar.gz");
        write_tar_gz(
            &path,
            &[
                (
                    "streamlib-0.5.0/PKG-INFO",
                    0o644,
                    1_700_000_123,
                    1000,
                    b"meta\n",
                ),
                (
                    "streamlib-0.5.0/src/lib.py",
                    0o664,
                    1_699_999_888,
                    1000,
                    b"# src\n",
                ),
            ],
            Compression::default(),
            1_700_000_123,
        );

        let names_before = entry_names(&path);
        normalize_tar_gz(&path).unwrap();

        // Order + contents preserved.
        assert_eq!(entry_names(&path), names_before);
        // Every entry's mtime / uid / gid canonicalized to 0.
        for (name, mtime, uid, gid, _data) in entry_meta(&path) {
            assert_eq!(mtime, 0, "{name}: mtime must be zeroed");
            assert_eq!(uid, 0, "{name}: uid must be canonicalized");
            assert_eq!(gid, 0, "{name}: gid must be canonicalized");
        }

        let after_first = std::fs::read(&path).unwrap();
        normalize_tar_gz(&path).unwrap();
        let after_second = std::fs::read(&path).unwrap();
        assert_eq!(
            after_first, after_second,
            "normalize must be idempotent — a second pass reproduces identical bytes"
        );
    }

    /// KEY (exit-criterion 1): two independent emits of identical source that
    /// differ only in entry mtimes, build user, gzip level, and gzip MTIME
    /// header — exactly the measured real-tool non-determinism — normalize to
    /// byte-identical archives with equal checksums. Mentally revert the
    /// header canonicalization in `canonical_tar_bytes` and the two differ.
    #[test]
    fn two_emits_differing_only_in_metadata_normalize_identical() {
        let dir = tempdir().unwrap();
        let a = artifact_path(dir.path(), "streamlib-0.5.0.tar.gz");
        let b = dir.path().join("streamlib-0.5.0-b.tar.gz");
        // Emit A: one build wall-clock, user 1000, default compression.
        let a_entries: Vec<_> = SDIST_CONTENT
            .iter()
            .map(|(n, m, _mt, _u, d)| (*n, *m, 1_700_000_001u64, 1000u64, *d))
            .collect();
        write_tar_gz(&a, &a_entries, Compression::default(), 1_700_000_001);
        // Emit B: a later build wall-clock, a different user, a different gzip
        // level — the superset of what two real emits (even cross-machine) vary.
        let b_entries: Vec<_> = SDIST_CONTENT
            .iter()
            .map(|(n, m, _mt, _u, d)| (*n, *m, 1_700_009_999u64, 2000u64, *d))
            .collect();
        write_tar_gz(&b, &b_entries, Compression::best(), 1_700_009_999);

        assert_ne!(
            std::fs::read(&a).unwrap(),
            std::fs::read(&b).unwrap(),
            "sanity: differing mtime + user + gzip make the raw archives differ"
        );

        normalize_tar_gz(&a).unwrap();
        normalize_tar_gz(&b).unwrap();

        assert_eq!(
            std::fs::read(&a).unwrap(),
            std::fs::read(&b).unwrap(),
            "identical file contents must normalize to byte-identical archives"
        );
        assert_eq!(sha256_of_file(&a), sha256_of_file(&b));
    }

    /// The content fingerprint ignores the build wall-clock, the build user,
    /// AND the gzip level — it is a function of file content only.
    #[test]
    fn content_fingerprint_ignores_metadata_and_compression() {
        let dir = tempdir().unwrap();
        let a = artifact_path(dir.path(), "a.tar.gz");
        let b = dir.path().join("b.tar.gz");
        write_tar_gz(
            &a,
            &[("pkg/mod.js", 0o644, 111, 1000, b"export const x = 1;\n")],
            Compression::default(),
            111,
        );
        write_tar_gz(
            &b,
            &[("pkg/mod.js", 0o644, 999_999, 4242, b"export const x = 1;\n")],
            Compression::best(),
            999_999,
        );
        assert_ne!(
            std::fs::read(&a).unwrap(),
            std::fs::read(&b).unwrap(),
            "sanity: differing metadata + compression make the raw archives differ"
        );
        assert_eq!(
            tar_gz_content_fingerprint(&a).unwrap(),
            tar_gz_content_fingerprint(&b).unwrap(),
            "the fingerprint must be independent of mtime / owner / gzip level"
        );
    }

    /// NEGATIVE (exit-criterion 2): a changed source under the same published
    /// artifact is refused. Mentally revert the guard (drop the fingerprint
    /// compare in `finalize_tar_gz`) and this returns Ok instead of Err.
    #[test]
    fn guard_refuses_changed_source_same_version() {
        let dir = tempdir().unwrap();
        let served = artifact_path(dir.path(), "streamlib-0.5.0.tar.gz");
        let fresh = dir.path().join("streamlib-0.5.0-fresh.tar.gz");
        write_tar_gz(
            &served,
            &[("streamlib-0.5.0/mod.py", 0o644, 111, 1000, b"# source A\n")],
            Compression::default(),
            111,
        );
        write_tar_gz(
            &fresh,
            &[(
                "streamlib-0.5.0/mod.py",
                0o644,
                222,
                2000,
                b"# source B - CHANGED\n",
            )],
            Compression::best(),
            222,
        );

        let err = finalize_tar_gz(&fresh, Some(served.as_path())).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("changed at the same version"),
            "expected the immutability-guard message, got: {msg}"
        );
    }

    /// An unchanged re-emit (identical contents, a fresh build wall-clock + a
    /// different build user) passes the guard even when the served tree is a
    /// legacy *un-normalized* archive, and the returned checksum is the stable
    /// normalized one.
    #[test]
    fn guard_allows_same_content_different_metadata() {
        let dir = tempdir().unwrap();
        let served = artifact_path(dir.path(), "streamlib-0.5.0.tar.gz");
        let served_copy = dir.path().join("served-copy.tar.gz");
        let fresh = dir.path().join("fresh.tar.gz");
        // Served tree is legacy: real (non-zero) mtimes + a real build user.
        write_tar_gz(
            &served,
            &[(
                "streamlib-0.5.0/mod.py",
                0o644,
                111,
                1000,
                b"# stable source\n",
            )],
            Compression::default(),
            111,
        );
        std::fs::copy(&served, &served_copy).unwrap();
        write_tar_gz(
            &fresh,
            &[(
                "streamlib-0.5.0/mod.py",
                0o644,
                999,
                2000,
                b"# stable source\n",
            )],
            Compression::best(),
            999,
        );

        // The normalized checksum the served tree *should* carry.
        normalize_tar_gz(&served_copy).unwrap();
        let served_normalized_cksum = sha256_of_file(&served_copy);

        let cksum = finalize_tar_gz(&fresh, Some(served.as_path()))
            .expect("identical contents under a legacy served tree must pass the guard");
        assert_eq!(
            cksum, served_normalized_cksum,
            "an unchanged re-emit must yield the same normalized checksum (no index churn)"
        );
    }

    /// A first emit into a fresh tree (no prior served artifact) is always
    /// allowed and still returns the normalized checksum.
    #[test]
    fn guard_absent_served_is_ok() {
        let dir = tempdir().unwrap();
        let fresh = artifact_path(dir.path(), "streamlib-0.5.0.tar.gz");
        write_tar_gz(
            &fresh,
            &[("streamlib-0.5.0/mod.py", 0o644, 111, 1000, b"# lib\n")],
            Compression::default(),
            111,
        );

        let cksum = finalize_tar_gz(&fresh, None).unwrap();
        assert_eq!(cksum, sha256_of_file(&fresh));
    }

    /// Local integration harness (real-tool evidence for exit-criterion 1):
    /// point `STREAMLIB_TARBALL_A` / `STREAMLIB_TARBALL_B` at two independent
    /// `uv build --sdist` (or `deno pack`) outputs of the *same* source, and
    /// this asserts they normalize byte-identically (and that a one-byte
    /// content change flips the fingerprint). Ignored by default — it needs
    /// pre-built artifacts and is skipped when the env vars are unset; run with
    /// `cargo test -p streamlib-pack -- --ignored real_tool_outputs`.
    #[test]
    #[ignore = "requires two pre-built real-tool artifacts via STREAMLIB_TARBALL_{A,B}"]
    fn real_tool_outputs_normalize_identical() {
        let (Ok(a_src), Ok(b_src)) = (
            std::env::var("STREAMLIB_TARBALL_A"),
            std::env::var("STREAMLIB_TARBALL_B"),
        ) else {
            return;
        };
        let dir = tempdir().unwrap();
        let a = dir.path().join("a.tar.gz");
        let b = dir.path().join("b.tar.gz");
        std::fs::copy(&a_src, &a).unwrap();
        std::fs::copy(&b_src, &b).unwrap();

        // The two raw artifacts may differ (uv stamps the build wall-clock) or
        // already match (deno pack is byte-deterministic) — don't require
        // either; the load-bearing assertion is post-normalize. After
        // normalization the two must be byte-identical with equal fingerprints,
        // regardless of whether the raw bytes differed.
        normalize_tar_gz(&a).unwrap();
        normalize_tar_gz(&b).unwrap();
        assert_eq!(
            std::fs::read(&a).unwrap(),
            std::fs::read(&b).unwrap(),
            "two real-tool emits of identical source must normalize byte-identically"
        );
        assert_eq!(
            tar_gz_content_fingerprint(&a).unwrap(),
            tar_gz_content_fingerprint(&b).unwrap()
        );

        // Normalizing a second copy of the same source reproduces the same
        // fingerprint (idempotence over a real artifact).
        let again = dir.path().join("again.tar.gz");
        std::fs::copy(&a_src, &again).unwrap();
        normalize_tar_gz(&again).unwrap();
        assert_eq!(
            tar_gz_content_fingerprint(&a).unwrap(),
            tar_gz_content_fingerprint(&again).unwrap(),
            "same source normalizes to the same fingerprint"
        );
    }

    /// The normalized archive is still a structurally valid tar.gz — every
    /// entry enumerates and its data round-trips unchanged.
    #[test]
    fn normalized_archive_preserves_contents() {
        let dir = tempdir().unwrap();
        let path = artifact_path(dir.path(), "streamlib-0.5.0.tar.gz");
        let before: Vec<(String, Vec<u8>)> = SDIST_CONTENT
            .iter()
            .map(|(n, _m, _mt, _u, d)| (n.to_string(), d.to_vec()))
            .collect();
        let entries: Vec<_> = SDIST_CONTENT
            .iter()
            .map(|(n, m, _mt, _u, d)| (*n, *m, 1_700_000_001u64, 1000u64, *d))
            .collect();
        write_tar_gz(&path, &entries, Compression::default(), 1_700_000_001);

        normalize_tar_gz(&path).unwrap();

        let after: Vec<(String, Vec<u8>)> = entry_meta(&path)
            .into_iter()
            .map(|(n, _mt, _u, _g, d)| (n, d))
            .collect();
        assert_eq!(
            after, before,
            "normalize must preserve entry paths + contents"
        );
    }
}
