// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

/// The catalog names each module archive's copy version by this same rule, in
/// `scripts/tatolab_module_projects.py`, so this value is pinned there too: change both or neither.
#[test]
fn an_imports_version_is_the_sha256_of_its_files_hashes_as_canonical_json() {
    let copied_files = BTreeMap::from([
        (
            "tatolab-project.json".to_owned(),
            content_sha256_of(b"{}\n"),
        ),
        ("wheels/a.whl".to_owned(), content_sha256_of(b"wheel")),
    ]);
    assert_eq!(
        version_of_the_copied_files(&copied_files).unwrap(),
        "e26dca00880b66fe5d7f2f49e2a238ec7c9d881b48793ba29f09dd77d1c397a9"
    );
}
