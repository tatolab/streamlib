// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn a_path_inside_its_own_folder_has_no_scheme_root_or_parent_step() {
    for inside in ["index.html", "assets/icon.svg", "a..b/c"] {
        assert!(is_a_path_inside_its_own_folder(inside), "{inside}");
    }
    for outside in [
        "",
        "/index.html",
        "../index.html",
        "a/./b",
        "a//b",
        "a\\b",
        "https://example.com/x",
        "data:text/html,x",
    ] {
        assert!(!is_a_path_inside_its_own_folder(outside), "{outside}");
    }
}

#[test]
fn a_contract_version_is_two_canonical_unsigned_integers() {
    for version in ["0.0", "1.0", "1.12", "10.3"] {
        assert!(is_a_contract_version(version), "{version}");
    }
    for not_a_version in [
        "", "1", "1.", ".1", "01.1", "1.01", "1.1.1", "-1.0", "one.two",
    ] {
        assert!(!is_a_contract_version(not_a_version), "{not_a_version}");
    }
}

#[test]
fn a_contract_version_is_read_up_to_the_highest_minor_of_its_major() {
    assert!(is_a_contract_version_of_major_up_to_minor("1.0", 1, 1));
    assert!(is_a_contract_version_of_major_up_to_minor("1.1", 1, 1));
    assert!(!is_a_contract_version_of_major_up_to_minor("1.2", 1, 1));
    assert!(!is_a_contract_version_of_major_up_to_minor("2.0", 1, 1));
    assert!(!is_a_contract_version_of_major_up_to_minor("0.9", 1, 1));
}
