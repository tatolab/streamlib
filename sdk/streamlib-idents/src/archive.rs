// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::path::{Path, PathBuf};

/// Archive container format detected from leading magic bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveKind {
    /// ZIP container (`.slpkg` / `.zip`).
    Zip,
    /// gzip-compressed tar (`.tar.gz` / `.tgz`).
    TarGz,
}

impl ArchiveKind {
    /// Human-readable label for error text.
    pub fn label(&self) -> &'static str {
        match self {
            ArchiveKind::Zip => "zip",
            ArchiveKind::TarGz => "tar.gz",
        }
    }
}

/// Per-failure-mode error from archive extraction.
#[derive(Debug, thiserror::Error)]
pub enum ArchiveError {
    /// The archive container failed to open or an entry failed to enumerate.
    #[error("malformed {kind} archive '{source_label}': {detail}")]
    Malformed {
        kind: &'static str,
        source_label: String,
        detail: String,
    },

    /// An entry's path escapes the extraction directory (`..` / absolute).
    #[error("archive entry '{entry}' in '{source_label}' escapes the extraction directory")]
    PathTraversal { source_label: String, entry: String },

    /// A symlink (or hard link) entry targets an absolute path or a path that
    /// escapes the package root — breaking the self-contained-materialization
    /// contract.
    #[error(
        "symlink entry '{entry}' in '{source_label}' targets '{target}', which is absolute \
         or escapes the package root"
    )]
    SymlinkEscape {
        source_label: String,
        entry: String,
        target: String,
    },

    /// Writing an extracted entry to disk failed.
    #[error("extracting '{entry}' from '{source_label}' failed: {detail}")]
    EntryWriteFailed {
        source_label: String,
        entry: String,
        detail: String,
    },

    /// Clearing or creating the destination directory failed.
    #[error("preparing extraction directory {}: {detail}", dest_dir.display())]
    DestinationPreparationFailed { dest_dir: PathBuf, detail: String },
}

/// Detect the archive container format from leading magic bytes. Magic is
/// authoritative; file extensions are at most a hint for error text.
pub fn sniff_archive_kind(bytes: &[u8]) -> Option<ArchiveKind> {
    if bytes.starts_with(b"PK\x03\x04") {
        return Some(ArchiveKind::Zip);
    }
    if bytes.starts_with(&[0x1f, 0x8b]) {
        return Some(ArchiveKind::TarGz);
    }
    None
}

/// Clear `dest_dir` if present and recreate it empty.
fn prepare_destination_dir(dest_dir: &Path) -> Result<(), ArchiveError> {
    let preparation_err = |detail: String| ArchiveError::DestinationPreparationFailed {
        dest_dir: dest_dir.to_path_buf(),
        detail,
    };
    if dest_dir.exists() {
        std::fs::remove_dir_all(dest_dir)
            .map_err(|e| preparation_err(format!("clearing existing directory: {e}")))?;
    }
    std::fs::create_dir_all(dest_dir)
        .map_err(|e| preparation_err(format!("creating directory: {e}")))?;
    Ok(())
}

