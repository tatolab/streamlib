// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::project_repository_refusal::ProjectRepositoryRefusal;

/// Where a project keeps its assets' bytes, relative to the project: one file per SHA-256.
pub const PROJECT_CONTENT_STORE_DIRECTORY: &str = ".tatolab/content";

const CONTENT_COPY_CHUNK_BYTE_LEN: usize = 1 << 20;

/// Bytes the content store holds, named by their hash.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredProjectContent {
    /// The SHA-256 of the bytes, lowercase hex.
    pub content_sha256: String,
    /// Their length.
    pub content_byte_len: u64,
}

/// A project's content store: asset bytes kept apart from history and from every transaction.
#[derive(Clone, Debug)]
pub struct ProjectContentStore {
    store_directory: PathBuf,
}

impl ProjectContentStore {
    /// The store of the project at `project_directory`, made when missing.
    pub fn of_project(project_directory: &Path) -> Result<Self, ProjectRepositoryRefusal> {
        let store_directory = project_directory.join(PROJECT_CONTENT_STORE_DIRECTORY);
        std::fs::create_dir_all(&store_directory)?;
        Ok(Self { store_directory })
    }

    /// Copy the file at `source` into the store, hashing it on the way; a hash already held is kept as it is.
    pub fn store_the_file_at(
        &self,
        source: &Path,
    ) -> Result<StoredProjectContent, ProjectRepositoryRefusal> {
        let unreadable = |error: std::io::Error| ProjectRepositoryRefusal::ImportSourceUnreadable {
            path: source.to_path_buf(),
            why: error.to_string(),
        };
        let mut source_file = File::open(source).map_err(unreadable)?;
        let incoming = self.write_incoming_content(|partial_file, hasher| {
            copy_while_hashing(&mut source_file, partial_file, hasher, unreadable)
        })?;
        incoming.keep_in(self)
    }

    #[cfg(feature = "history")]
    /// Store the bytes `fill` writes, kept only when they hash to `expected` at its length; other
    /// bytes are refused naming both hashes and leave nothing in the store.
    pub(crate) fn store_content_expected_to_be(
        &self,
        expected: &StoredProjectContent,
        fill: impl FnOnce(&mut dyn Write) -> Result<(), ProjectRepositoryRefusal>,
    ) -> Result<StoredProjectContent, ProjectRepositoryRefusal> {
        let incoming = self.write_incoming_content(|partial_file, hasher| {
            let mut hashing_writer = HashingWriter {
                partial_file,
                hasher,
                written_byte_len: 0,
            };
            fill(&mut hashing_writer)?;
            Ok(hashing_writer.written_byte_len)
        })?;
        if incoming.content.content_sha256 != expected.content_sha256
            || incoming.content.content_byte_len != expected.content_byte_len
        {
            let _ = std::fs::remove_file(&incoming.partial_path);
            return Err(ProjectRepositoryRefusal::ContentArrivedAsOtherBytes {
                expected_sha256: expected.content_sha256.clone(),
                expected_byte_len: expected.content_byte_len,
                arrived_sha256: incoming.content.content_sha256,
                arrived_byte_len: incoming.content.content_byte_len,
            });
        }
        incoming.keep_in(self)
    }

    fn write_incoming_content(
        &self,
        write: impl FnOnce(&mut File, &mut Sha256) -> Result<u64, ProjectRepositoryRefusal>,
    ) -> Result<IncomingProjectContent, ProjectRepositoryRefusal> {
        let partial_path = self
            .store_directory
            .join(format!(".incoming-{}", Uuid::new_v4()));
        let mut partial_file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&partial_path)?;
        let mut hasher = Sha256::new();
        let written = write(&mut partial_file, &mut hasher)
            .and_then(|content_byte_len| Ok((content_byte_len, partial_file.sync_all()?)));
        match written {
            Ok((content_byte_len, ())) => Ok(IncomingProjectContent {
                partial_path,
                content: StoredProjectContent {
                    content_sha256: lowercase_hex(&hasher.finalize()),
                    content_byte_len,
                },
            }),
            Err(refusal) => {
                let _ = std::fs::remove_file(&partial_path);
                Err(refusal)
            }
        }
    }

    /// The bytes held under `content_sha256`, or a refusal naming the hash when they are not held.
    pub fn stored_content(
        &self,
        content_sha256: &str,
    ) -> Result<StoredProjectContent, ProjectRepositoryRefusal> {
        let not_held = || ProjectRepositoryRefusal::ContentNotInTheStore(content_sha256.to_owned());
        if !is_a_sha256_digest(content_sha256) {
            return Err(not_held());
        }
        let metadata =
            std::fs::metadata(self.content_path(content_sha256)).map_err(|_| not_held())?;
        Ok(StoredProjectContent {
            content_sha256: content_sha256.to_owned(),
            content_byte_len: metadata.len(),
        })
    }

    /// Where the bytes named `content_sha256` sit.
    pub fn content_path(&self, content_sha256: &str) -> PathBuf {
        self.store_directory.join(content_sha256)
    }

    /// Copy the bytes named `content_sha256` to a new file at `destination`, its own copy so an
    /// edit to it never reaches the store; a file already there is refused and left alone.
    pub fn place_content_at(
        &self,
        content_sha256: &str,
        destination: &Path,
    ) -> Result<(), ProjectRepositoryRefusal> {
        self.stored_content(content_sha256)?;
        let mut stored_file = File::open(self.content_path(content_sha256))?;
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut placed_file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(destination)?;
        let copied =
            std::io::copy(&mut stored_file, &mut placed_file).and_then(|_| placed_file.sync_all());
        if let Err(not_copied) = copied {
            drop(placed_file);
            let _ = std::fs::remove_file(destination);
            return Err(not_copied.into());
        }
        Ok(())
    }
}

