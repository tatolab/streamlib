// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::project_repository_refusal::ProjectRepositoryRefusal;

/// Where a client's upload to a project lands before its import, relative to the project: one
/// file per SHA-256, which the import moves to the asset's path.
pub const PROJECT_UPLOADS_DIRECTORY: &str = ".tatolab/uploads";

const ASSET_FILE_COPY_CHUNK_BYTE_LEN: usize = 1 << 20;

/// The SHA-256 and length of an asset's bytes, as its sidecar records them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AssetFileBytesRecorded {
    pub(crate) content_sha256: String,
    pub(crate) content_byte_len: u64,
}

/// The file an upload of the bytes named `content_sha256` lands in, in the project at
/// `project_directory`.
pub fn the_upload_file_of(project_directory: &Path, content_sha256: &str) -> PathBuf {
    project_directory
        .join(PROJECT_UPLOADS_DIRECTORY)
        .join(content_sha256)
}

/// The SHA-256 and length of the file at `path`, read once in chunks.
pub(crate) fn the_bytes_recorded_of_the_file_at(
    path: &Path,
) -> Result<AssetFileBytesRecorded, ProjectRepositoryRefusal> {
    let unreadable = |error: std::io::Error| ProjectRepositoryRefusal::ImportSourceUnreadable {
        path: path.to_path_buf(),
        why: error.to_string(),
    };
    hash_while_copying(&mut File::open(path).map_err(unreadable)?, None, unreadable)
}

/// Copy the file at `source` to a new file in the project's uploads, hashing it on the way,
/// answering where the copy is and what its sidecar records of it.
pub(crate) fn copy_the_file_into_the_uploads_hashing_it(
    project_directory: &Path,
    source: &Path,
) -> Result<(PathBuf, AssetFileBytesRecorded), ProjectRepositoryRefusal> {
    let unreadable = |error: std::io::Error| ProjectRepositoryRefusal::ImportSourceUnreadable {
        path: source.to_path_buf(),
        why: error.to_string(),
    };
    let mut source_file = File::open(source).map_err(unreadable)?;
    let uploads_directory = project_directory.join(PROJECT_UPLOADS_DIRECTORY);
    std::fs::create_dir_all(&uploads_directory)?;
    let copy_path = uploads_directory.join(format!(".incoming-{}", Uuid::new_v4()));
    let mut copy_file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&copy_path)?;
    match hash_while_copying(&mut source_file, Some(&mut copy_file), unreadable)
        .and_then(|recorded| Ok(copy_file.sync_all().map(|()| recorded)?))
    {
        Ok(recorded) => Ok((copy_path, recorded)),
        Err(refusal) => {
            drop(copy_file);
            let _ = std::fs::remove_file(&copy_path);
            Err(refusal)
        }
    }
}

fn hash_while_copying(
    source_file: &mut File,
    mut copy_file: Option<&mut File>,
    unreadable: impl Fn(std::io::Error) -> ProjectRepositoryRefusal,
) -> Result<AssetFileBytesRecorded, ProjectRepositoryRefusal> {
    let mut hasher = Sha256::new();
    let mut content_byte_len = 0u64;
    let mut chunk = vec![0u8; ASSET_FILE_COPY_CHUNK_BYTE_LEN];
    loop {
        let read = source_file.read(&mut chunk).map_err(&unreadable)?;
        if read == 0 {
            break;
        }
        hasher.update(&chunk[..read]);
        if let Some(copy_file) = copy_file.as_mut() {
            copy_file.write_all(&chunk[..read])?;
        }
        content_byte_len += read as u64;
    }
    Ok(AssetFileBytesRecorded {
        content_sha256: lowercase_hex(&hasher.finalize()),
        content_byte_len,
    })
}

/// The SHA-256 of `bytes`, lowercase hex, as a sidecar records it.
pub fn content_sha256_of(bytes: &[u8]) -> String {
    lowercase_hex(&Sha256::digest(bytes))
}

/// Whether `content_sha256` reads as a SHA-256 digest: 64 hexadecimal digits, so it names one
/// upload and no path beside it.
pub(crate) fn is_a_sha256_digest(content_sha256: &str) -> bool {
    content_sha256.len() == 64 && content_sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn lowercase_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_copy_into_the_uploads_holds_the_bytes_and_records_their_sha256_and_length() {
        let project_directory = tempfile::tempdir().unwrap();
        let source = project_directory.path().join("hello.txt");
        std::fs::write(&source, b"hello").unwrap();

        let (copy_path, recorded) =
            copy_the_file_into_the_uploads_hashing_it(project_directory.path(), &source).unwrap();

        assert_eq!(std::fs::read(&copy_path).unwrap(), b"hello");
        assert!(copy_path.starts_with(project_directory.path().join(PROJECT_UPLOADS_DIRECTORY)));
        assert_eq!(
            recorded,
            AssetFileBytesRecorded {
                content_sha256: content_sha256_of(b"hello"),
                content_byte_len: 5,
            }
        );
        assert_eq!(
            the_bytes_recorded_of_the_file_at(&source).unwrap(),
            recorded
        );
    }

    #[test]
    fn a_missing_source_is_refused_naming_it_and_leaves_no_copy() {
        let project_directory = tempfile::tempdir().unwrap();
        let missing = project_directory.path().join("missing.mp4");

        let refusal = copy_the_file_into_the_uploads_hashing_it(project_directory.path(), &missing)
            .unwrap_err();

        assert!(
            matches!(&refusal, ProjectRepositoryRefusal::ImportSourceUnreadable { path, .. } if *path == missing),
            "{refusal}"
        );
        assert!(
            !project_directory
                .path()
                .join(PROJECT_UPLOADS_DIRECTORY)
                .exists()
        );
    }

    #[test]
    fn only_sixty_four_hex_digits_name_an_upload() {
        assert!(is_a_sha256_digest(&content_sha256_of(b"x")));
        assert!(!is_a_sha256_digest("../etc/passwd"));
        assert!(!is_a_sha256_digest(&"g".repeat(64)));
    }
}
