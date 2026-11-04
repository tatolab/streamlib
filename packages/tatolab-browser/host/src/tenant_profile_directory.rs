// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Where a stream's CEF profile — its cookies, storage and cache — lives.

use std::path::{Path, PathBuf};

/// What every stream's profile directory is named with, keeping it apart from
/// the directories Chromium keeps in its root cache itself.
const TENANT_PROFILE_DIRECTORY_NAME_PREFIX: &str = "stream-profile-";

/// The profile directory of `tenant_stream_id`: directly in
/// `root_cache_directory`, since CEF's Chrome runtime refuses a profile
/// anywhere deeper, and named so no stream id can reach outside it.
pub fn tenant_profile_directory(root_cache_directory: &Path, tenant_stream_id: &str) -> PathBuf {
    root_cache_directory.join(format!(
        "{TENANT_PROFILE_DIRECTORY_NAME_PREFIX}{}",
        file_name_safe_spelling(tenant_stream_id)
    ))
}

/// `text` with every byte but ASCII letters, digits, `-` and `_` written as
/// `.` and two hex digits, so distinct ids never share a name.
fn file_name_safe_spelling(text: &str) -> String {
    let mut spelling = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_' {
            spelling.push(byte as char);
        } else {
            spelling.push_str(&format!(".{byte:02x}"));
        }
    }
    spelling
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_stream_id_keeps_its_spelling() {
        assert_eq!(
            tenant_profile_directory(Path::new("/cache"), "lower-third_2"),
            Path::new("/cache/stream-profile-lower-third_2")
        );
    }

    #[test]
    fn no_stream_id_reaches_outside_the_root_cache() {
        let directory = tenant_profile_directory(Path::new("/cache"), "../../etc");
        assert_eq!(directory.parent(), Some(Path::new("/cache")));
        assert_eq!(
            directory,
            Path::new("/cache/stream-profile-.2e.2e.2f.2e.2e.2fetc")
        );
    }

    #[test]
    fn distinct_stream_ids_never_share_a_profile() {
        assert_ne!(
            file_name_safe_spelling("a.b"),
            file_name_safe_spelling("a.2eb")
        );
        assert_ne!(
            file_name_safe_spelling("a/b"),
            file_name_safe_spelling("a.b")
        );
    }
}