struct IncomingProjectContent {
    partial_path: PathBuf,
    content: StoredProjectContent,
}

impl IncomingProjectContent {
    fn keep_in(
        self,
        content_store: &ProjectContentStore,
    ) -> Result<StoredProjectContent, ProjectRepositoryRefusal> {
        let stored_path = content_store.content_path(&self.content.content_sha256);
        let kept = if stored_path.exists() {
            std::fs::remove_file(&self.partial_path)
        } else {
            std::fs::rename(&self.partial_path, &stored_path)
        };
        if let Err(not_kept) = kept {
            let _ = std::fs::remove_file(&self.partial_path);
            return Err(not_kept.into());
        }
        Ok(self.content)
    }
}

#[cfg(feature = "history")]
struct HashingWriter<'partial> {
    partial_file: &'partial mut File,
    hasher: &'partial mut Sha256,
    written_byte_len: u64,
}

#[cfg(feature = "history")]
impl Write for HashingWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let written = self.partial_file.write(bytes)?;
        self.hasher.update(&bytes[..written]);
        self.written_byte_len += written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.partial_file.flush()
    }
}

fn copy_while_hashing(
    source: &mut impl Read,
    partial_file: &mut File,
    hasher: &mut Sha256,
    unreadable: impl Fn(std::io::Error) -> ProjectRepositoryRefusal,
) -> Result<u64, ProjectRepositoryRefusal> {
    let mut chunk = vec![0u8; CONTENT_COPY_CHUNK_BYTE_LEN];
    let mut content_byte_len = 0u64;
    loop {
        let read = source.read(&mut chunk).map_err(&unreadable)?;
        if read == 0 {
            return Ok(content_byte_len);
        }
        hasher.update(&chunk[..read]);
        partial_file.write_all(&chunk[..read])?;
        content_byte_len += read as u64;
    }
}

/// The SHA-256 of `bytes`, lowercase hex, as the store names content.
#[cfg(feature = "history")]
pub(crate) fn content_sha256_of(bytes: &[u8]) -> String {
    lowercase_hex(&Sha256::digest(bytes))
}

/// Whether `content_sha256` reads as a SHA-256 digest: 64 hexadecimal digits, so it names one
/// file of a content store and no path beside it.
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
    fn a_stored_file_is_named_by_its_sha256_and_storing_it_twice_keeps_one_copy() {
        let project_directory = tempfile::tempdir().unwrap();
        let source = project_directory.path().join("hello.txt");
        std::fs::write(&source, b"hello").unwrap();
        let content_store = ProjectContentStore::of_project(project_directory.path()).unwrap();
        let first = content_store.store_the_file_at(&source).unwrap();
        let second = content_store.store_the_file_at(&source).unwrap();
        assert_eq!(first, second);
        assert_eq!(
            first.content_sha256,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
        assert_eq!(first.content_byte_len, 5);
        let held: Vec<_> = std::fs::read_dir(
            project_directory
                .path()
                .join(PROJECT_CONTENT_STORE_DIRECTORY),
        )
        .unwrap()
        .collect();
        assert_eq!(held.len(), 1);
    }

    #[test]
    fn placed_content_is_its_own_copy_and_never_overwrites_a_file() {
        let project_directory = tempfile::tempdir().unwrap();
        let source = project_directory.path().join("hello.txt");
        std::fs::write(&source, b"hello").unwrap();
        let content_store = ProjectContentStore::of_project(project_directory.path()).unwrap();
        let stored = content_store.store_the_file_at(&source).unwrap();
        let placed = project_directory.path().join("media/hello.txt");
        content_store
            .place_content_at(&stored.content_sha256, &placed)
            .unwrap();
        std::fs::write(&placed, b"edited").unwrap();
        assert_eq!(
            std::fs::read(content_store.content_path(&stored.content_sha256)).unwrap(),
            b"hello"
        );
        assert!(
            content_store
                .place_content_at(&stored.content_sha256, &placed)
                .is_err()
        );
        assert_eq!(std::fs::read(&placed).unwrap(), b"edited");
        assert!(
            content_store
                .place_content_at("../../etc/passwd", &project_directory.path().join("x"))
                .is_err()
        );
    }

    #[test]
    fn a_hash_the_store_does_not_hold_is_refused_by_name() {
        let project_directory = tempfile::tempdir().unwrap();
        let content_store = ProjectContentStore::of_project(project_directory.path()).unwrap();
        for absent in ["0".repeat(64), "../../etc/passwd".to_owned()] {
            let refusal = content_store.stored_content(&absent).unwrap_err();
            assert!(refusal.to_string().contains(&absent), "{refusal}");
        }
    }

    #[test]
    fn an_unreadable_source_leaves_nothing_in_the_store() {
        let project_directory = tempfile::tempdir().unwrap();
        let content_store = ProjectContentStore::of_project(project_directory.path()).unwrap();
        assert!(matches!(
            content_store.store_the_file_at(&project_directory.path().join("absent.mp4")),
            Err(ProjectRepositoryRefusal::ImportSourceUnreadable { .. })
        ));
        let held: Vec<_> = std::fs::read_dir(
            project_directory
                .path()
                .join(PROJECT_CONTENT_STORE_DIRECTORY),
        )
        .unwrap()
        .collect();
        assert!(held.is_empty());
    }
}
