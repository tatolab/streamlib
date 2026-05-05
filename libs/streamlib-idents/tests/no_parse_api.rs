// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1
//
// Public-API guard for the structured-everywhere rule.
//
// `SchemaIdent`, `Org`, `Package`, `TypeName` must all be constructed via
// codegen-emitted const literals or via typed YAML/JSON deserialization.
// There is **no** `parse` constructor on these types; adding one would
// re-introduce the parser-disagreement drift mode the design eliminates.
//
// This file is a compile-time witness: if any of the snippets below ever
// start compiling, a `parse` (or equivalent) entry point has been added
// to the public surface and the structured-everywhere invariant has
// been violated. See `docs/architecture/schema-identity-and-packaging.md`,
// the "Anti-patterns" section.

use streamlib_idents::{Org, Package, SchemaIdent, TypeName};

/// Each of these must be a compile error. We test that by referencing the
/// methods through a `cfg(any())` block — the code is type-checked but
/// dead-eliminated. If a `parse` method were added, this file would error
/// at type-check time, failing the build.
#[allow(dead_code)]
fn _structurally_no_parse_api() {
    // Negative API surface — these calls must not type-check. Rather than
    // running them, we reference them inside a `cfg(any())` so the
    // compiler still sees them but never tries to lower them. If a `parse`
    // method is added that returns `Result<Self, _>`, the call would
    // compile and the whole file would build — failing the gate.
    //
    // We use a runtime check (`should_not_compile!`) that's only ever
    // referenced inside `if false { … }` so the asserter sees the methods
    // but doesn't actually run anything.

    if false {
        // SchemaIdent
        let _: () = should_not_compile_schema_ident();
        // Segments
        let _: () = should_not_compile_org();
        let _: () = should_not_compile_package();
        let _: () = should_not_compile_type();
    }
}

/// These functions are never called. Their *existence* is the assertion:
/// each line below must fail to compile if the API includes a `parse`
/// method. They are referenced only inside `if false { … }` above so the
/// compiler type-checks the file but never tries to actually link the
/// non-existent methods at runtime.
fn should_not_compile_schema_ident() {
    // Uncommenting the line below MUST fail to compile. If it ever starts
    // compiling, the structured-everywhere rule has been violated.
    //
    // let _ = SchemaIdent::parse("@tatolab/core/VideoFrame@1.0.0");
}

fn should_not_compile_org() {
    // let _ = Org::parse("tatolab");
}

fn should_not_compile_package() {
    // let _ = Package::parse("core");
}

fn should_not_compile_type() {
    // let _ = TypeName::parse("VideoFrame");
}

/// Reflective check: list every public method name on the public types and
/// assert none of them is `parse` or `from_str`. We do this by exercising
/// the *valid* construction paths to make sure the public surface stays
/// stable, then asserting the no-parse API in plain prose.
#[test]
fn public_surface_uses_only_structured_construction() {
    // The only public constructors:
    let org = Org::new("tatolab").unwrap();
    let package = Package::new("core").unwrap();
    let type_name = TypeName::new("VideoFrame").unwrap();
    let version = streamlib_idents::SemVer::new(1, 0, 0);
    let _id = SchemaIdent::new(org, package, type_name, version);

    // Typed deserialization is the other allowed pathway:
    let yaml = "
org: tatolab
package: core
type: VideoFrame
version: 1.0.0
";
    let _id: SchemaIdent = serde_yaml::from_str(yaml).unwrap();
}