/// Extract every entry of the in-memory ZIP `bytes` into `dest_dir` (cleared
/// first, always-overwrite), rejecting path-traversal entries. `source_label`
/// names the archive in `tracing` / error text only.
#[tracing::instrument(skip(bytes), fields(dest = %dest_dir.display()))]
pub fn extract_zip_bytes_to_dir(
    bytes: &[u8],
    dest_dir: &Path,
    source_label: &str,
) -> Result<(), ArchiveError> {
    let malformed = |detail: String| ArchiveError::Malformed {
        kind: "zip",
        source_label: source_label.to_string(),
        detail,
    };

    let cursor = std::io::Cursor::new(bytes);
    let mut archive =
        zip::ZipArchive::new(cursor).map_err(|e| malformed(format!("opening archive: {e}")))?;

    // Validate every entry path BEFORE any bytes land, so a traversal entry
    // anywhere in the archive leaves no partial extraction behind.
    //
    // This decode path materializes EVERY entry — including symlink-flagged
    // ones — as a regular file (`File::create` + `io::copy` of the entry
    // content, never `symlink()`), so a zip cannot mint a real symlink that
    // escapes the slot. The symlink-target check below is contract
    // defense-in-depth: it refuses an absolute / escaping link target so the
    // materialized package never carries a host-path reference, matching the
    // tar path's guarantee.
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| malformed(format!("reading archive entry {i}: {e}")))?;
        let entry_name = entry.name().to_string();
        if is_path_traversal(&entry_name) {
            return Err(ArchiveError::PathTraversal {
                source_label: source_label.to_string(),
                entry: entry_name,
            });
        }
        if entry.is_symlink() {
            let mut target = String::new();
            std::io::Read::read_to_string(&mut entry, &mut target).map_err(|e| {
                malformed(format!("reading symlink target for {entry_name}: {e}"))
            })?;
            let target = target.trim();
            if symlink_target_escapes_root(&entry_name, target) {
                return Err(ArchiveError::SymlinkEscape {
                    source_label: source_label.to_string(),
                    entry: entry_name,
                    target: target.to_string(),
                });
            }
        }
    }

    prepare_destination_dir(dest_dir)?;
    tracing::info!("Extracting {source_label} to {}", dest_dir.display());

    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| malformed(format!("reading archive entry {i}: {e}")))?;
        let entry_name = entry.name().to_string();
        let output_path = dest_dir.join(&entry_name);

        let entry_write_err = |detail: String| ArchiveError::EntryWriteFailed {
            source_label: source_label.to_string(),
            entry: entry_name.clone(),
            detail,
        };

        if entry.is_dir() {
            std::fs::create_dir_all(&output_path)
                .map_err(|e| entry_write_err(format!("creating directory: {e}")))?;
            continue;
        }
        if let Some(parent) = output_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| entry_write_err(format!("creating parent directory: {e}")))?;
        }
        let mut output_file = std::fs::File::create(&output_path)
            .map_err(|e| entry_write_err(format!("creating file: {e}")))?;
        std::io::copy(&mut entry, &mut output_file)
            .map_err(|e| entry_write_err(format!("writing file contents: {e}")))?;
    }

    Ok(())
}

/// Extract every entry of the in-memory gzip-compressed tar `bytes` into
/// `dest_dir` (cleared first, always-overwrite), rejecting path-traversal
/// entries. `source_label` names the archive in `tracing` / error text only.
#[tracing::instrument(skip(bytes), fields(dest = %dest_dir.display()))]
pub fn extract_tar_gz_bytes_to_dir(
    bytes: &[u8],
    dest_dir: &Path,
    source_label: &str,
) -> Result<(), ArchiveError> {
    let malformed = |detail: String| ArchiveError::Malformed {
        kind: "tar.gz",
        source_label: source_label.to_string(),
        detail,
    };

    // Validate every entry path BEFORE any bytes land (first pass over the
    // in-memory bytes is cheap), so a traversal entry anywhere in the
    // archive leaves no partial extraction behind.
    {
        let decoder = flate2::read::GzDecoder::new(std::io::Cursor::new(bytes));
        let mut tar_archive = tar::Archive::new(decoder);
        let entries = tar_archive
            .entries()
            .map_err(|e| malformed(format!("opening archive: {e}")))?;
        for entry in entries {
            let entry = entry.map_err(|e| malformed(format!("reading archive entry: {e}")))?;
            let entry_path = entry
                .path()
                .map_err(|e| malformed(format!("reading entry path: {e}")))?;
            let entry_name = entry_path.to_string_lossy().into_owned();
            if is_path_traversal(&entry_name) {
                return Err(ArchiveError::PathTraversal {
                    source_label: source_label.to_string(),
                    entry: entry_name,
                });
            }
            // Reject symlink / hard-link entries whose target is absolute or
            // escapes the package root. `unpack_in` creates real symlinks, so
            // an unguarded absolute-target link would point at an arbitrary
            // host path (e.g. /etc/passwd) inside the materialized package.
            // Internal relative links are permitted (they stay self-contained).
            let entry_type = entry.header().entry_type();
            if entry_type.is_symlink() || entry_type.is_hard_link() {
                let link_target = entry.link_name().map_err(|e| {
                    malformed(format!("reading symlink target for {entry_name}: {e}"))
                })?;
                if let Some(target) = link_target {
                    let target_str = target.to_string_lossy().into_owned();
                    if symlink_target_escapes_root(&entry_name, &target_str) {
                        return Err(ArchiveError::SymlinkEscape {
                            source_label: source_label.to_string(),
                            entry: entry_name,
                            target: target_str,
                        });
                    }
                }
            }
        }
    }

    prepare_destination_dir(dest_dir)?;
    tracing::info!("Extracting {source_label} to {}", dest_dir.display());

    let decoder = flate2::read::GzDecoder::new(std::io::Cursor::new(bytes));
    let mut tar_archive = tar::Archive::new(decoder);
    let entries = tar_archive
        .entries()
        .map_err(|e| malformed(format!("opening archive: {e}")))?;
    for entry in entries {
        let mut entry = entry.map_err(|e| malformed(format!("reading archive entry: {e}")))?;
        let entry_name = entry
            .path()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| "<unreadable path>".to_string());
        // `unpack_in` re-guards traversal (including symlink escapes) as
        // defense in depth on top of the pre-pass above.
        let unpacked = entry
            .unpack_in(dest_dir)
            .map_err(|e| ArchiveError::EntryWriteFailed {
                source_label: source_label.to_string(),
                entry: entry_name.clone(),
                detail: e.to_string(),
            })?;
        if !unpacked {
            return Err(ArchiveError::PathTraversal {
                source_label: source_label.to_string(),
                entry: entry_name,
            });
        }
    }

    Ok(())
}

