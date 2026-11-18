// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

const RUNTIME_VERSION: u32 = 3;
const RUNTIME_REVISION: u32 = 2;

fn the_older_side_against_the_runtime(version: u32, revision: u32) -> Option<OlderSide> {
    the_older_side_of(version, revision, RUNTIME_VERSION, RUNTIME_REVISION)
}

#[test]
fn a_revision_a_client_names_none_of_is_read_as_zero() {
    assert_eq!(
        ContractAClientNames::named(Some(RUNTIME_VERSION), None).revision,
        0
    );
}

#[test]
fn a_client_of_the_same_version_and_a_revision_no_higher_is_served() {
    assert_eq!(the_older_side_against_the_runtime(RUNTIME_VERSION, 0), None);
    assert_eq!(
        the_older_side_against_the_runtime(RUNTIME_VERSION, RUNTIME_REVISION),
        None
    );
}

#[test]
fn a_client_of_a_higher_revision_finds_the_runtime_older() {
    assert_eq!(
        the_older_side_against_the_runtime(RUNTIME_VERSION, RUNTIME_REVISION + 1),
        Some(OlderSide::Runtime)
    );
}

#[test]
fn a_client_of_another_version_finds_the_side_of_the_lower_version_older() {
    assert_eq!(
        the_older_side_against_the_runtime(RUNTIME_VERSION + 1, 0),
        Some(OlderSide::Runtime)
    );
    assert_eq!(
        the_older_side_against_the_runtime(RUNTIME_VERSION - 1, RUNTIME_REVISION + 5),
        Some(OlderSide::Client)
    );
}
