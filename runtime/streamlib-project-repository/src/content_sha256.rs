// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The SHA-256 a file's bytes are named by, lowercase hex, as `read_project_file` answers it and
//! a sidecar records it.

use sha2::{Digest, Sha256};

/// The SHA-256 of `bytes`, lowercase hex.
pub fn content_sha256_of(bytes: &[u8]) -> String {
    lowercase_hex(&Sha256::digest(bytes))
}

/// `bytes` as lowercase hex, two digits each.
pub(crate) fn lowercase_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
