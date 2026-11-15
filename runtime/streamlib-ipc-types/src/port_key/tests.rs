// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn port_key_accepts_max_length_name() {
    // Exact-fit boundary: a name of MAX_NAME_BYTES must construct.
    let name = "a".repeat(PortKey::MAX_NAME_BYTES);
    let key = PortKey::new(&name).expect("max-length name must construct");
    assert_eq!(key.as_str(), name);
}

#[test]
fn port_key_rejects_over_length_name_instead_of_truncating() {
    // A name one byte past the wire capacity must be a named error, NOT a clipped key
    // that routes frames to the wrong port. Revert `PortKey::new` to the
    // `.min(MAX_PORT_KEY_SIZE - 1)` truncation and this fails — the
    // construction would succeed and `as_str()` would return the clipped
    // 63-byte prefix.
    let over = "b".repeat(PortKey::MAX_NAME_BYTES + 1);
    assert_eq!(over.len(), 64);
    assert_eq!(
        PortKey::new(&over),
        Err(PortKeyError::TooLong { len: 64, max: 63 })
    );
}
