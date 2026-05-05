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
// ## Where the actual gate lives
//
// The compile-time witness for "no `parse` API" lives in `compile_fail`
// doctests on each type definition (`SchemaIdent`, `Org`, `Package`,
// `TypeName`) — see their rustdoc. Those doctests assert the
// forbidden snippets MUST fail to compile; if a `parse` (or `FromStr`)
// API is ever added, the doctests would compile cleanly, the `compile_fail`
// assertion would flip, and `cargo test --doc -p streamlib-idents` would
// surface the regression.
//
// This integration test is the *positive* counterpart: it locks the
// allowed construction pathways (`Type::new` validating constructors and
// typed YAML deserialization) so that whatever change adds a new public
// constructor has to pass through here too. If a `parse` API were
// accidentally added in the same change that removed one of the allowed
// constructors, the doctest would catch the addition and this test would
// catch the removal — together they bracket the public surface.

use streamlib_idents::{Org, Package, SchemaIdent, SemVer, TypeName};

#[test]
fn allowed_construction_paths_remain_intact() {
    // Path 1 — codegen-style: validating segment constructors + struct
    // literal. This is what the rust-side macro emits.
    let org = Org::new("tatolab").unwrap();
    let package = Package::new("core").unwrap();
    let type_name = TypeName::new("VideoFrame").unwrap();
    let version = SemVer::new(1, 0, 0);
    let id = SchemaIdent::new(org, package, type_name, version);
    assert_eq!(id.to_string(), "@tatolab/core/VideoFrame@1.0.0");

    // Path 2 — typed YAML deserialization: each segment is its own field.
    // The wire shape is structured fields, not a joined string.
    let yaml = "
org: tatolab
package: core
type: VideoFrame
version: 1.0.0
";
    let id: SchemaIdent = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(id.org.as_str(), "tatolab");
    assert_eq!(id.package.as_str(), "core");
    assert_eq!(id.r#type.as_str(), "VideoFrame");
    assert_eq!(id.version, SemVer::new(1, 0, 0));

    // Path 3 — typed JSON deserialization: same structured shape over JSON.
    let json = r#"{"org":"tatolab","package":"core","type":"VideoFrame","version":"1.0.0"}"#;
    let id: SchemaIdent = serde_json::from_str(json).unwrap();
    assert_eq!(id.to_string(), "@tatolab/core/VideoFrame@1.0.0");
}

#[test]
fn joined_string_is_not_a_valid_yaml_shape() {
    // The Display form `@org/package/Type@version` is render-only — feeding
    // it back as a YAML string for a SchemaIdent must NOT round-trip. If
    // someone added a custom Deserialize impl that accepts the joined form,
    // this test would start passing-with-the-wrong-id and the assertion
    // below would fire.
    let yaml = "\"@tatolab/core/VideoFrame@1.0.0\"";
    let res: Result<SchemaIdent, _> = serde_yaml::from_str(yaml);
    assert!(
        res.is_err(),
        "joined-string deserialization MUST fail — structured-everywhere rule"
    );
}
