// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Tests for `streamlib::sdk::schema_ident_any_version!` — the
//! version-omitting companion of `schema_ident!` that resolves the
//! version at runtime against the global processor registry.
//!
//! The macro performs compile-time validation of `(org, package, type)`
//! and expands to a `PROCESSOR_REGISTRY.resolve_any_version(...)` call
//! that returns `Result<SchemaIdent, Error>`. These tests assert the
//! end-to-end behavior: the registry is consulted, the highest semver
//! wins when multiple are registered, and `Error::UnknownProcessorType`
//! surfaces cleanly when nothing matches.

use streamlib_engine::core::ProcessorDescriptor;
use streamlib_engine::core::descriptors::{Org, Package, SchemaIdent, SemVer, TypeName};
use streamlib_engine::core::error::Error;
use streamlib_engine::core::processors::PROCESSOR_REGISTRY;

fn ident(org: &str, pkg: &str, ty: &str, v: SemVer) -> SchemaIdent {
    SchemaIdent::new(
        Org::new(org).unwrap(),
        Package::new(pkg).unwrap(),
        TypeName::new(ty).unwrap(),
        v,
    )
}

#[test]
fn macro_resolves_to_highest_registered_version() {
    // Register three versions of the same `(org, package, type)` tuple
    // under deliberately collision-resistant names. We don't rely on
    // any in-tree processor for this test — registering against the
    // global PROCESSOR_REGISTRY directly keeps the test hermetic to
    // whatever processors happen to be linked.
    let v1 = ident(
        "schema-ident-any-version-test",
        "fixture-pkg",
        "FixtureProc",
        SemVer::new(1, 0, 0),
    );
    let v2 = ident(
        "schema-ident-any-version-test",
        "fixture-pkg",
        "FixtureProc",
        SemVer::new(1, 5, 3),
    );
    let v3 = ident(
        "schema-ident-any-version-test",
        "fixture-pkg",
        "FixtureProc",
        SemVer::new(2, 1, 0),
    );

    // `register_descriptor_only` is idempotent across test runs in the
    // sense that duplicate inserts return an Err; we want the inserts
    // to succeed when this test runs first, and we want the lookup to
    // succeed when this test runs after a sibling has already inserted
    // the same entries. Either direction is fine — what matters is that
    // by the time the macro fires, all three versions are present.
    let _ =
        PROCESSOR_REGISTRY.register_descriptor_only(ProcessorDescriptor::new(v1.clone(), "test"));
    let _ =
        PROCESSOR_REGISTRY.register_descriptor_only(ProcessorDescriptor::new(v3.clone(), "test"));
    let _ =
        PROCESSOR_REGISTRY.register_descriptor_only(ProcessorDescriptor::new(v2.clone(), "test"));

    let resolved: SchemaIdent = streamlib::sdk::schema_ident_any_version!(
        "schema-ident-any-version-test",
        "fixture-pkg",
        "FixtureProc",
    )
    .expect("registry has at least one match");

    assert_eq!(resolved, v3, "macro must pick the highest semver");
}

#[test]
fn macro_returns_unknown_processor_type_when_unregistered() {
    // No registration for this triple. `_unregistered` suffix avoids
    // accidental collision with anything else linked in.
    let result: Result<SchemaIdent, Error> = streamlib::sdk::schema_ident_any_version!(
        "schema-ident-any-version-test",
        "fixture-pkg-unregistered",
        "NeverRegistered"
    );

    match result {
        Err(Error::UnknownProcessorType { ident }) => {
            assert_eq!(ident.org.as_str(), "schema-ident-any-version-test");
            assert_eq!(ident.package.as_str(), "fixture-pkg-unregistered");
            assert_eq!(ident.r#type.as_str(), "NeverRegistered");
        }
        Err(other) => panic!("expected UnknownProcessorType, got {other:?}"),
        Ok(found) => panic!("expected Err, got Ok({found})"),
    }
}

#[test]
fn macro_accepts_trailing_comma() {
    // Same shape as `schema_ident!`'s trailing-comma test — the parser
    // should tolerate an optional trailing comma after the type arg.
    let result: Result<SchemaIdent, Error> = streamlib::sdk::schema_ident_any_version!(
        "schema-ident-any-version-test",
        "fixture-pkg-trailing-comma",
        "DoesNotMatter",
    );
    // Doesn't matter whether registered — we only care that the macro
    // expands and the call compiles + runs.
    let _ = result;
}