/// Whether an archive entry path escapes the extraction directory.
fn is_path_traversal(entry_name: &str) -> bool {
    entry_name.starts_with('/')
        || Path::new(entry_name)
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
}

/// Whether a symlink entry's `target` escapes the extraction root when
/// resolved relative to the entry's own directory. Absolute targets, and
/// relative targets whose `..` components climb above the root, both escape.
fn symlink_target_escapes_root(entry_name: &str, target: &str) -> bool {
    use std::path::Component;
    let target_path = Path::new(target);
    if target_path.is_absolute() {
        return true;
    }
    // Depth starts at the entry's parent directory (relative to root); the
    // link target's components then walk from there. A negative depth means
    // the target climbed above the root.
    let mut depth: i64 = 0;
    if let Some(parent) = Path::new(entry_name).parent() {
        for comp in parent.components() {
            match comp {
                Component::Normal(_) => depth += 1,
                Component::ParentDir => depth -= 1,
                _ => {}
            }
        }
    }
    for comp in target_path.components() {
        match comp {
            Component::Normal(_) => depth += 1,
            Component::CurDir => {}
            Component::ParentDir => {
                depth -= 1;
                if depth < 0 {
                    return true;
                }
            }
            Component::RootDir | Component::Prefix(_) => return true,
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn zip_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut cursor = std::io::Cursor::new(Vec::new());
        {
            let mut writer = zip::ZipWriter::new(&mut cursor);
            let opts = zip::write::FileOptions::<()>::default()
                .compression_method(zip::CompressionMethod::Stored);
            for (name, body) in entries {
                if name.ends_with('/') {
                    writer.add_directory(name.trim_end_matches('/'), opts).unwrap();
                } else {
                    writer.start_file(*name, opts).unwrap();
                    writer.write_all(body).unwrap();
                }
            }
            writer.finish().unwrap();
        }
        cursor.into_inner()
    }

    fn tar_gz_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let encoder =
            flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        let mut builder = tar::Builder::new(encoder);
        for (name, body) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(body.len() as u64);
            header.set_mode(0o644);
            // Forge the entry name directly — `append_data` / `set_path`
            // refuse `..` paths, but a hostile archive carries them anyway,
            // which is exactly what the traversal tests need to simulate.
            {
                let name_bytes = name.as_bytes();
                header.as_old_mut().name[..name_bytes.len()].copy_from_slice(name_bytes);
            }
            header.set_cksum();
            builder.append(&header, *body).unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap()
    }

    #[test]
    fn sniff_detects_zip_targz_and_unknown() {
        assert_eq!(
            sniff_archive_kind(&zip_bytes(&[("a.txt", b"x")])),
            Some(ArchiveKind::Zip)
        );
        assert_eq!(
            sniff_archive_kind(&tar_gz_bytes(&[("a.txt", b"x")])),
            Some(ArchiveKind::TarGz)
        );
        assert_eq!(sniff_archive_kind(b"not an archive"), None);
        assert_eq!(sniff_archive_kind(b""), None);
    }

    #[test]
    fn sniff_is_magic_authoritative_not_extension() {
        // The caller passes bytes, not a path — a mislabeled `.slpkg` that is
        // actually a tar.gz still sniffs as TarGz.
        let bytes = tar_gz_bytes(&[("streamlib.yaml", b"x")]);
        assert_eq!(sniff_archive_kind(&bytes), Some(ArchiveKind::TarGz));
    }

    #[test]
    fn zip_extracts_files_dirs_and_nested_paths() {
        let bytes = zip_bytes(&[
            ("streamlib.yaml", b"manifest".as_slice()),
            ("schemas/", b"".as_slice()),
            ("schemas/frame.yaml", b"schema".as_slice()),
        ]);
        let dest = tempfile::tempdir().unwrap();
        extract_zip_bytes_to_dir(&bytes, dest.path(), "test.zip").unwrap();
        assert_eq!(
            std::fs::read(dest.path().join("streamlib.yaml")).unwrap(),
            b"manifest"
        );
        assert_eq!(
            std::fs::read(dest.path().join("schemas/frame.yaml")).unwrap(),
            b"schema"
        );
    }

    #[test]
    fn zip_clears_preexisting_destination() {
        let dest = tempfile::tempdir().unwrap();
        std::fs::write(dest.path().join("stale.txt"), b"old").unwrap();
        let bytes = zip_bytes(&[("fresh.txt", b"new".as_slice())]);
        extract_zip_bytes_to_dir(&bytes, dest.path(), "test.zip").unwrap();
        assert!(!dest.path().join("stale.txt").exists());
        assert!(dest.path().join("fresh.txt").exists());
    }

    #[test]
    fn zip_rejects_path_traversal_before_any_bytes_land() {
        let bytes = zip_bytes(&[
            ("ok.txt", b"fine".as_slice()),
            ("../escape.txt", b"evil".as_slice()),
        ]);
        let parent = tempfile::tempdir().unwrap();
        let dest = parent.path().join("extraction");
        let err = extract_zip_bytes_to_dir(&bytes, &dest, "evil.zip")
            .expect_err("traversal entry must be rejected");
        assert!(matches!(err, ArchiveError::PathTraversal { .. }), "{err:?}");
        // The pre-pass rejects before extraction starts — no partial state,
        // not even the benign first entry.
        assert!(!dest.exists(), "no partial extraction may survive");
        assert!(!parent.path().join("escape.txt").exists());
    }

    #[test]
    fn zip_rejects_absolute_entry_path() {
        let bytes = zip_bytes(&[("/etc/evil.txt", b"evil".as_slice())]);
        let dest = tempfile::tempdir().unwrap();
        let err = extract_zip_bytes_to_dir(&bytes, dest.path(), "abs.zip")
            .expect_err("absolute entry must be rejected");
        assert!(matches!(err, ArchiveError::PathTraversal { .. }), "{err:?}");
    }

    #[test]
    fn tar_gz_extracts_nested_paths() {
        let bytes = tar_gz_bytes(&[
            ("streamlib.yaml", b"manifest".as_slice()),
            ("schemas/frame.yaml", b"schema".as_slice()),
        ]);
        let dest = tempfile::tempdir().unwrap();
        extract_tar_gz_bytes_to_dir(&bytes, dest.path(), "test.tar.gz").unwrap();
        assert_eq!(
            std::fs::read(dest.path().join("streamlib.yaml")).unwrap(),
            b"manifest"
        );
        assert_eq!(
            std::fs::read(dest.path().join("schemas/frame.yaml")).unwrap(),
            b"schema"
        );
    }

    #[test]
    fn tar_gz_rejects_path_traversal_before_any_bytes_land() {
        let bytes = tar_gz_bytes(&[
            ("ok.txt", b"fine".as_slice()),
            ("../escape.txt", b"evil".as_slice()),
        ]);
        let parent = tempfile::tempdir().unwrap();
        let dest = parent.path().join("extraction");
        let err = extract_tar_gz_bytes_to_dir(&bytes, &dest, "evil.tar.gz")
            .expect_err("traversal entry must be rejected");
        assert!(matches!(err, ArchiveError::PathTraversal { .. }), "{err:?}");
        assert!(!dest.exists(), "no partial extraction may survive");
        assert!(!parent.path().join("escape.txt").exists());
    }

    #[test]
    fn malformed_bytes_error_loud_per_kind() {
        let dest = tempfile::tempdir().unwrap();
        let err = extract_zip_bytes_to_dir(b"not a zip", dest.path(), "junk.zip")
            .expect_err("junk must not open as zip");
        assert!(matches!(err, ArchiveError::Malformed { kind: "zip", .. }), "{err:?}");
        // A gzip header followed by junk fails as a malformed tar.gz.
        let mut junk_gz = vec![0x1f, 0x8b];
        junk_gz.extend_from_slice(b"junk body");
        let err = extract_tar_gz_bytes_to_dir(&junk_gz, dest.path(), "junk.tar.gz")
            .expect_err("junk must not open as tar.gz");
        assert!(
            matches!(err, ArchiveError::Malformed { kind: "tar.gz", .. }),
            "{err:?}"
        );
    }

    /// A `.tar.gz` with one regular file plus a symlink entry `link_path` →
    /// `target`.
    fn tar_gz_with_symlink(link_path: &str, target: &str) -> Vec<u8> {
        let encoder =
            flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        let mut builder = tar::Builder::new(encoder);
        let body = b"manifest";
        let mut fh = tar::Header::new_gnu();
        fh.set_size(body.len() as u64);
        fh.set_mode(0o644);
        fh.set_cksum();
        builder
            .append_data(&mut fh, "streamlib.yaml", body.as_slice())
            .unwrap();
        let mut lh = tar::Header::new_gnu();
        lh.set_entry_type(tar::EntryType::Symlink);
        lh.set_size(0);
        lh.set_mode(0o777);
        // `append_link` sets the path, link name, and checksum.
        builder.append_link(&mut lh, link_path, target).unwrap();
        builder.into_inner().unwrap().finish().unwrap()
    }

    #[test]
    fn tar_gz_rejects_absolute_target_symlink_with_no_residue() {
        let bytes = tar_gz_with_symlink("passwd", "/etc/passwd");
        let parent = tempfile::tempdir().unwrap();
        let dest = parent.path().join("extraction");
        let err = extract_tar_gz_bytes_to_dir(&bytes, &dest, "evil.tar.gz")
            .expect_err("absolute-target symlink must be rejected");
        assert!(matches!(err, ArchiveError::SymlinkEscape { .. }), "{err:?}");
        assert!(!dest.exists(), "no partial extraction may survive");
    }

    #[test]
    fn tar_gz_rejects_escaping_relative_symlink() {
        let bytes = tar_gz_with_symlink("link", "../../etc/passwd");
        let parent = tempfile::tempdir().unwrap();
        let dest = parent.path().join("extraction");
        let err = extract_tar_gz_bytes_to_dir(&bytes, &dest, "evil.tar.gz")
            .expect_err("escaping relative symlink must be rejected");
        assert!(matches!(err, ArchiveError::SymlinkEscape { .. }), "{err:?}");
        assert!(!dest.exists());
    }

    #[test]
    fn tar_gz_allows_internal_relative_symlink() {
        // A link that stays inside the package root materializes as a real,
        // self-contained symlink.
        let bytes = tar_gz_with_symlink("alias.yaml", "streamlib.yaml");
        let dest = tempfile::tempdir().unwrap();
        extract_tar_gz_bytes_to_dir(&bytes, dest.path(), "ok.tar.gz")
            .expect("internal relative symlink must be allowed");
        let link = dest.path().join("alias.yaml");
        assert!(
            link.symlink_metadata().unwrap().file_type().is_symlink(),
            "internal link must survive as a symlink"
        );
        // And it resolves to the sibling manifest content.
        assert_eq!(std::fs::read(&link).unwrap(), b"manifest");
    }

    #[test]
    fn zip_rejects_absolute_target_symlink_with_no_residue() {
        let mut cursor = std::io::Cursor::new(Vec::new());
        {
            let mut writer = zip::ZipWriter::new(&mut cursor);
            let opts = zip::write::FileOptions::<()>::default();
            writer.start_file("streamlib.yaml", opts).unwrap();
            writer.write_all(b"manifest").unwrap();
            writer.add_symlink("passwd", "/etc/passwd", opts).unwrap();
            writer.finish().unwrap();
        }
        let bytes = cursor.into_inner();
        let parent = tempfile::tempdir().unwrap();
        let dest = parent.path().join("extraction");
        let err = extract_zip_bytes_to_dir(&bytes, &dest, "evil.zip")
            .expect_err("absolute-target zip symlink must be rejected");
        assert!(matches!(err, ArchiveError::SymlinkEscape { .. }), "{err:?}");
        assert!(!dest.exists(), "no partial extraction may survive");
    }

    #[test]
    fn symlink_target_escape_classifier_covers_the_cases() {
        // Absolute → escape.
        assert!(symlink_target_escapes_root("link", "/etc/passwd"));
        // Climbs above root → escape.
        assert!(symlink_target_escapes_root("link", "../foo"));
        assert!(symlink_target_escapes_root("a/link", "../../foo"));
        // Stays inside → allowed.
        assert!(!symlink_target_escapes_root("link", "streamlib.yaml"));
        assert!(!symlink_target_escapes_root("a/link", "../streamlib.yaml"));
        assert!(!symlink_target_escapes_root("a/b/link", "../../c/d.yaml"));
    }
}
