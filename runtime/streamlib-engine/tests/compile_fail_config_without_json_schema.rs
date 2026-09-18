// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The refusal an author meets when a `config =` type does not derive
//! `JsonSchema`.
//!
//! The descriptor carries the config type's schema, so the derive is a
//! requirement rather than an option. Without a compile-fail case nothing runs
//! the compiler over a config type that lacks it, and the note that names the
//! fix could rot into a bare trait-bound error with every other test green.
//!
//! The expectation carries rustc's multiple-versions note, because the engine's
//! graph holds two `schemars`: ours at 0.8 and the 1.x `zenoh-keyexpr` pulls in
//! behind its `std` feature. Both are the version each crate chose and neither
//! type crosses into the other, so the note is what an author genuinely sees —
//! locked here rather than filtered out, since a filter would also hide a
//! duplicate that did matter.
//!
//! Refresh the expected output with `TRYBUILD=overwrite cargo test -p
//! streamlib-engine --test compile_fail_config_without_json_schema` after a
//! compiler upgrade reflows a diagnostic, or after a dependency change moves
//! what is in the graph.

#[test]
fn a_config_type_without_the_derive_is_refused_with_the_derive_and_its_path() {
    trybuild::TestCases::new().compile_fail("tests/compile_fail/*.rs");
}
