// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Tests for `streamlib::sdk::schema_ident!` — the convenience macro that
//! is a short form of `SchemaIdent::new(Org::new(...), ...)`.
//!
//! The macro takes the same four fields as the long form. These tests
//! assert the macro output matches the long form byte-for-byte.

use streamlib_engine::core::descriptors::{Org, Package, SchemaIdent, SemVer, TypeName};

#[test]
fn macro_matches_long_form() {
    let short = streamlib::sdk::schema_ident!("tatolab", "polyglot-foo", "PolyglotFoo", "1.2.3");

    let long = SchemaIdent::new(
        Org::new("tatolab").unwrap(),
        Package::new("polyglot-foo").unwrap(),
        TypeName::new("PolyglotFoo").unwrap(),
        SemVer::new(1, 2, 3),
    );

    assert_eq!(short, long);
}

#[test]
fn macro_accepts_zero_version() {
    let id = streamlib::sdk::schema_ident!("tatolab", "core", "VideoFrame", "0.0.0");
    assert_eq!(id.version, SemVer::new(0, 0, 0));
}

#[test]
fn macro_accepts_trailing_comma() {
    let id = streamlib::sdk::schema_ident!("tatolab", "core", "VideoFrame", "1.0.0",);
    assert_eq!(id.version, SemVer::new(1, 0, 0));
}
