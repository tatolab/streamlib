// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

/// Compute a deterministic checksum from a JSON value.
pub fn compute_json_checksum(value: &serde_json::Value) -> u64 {
    let mut hasher = DefaultHasher::new();
    // Use canonical string representation for deterministic hashing
    value.to_string().hash(&mut hasher);
    hasher.finish()
}
