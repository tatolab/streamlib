// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Procedural macros for streamlib.
//!
//! - `#[streamlib::processor("Camera")]` — processor definition by
//!   PascalCase short name. The macro reads `CARGO_MANIFEST_DIR/streamlib.yaml`
//!   and resolves the full structured [`SchemaIdent`] from the package's
//!   `package: { org, name, version }` block plus the matching entry in
//!   the `processors:` list.
//! - `streamlib::sdk::schema_ident_any_version!("org", "package", "Type")`
//!   — **canonical, default form.** Validates `(org, package, type)` at
//!   compile time; resolves the version at runtime against the global
//!   processor registry (highest installed `SemVer` wins, Cargo / npm
//!   convention). Returns `Result<SchemaIdent, Error>` —
//!   `Error::UnknownProcessorType` when nothing matches.
//! - `streamlib::sdk::schema_ident!("org", "package", "Type", "1.0.0")` —
//!   strict version-pinning form. Same four fields as the long
//!   [`SchemaIdent::new`] constructor, validated at compile time,
//!   expands to the long form verbatim. Reach for this when you have a
//!   reason to refuse newer-but-compatible registered versions; the
//!   `_any_version` form is the right default for everything else.

mod analysis;
mod attributes;
mod codegen;
mod config_descriptor;

use proc_macro::TokenStream;
use quote::quote;
use std::path::Path;
use streamlib_processor_schema::{
    Org, Package, PackageMetadata, PortSchemaSpec, ProcessorSchema, ProjectConfigMinimal,
    SchemaIdent, SemVer, TypeName,
};

// Range parsing for `module_ident!*` macros. The streamlib_idents crate
// owns the SemVerRange grammar; the macro just forwards the input through
// it at expansion time so invalid ranges surface as compile errors.
use streamlib_idents::SemVerRange;
use syn::{
    parse::{Parse, ParseStream},
    parse_macro_input, DeriveInput, ItemStruct, LitStr, Token,
};

/// Main processor attribute macro.
///
/// Transforms a struct definition into a processor module by looking up a
/// PascalCase short name in `streamlib.yaml`'s `processors:` list and
/// composing the full structured `SchemaIdent` from the package's
/// `package: { org, name, version }` block.
///
/// Example: `#[streamlib::processor("Camera")]` resolves to
/// `SchemaIdent { org: tatolab, package: streamlib, type: Camera,
/// version: <package.version> }` when used inside a manifest declaring
/// `package: { org: tatolab, name: streamlib, ... }`.
///
/// The macro emits the processor's type, port markers, descriptor, and
/// `schema_ident()` accessor — but does NOT register the processor in
/// the global `PROCESSOR_REGISTRY`. Callers register processors through
/// one of two paths:
///
/// - **Cdylib packages** declare `crate-type = ["rlib", "cdylib"]` and
///   call `export_plugin!(...)` from `lib.rs`. The runtime `dlopen()`s
///   the cdylib at `runtime.add_module(...)` time; the plugin ABI's
///   `STREAMLIB_PLUGIN` callback registers each processor via the host's
///   `processor_register` callback.
/// - **In-process Rust callers** invoke
///   `PROCESSOR_REGISTRY.register::<Foo::Processor>()` directly. Tests
///   and engine-internal mocks use this path.
#[proc_macro_attribute]
pub fn processor(attr: TokenStream, item: TokenStream) -> TokenStream {
    let item_struct = parse_macro_input!(item as ItemStruct);

    let short_name = match parse_processor_attr(attr) {
        Ok(name) => name,
        Err(err) => return err.to_compile_error().into(),
    };

    let (schema, schema_ident, config_schema_id) =
        match load_processor_schema(&short_name, &item_struct) {
            Ok(triple) => triple,
            Err(err) => return err.to_compile_error().into(),
        };

    let generated = codegen::generate_from_processor_schema(
        &item_struct,
        &schema,
        &schema_ident,
        config_schema_id.as_deref(),
    );

    TokenStream::from(generated)
}

/// Parse the processor's PascalCase short name out of the attribute
/// arguments.
fn parse_processor_attr(attr: TokenStream) -> syn::Result<String> {
    use syn::parse::Parser;

    let parser = |input: syn::parse::ParseStream<'_>| -> syn::Result<String> {
        let name: LitStr = input.parse()?;
        if !input.is_empty() {
            input.parse::<Token![,]>()?;
            if !input.is_empty() {
                let extra: syn::Ident = input.parse()?;
                return Err(syn::Error::new(
                    extra.span(),
                    format!(
                        "unexpected processor attribute argument `{}` — the macro \
                         takes only the PascalCase short name (e.g. `#[processor(\"Camera\")]`)",
                        extra
                    ),
                ));
            }
        }
        Ok(name.value())
    };
    parser.parse(attr.into())
}

/// Locate `CARGO_MANIFEST_DIR/streamlib.yaml`, resolve the package metadata
/// + matching processor entry by short name, and compose the full
/// [`SchemaIdent`]. Also resolves any bare-name [`PortSchemaSpec::Named`]
/// references on the matched processor's port and config schemas against
/// the manifest's `schemas:` map (#767), in-place rewriting them to
/// [`PortSchemaSpec::Specific`]. Downstream codegen sees `Any` or
/// `Specific` only — `Named` never reaches the token-emission layer.
///
/// Returns the (mutated) processor schema, the processor's structured
/// [`SchemaIdent`], and an optional pre-resolved canonical id string for
/// the config schema (or `None` when the processor declares no config).
fn load_processor_schema(
    short_name: &str,
    item: &ItemStruct,
) -> syn::Result<(ProcessorSchema, SchemaIdent, Option<String>)> {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").map_err(|_| {
        syn::Error::new_spanned(
            item,
            "CARGO_MANIFEST_DIR not set. This macro must be used within a Cargo build.",
        )
    })?;

    let manifest_dir_path = Path::new(&manifest_dir);
    let config_path = manifest_dir_path.join("streamlib.yaml");

    if !config_path.exists() {
        return Err(syn::Error::new_spanned(
            item,
            format!(
                "streamlib.yaml not found at {}\n\
                 The #[streamlib::processor(\"<ShortName>\")] macro requires a streamlib.yaml\n\
                 next to Cargo.toml with a `package:` block and a matching processor entry.",
                config_path.display()
            ),
        ));
    }

    let yaml_content = std::fs::read_to_string(&config_path).map_err(|e| {
        syn::Error::new_spanned(item, format!("Failed to read streamlib.yaml: {}", e))
    })?;

    let config: ProjectConfigMinimal = serde_yaml::from_str(&yaml_content).map_err(|e| {
        syn::Error::new_spanned(item, format!("Failed to parse streamlib.yaml: {}", e))
    })?;

    let pkg: PackageMetadata = config.package.ok_or_else(|| {
        syn::Error::new_spanned(
            item,
            format!(
                "streamlib.yaml at {} is missing a `package:` block. The processor macro requires `package: {{ org, name, version }}` to construct a full SchemaIdent for `{}`.",
                config_path.display(),
                short_name
            ),
        )
    })?;

    let available_names: Vec<String> = config.processors.iter().map(|p| p.name.clone()).collect();

    let mut schema = config
        .processors
        .into_iter()
        .find(|p| p.name == short_name)
        .ok_or_else(|| {
            let mut msg = format!(
                "Processor '{}' not found in streamlib.yaml at {}",
                short_name,
                config_path.display()
            );
            if !available_names.is_empty() {
                msg.push_str("\n  Available processors:");
                for name in &available_names {
                    msg.push_str(&format!("\n    - {}", name));
                }
            }
            syn::Error::new_spanned(item, msg)
        })?;

    let type_name = TypeName::new(short_name).map_err(|e| {
        syn::Error::new_spanned(
            item,
            format!(
                "processor short name `{}` is not valid PascalCase: {}",
                short_name, e
            ),
        )
    })?;

    let ident = SchemaIdent::new(
        pkg.org.clone(),
        pkg.name.clone(),
        type_name,
        pkg.version,
    );

    // Resolve bare-name port + config schema references against the
    // enclosing manifest's `schemas:` map (#767). After this pass, every
    // `PortSchemaSpec::Named` on this processor's ports is replaced with
    // `PortSchemaSpec::Specific(SchemaIdent)`; `config.schema`'s
    // canonical id is computed and returned to the caller for codegen.
    //
    // Skip resolution entirely when the processor has no `Named` /
    // config refs to resolve — avoids invoking the resolver (which
    // touches the dependency graph) for processors with `any`-only
    // ports and no config.
    let needs_resolution = schema
        .inputs
        .iter()
        .chain(schema.outputs.iter())
        .any(|p| matches!(p.schema, PortSchemaSpec::Named(_)))
        || schema.config.is_some();

    let config_schema_id = if needs_resolution {
        let resolved = streamlib_idents::resolve_with(
            manifest_dir_path,
            &streamlib_idents::ResolverOptions::default(),
        )
        .map_err(|e| {
            syn::Error::new_spanned(
                item,
                format!(
                    "Failed to resolve manifest dependencies for bare-name schema lookup at {}: {}",
                    config_path.display(),
                    e
                ),
            )
        })?;

        // Resolve port schemas in-place.
        for port in schema.inputs.iter_mut().chain(schema.outputs.iter_mut()) {
            if let PortSchemaSpec::Named(name) = &port.schema {
                let resolved_ident = resolve_named_to_ident(&resolved, name).map_err(|msg| {
                    syn::Error::new_spanned(
                        item,
                        format!(
                            "port `{}` in processor `{}`: {}",
                            port.name, short_name, msg
                        ),
                    )
                })?;
                port.schema = PortSchemaSpec::Specific(resolved_ident);
            }
        }

        // Resolve config schema (TypeName) to a canonical id string for
        // the codegen `derive_config_type_from_schema` helper.
        if let Some(config) = &schema.config {
            let id = resolve_config_schema_to_canonical_id(&resolved, &config.schema).map_err(
                |msg| {
                    syn::Error::new_spanned(
                        item,
                        format!(
                            "config schema `{}` in processor `{}`: {}",
                            config.schema.as_str(),
                            short_name,
                            msg
                        ),
                    )
                },
            )?;
            Some(id)
        } else {
            None
        }
    } else {
        None
    };

    Ok((schema, ident, config_schema_id))
}

/// Walk the resolved-packages graph for a bare TypeName reference and
/// build the fully-qualified [`SchemaIdent`] from the owning package's
/// metadata. Reads the schema file's `metadata.type` (preferred) for
/// the type segment; falls back to the bare name if the YAML lacks
/// `metadata.type` (legacy reverse-DNS schemas with `metadata.name` only
/// don't carry a separate type, so the bare-name lookup form falls back
/// to the map key, which is the bare PascalCase the user wrote).
fn resolve_named_to_ident(
    resolved: &streamlib_idents::ResolvedPackages,
    name: &TypeName,
) -> Result<SchemaIdent, String> {
    let (owner, schema_path) =
        streamlib_idents::resolve_bare_schema_name(resolved, &resolved.root, name)
            .map_err(|e| format!("bare-name resolution failed: {}", e))?;

    let owner_pkg = owner
        .manifest
        .package
        .as_ref()
        .ok_or_else(|| "owning package has no `package:` block".to_string())?;

    // Prefer `metadata.type` from the schema file when present; fall back
    // to the bare map-key name. This preserves the round-trip identity
    // for new-shape schemas while tolerating legacy reverse-DNS schemas
    // that only declare `metadata.name`.
    let type_segment = read_schema_metadata_type(&schema_path).unwrap_or_else(|| name.clone());

    Ok(SchemaIdent::new(
        owner_pkg.org.clone(),
        owner_pkg.name.clone(),
        type_segment,
        owner_pkg.version,
    ))
}

/// Resolve a config schema bare-name `TypeName` to its canonical id
/// string for the codegen helper `derive_config_type_from_schema`.
///
/// Two id grammars are supported (and downstream codegen handles both):
/// - New-shape `@<org>/<package>/<TypeName>@<version>` — emitted when
///   the schema declares `metadata.type`
/// - Legacy reverse-DNS `<segments>.config@<version>` — emitted when
///   the schema declares only `metadata.name` (the legacy reverse-DNS
///   filename form). The semver suffix is appended from the owning
///   package's version.
fn resolve_config_schema_to_canonical_id(
    resolved: &streamlib_idents::ResolvedPackages,
    name: &TypeName,
) -> Result<String, String> {
    let (owner, schema_path) =
        streamlib_idents::resolve_bare_schema_name(resolved, &resolved.root, name)
            .map_err(|e| format!("bare-name resolution failed: {}", e))?;

    let owner_pkg = owner
        .manifest
        .package
        .as_ref()
        .ok_or_else(|| "owning package has no `package:` block".to_string())?;

    if let Some(type_segment) = read_schema_metadata_type(&schema_path) {
        Ok(format!(
            "@{}/{}/{}@{}",
            owner_pkg.org.as_str(),
            owner_pkg.name.as_str(),
            type_segment.as_str(),
            owner_pkg.version,
        ))
    } else if let Some(legacy_name) = read_schema_metadata_name(&schema_path) {
        // Legacy reverse-DNS — the metadata.name carries the canonical
        // unversioned id; append the owning package's semver to match
        // the long-form `<dotted>.config@<version>` codegen helper expects.
        Ok(format!("{}@{}", legacy_name, owner_pkg.version))
    } else {
        Err(format!(
            "schema {} declares neither `metadata.type` nor `metadata.name`",
            schema_path.display()
        ))
    }
}

/// Read `metadata.type` from a schema YAML file, returning `None` when
/// missing or when the file can't be read / parsed.
fn read_schema_metadata_type(schema_path: &Path) -> Option<TypeName> {
    let body = std::fs::read_to_string(schema_path).ok()?;
    let value: serde_yaml::Value = serde_yaml::from_str(&body).ok()?;
    let type_str = value.get("metadata")?.get("type")?.as_str()?;
    TypeName::new(type_str).ok()
}

/// Read `metadata.name` (legacy reverse-DNS form) from a schema YAML
/// file. Returns `None` when missing or when the file can't be read.
fn read_schema_metadata_name(schema_path: &Path) -> Option<String> {
    let body = std::fs::read_to_string(schema_path).ok()?;
    let value: serde_yaml::Value = serde_yaml::from_str(&body).ok()?;
    let name = value.get("metadata")?.get("name")?.as_str()?;
    Some(name.to_string())
}

/// Short form of [`SchemaIdent::new`] — strict version-pinning. Takes
/// the same four fields as the long-form constructor (org, package,
/// type, version) as string literals, validates each at compile time,
/// and expands to the equivalent `SchemaIdent::new(...)` expression.
///
/// **Prefer [`schema_ident_any_version!`] for the common case.** Reach
/// for `schema_ident!` only when you have a deliberate reason to refuse
/// any version other than the one you typed: tests asserting against a
/// specific historical version, callers that bind to a known-broken
/// version they don't want auto-upgraded out of, or any other case
/// where strict pinning is the *intent*. For "match whatever's
/// registered" — the dominant case — use `schema_ident_any_version!`.
///
/// ```ignore
/// // Long form (5 lines):
/// SchemaIdent::new(
///     Org::new("tatolab").unwrap(),
///     Package::new("polyglot-continuous-processor").unwrap(),
///     TypeName::new("PolyglotContinuousProcessor").unwrap(),
///     SemVer::new(1, 0, 0),
/// )
///
/// // Short form (1 line):
/// schema_ident!("tatolab", "polyglot-continuous-processor", "PolyglotContinuousProcessor", "1.0.0")
/// ```
///
/// Each segment is validated at proc-macro expansion: invalid org / package /
/// type / semver becomes a compile error, never a runtime panic.
#[proc_macro]
pub fn schema_ident(input: TokenStream) -> TokenStream {
    let args = parse_macro_input!(input as SchemaIdentArgs);
    match expand_schema_ident(&args) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.to_compile_error().into(),
    }
}

/// **Canonical, default form** for naming a processor at a call site.
/// Omits the version arg and resolves it at runtime from the global
/// processor registry, picking the highest registered `SemVer` for the
/// `(org, package, type)` tuple (Cargo / npm convention).
///
/// This is the right shape for nearly every call site — the spawning
/// binary should match whatever version of a processor happens to be
/// registered when `runtime.add_module(...)` finishes. Reach for the
/// strict-pin [`schema_ident!`] form only when you have a deliberate
/// reason to refuse newer-but-compatible registered versions.
///
/// ```ignore
/// // Compile-time:  org / package / type validated at proc-macro expansion.
/// // Runtime:       PROCESSOR_REGISTRY.resolve_any_version(...) picks the
/// //                highest semver and returns Result<SchemaIdent, Error>.
/// let id: SchemaIdent =
///     streamlib::sdk::schema_ident_any_version!("tatolab", "polyglot-foo", "PolyglotFoo")?;
/// ```
///
/// Returns `Result<SchemaIdent, streamlib::sdk::error::Error>`. `Error::UnknownProcessorType`
/// is returned when no registration matches `(org, package, type)`.
#[proc_macro]
pub fn schema_ident_any_version(input: TokenStream) -> TokenStream {
    let args = parse_macro_input!(input as SchemaIdentAnyVersionArgs);
    match expand_schema_ident_any_version(&args) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.to_compile_error().into(),
    }
}

struct SchemaIdentAnyVersionArgs {
    org: LitStr,
    package: LitStr,
    type_name: LitStr,
}

impl Parse for SchemaIdentAnyVersionArgs {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let org: LitStr = input.parse()?;
        input.parse::<Token![,]>()?;
        let package: LitStr = input.parse()?;
        input.parse::<Token![,]>()?;
        let type_name: LitStr = input.parse()?;
        // Tolerate an optional trailing comma.
        let _ = input.parse::<Token![,]>();
        Ok(Self {
            org,
            package,
            type_name,
        })
    }
}

fn expand_schema_ident_any_version(
    args: &SchemaIdentAnyVersionArgs,
) -> syn::Result<proc_macro2::TokenStream> {
    let org_str = args.org.value();
    let package_str = args.package.value();
    let type_str = args.type_name.value();

    Org::new(&org_str).map_err(|e| {
        syn::Error::new(
            args.org.span(),
            format!("invalid org `{}`: {}", org_str, e),
        )
    })?;
    Package::new(&package_str).map_err(|e| {
        syn::Error::new(
            args.package.span(),
            format!("invalid package `{}`: {}", package_str, e),
        )
    })?;
    TypeName::new(&type_str).map_err(|e| {
        syn::Error::new(
            args.type_name.span(),
            format!("invalid type name `{}`: {}", type_str, e),
        )
    })?;

    Ok(quote! {
        ::streamlib::sdk::processors::PROCESSOR_REGISTRY.resolve_any_version(
            &::streamlib::sdk::descriptors::Org::new(#org_str).expect("validated by macro"),
            &::streamlib::sdk::descriptors::Package::new(#package_str).expect("validated by macro"),
            &::streamlib::sdk::descriptors::TypeName::new(#type_str).expect("validated by macro"),
        )
    })
}

struct SchemaIdentArgs {
    org: LitStr,
    package: LitStr,
    type_name: LitStr,
    version: LitStr,
}

impl Parse for SchemaIdentArgs {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let org: LitStr = input.parse()?;
        input.parse::<Token![,]>()?;
        let package: LitStr = input.parse()?;
        input.parse::<Token![,]>()?;
        let type_name: LitStr = input.parse()?;
        input.parse::<Token![,]>()?;
        let version: LitStr = input.parse()?;
        // Tolerate an optional trailing comma.
        let _ = input.parse::<Token![,]>();
        Ok(Self {
            org,
            package,
            type_name,
            version,
        })
    }
}

fn expand_schema_ident(args: &SchemaIdentArgs) -> syn::Result<proc_macro2::TokenStream> {
    let org_str = args.org.value();
    let package_str = args.package.value();
    let type_str = args.type_name.value();
    let version_str = args.version.value();

    Org::new(&org_str).map_err(|e| {
        syn::Error::new(
            args.org.span(),
            format!("invalid org `{}`: {}", org_str, e),
        )
    })?;
    Package::new(&package_str).map_err(|e| {
        syn::Error::new(
            args.package.span(),
            format!("invalid package `{}`: {}", package_str, e),
        )
    })?;
    TypeName::new(&type_str).map_err(|e| {
        syn::Error::new(
            args.type_name.span(),
            format!("invalid type name `{}`: {}", type_str, e),
        )
    })?;
    let (major, minor, patch) = parse_semver(&version_str).map_err(|e| {
        syn::Error::new(
            args.version.span(),
            format!("invalid version `{}`: {}", version_str, e),
        )
    })?;

    let _ = SemVer::new(major, minor, patch);

    Ok(quote! {
        ::streamlib::sdk::descriptors::SchemaIdent::new(
            ::streamlib::sdk::descriptors::Org::new(#org_str).expect("validated by macro"),
            ::streamlib::sdk::descriptors::Package::new(#package_str).expect("validated by macro"),
            ::streamlib::sdk::descriptors::TypeName::new(#type_str).expect("validated by macro"),
            ::streamlib::sdk::descriptors::SemVer::new(#major, #minor, #patch),
        )
    })
}

// =============================================================================
// module_ident! / module_ident_any_version! / module_ident_joined! /
// module_ident_joined_any_version! — imperative-API ModuleIdent builders.
// =============================================================================
//
// Four macros, one identifier shape:
//
// - `module_ident!("org", "name", "^1.0.0")` — split args, with version.
// - `module_ident_any_version!("org", "name")` — split args, any version (`*`).
// - `module_ident_joined!("@org/name", "^1.0.0")` — joined org/name, with version.
// - `module_ident_joined_any_version!("@org/name")` — joined org/name, any version.
//
// Each macro validates inputs at expansion time (invalid org / name /
// semver range becomes a `compile_error!`) and expands to a
// `streamlib::sdk::descriptors::ModuleIdent::new(...)` expression.

/// `module_ident!("org", "name", "^1.0.0")` — split args, version required.
///
/// Validates org / name / semver range at compile time; expands to a
/// `ModuleIdent::new(...)` expression. Use [`module_ident_any_version!`]
/// when the version isn't pinned, [`module_ident_joined!`] when the
/// `@org/name` is already a single string.
#[proc_macro]
pub fn module_ident(input: TokenStream) -> TokenStream {
    let args = parse_macro_input!(input as ModuleIdentArgs);
    match expand_module_ident_split(&args.org, &args.name, Some(&args.version)) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.to_compile_error().into(),
    }
}

/// `module_ident_any_version!("org", "name")` — split args, any version.
///
/// Equivalent to `module_ident!("org", "name", "*")`. Use when the
/// caller doesn't pin a version range — the runtime resolver picks the
/// highest installed `SemVer` matching `(@org/name)`.
#[proc_macro]
pub fn module_ident_any_version(input: TokenStream) -> TokenStream {
    let args = parse_macro_input!(input as ModuleIdentAnyArgs);
    match expand_module_ident_split(&args.org, &args.name, None) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.to_compile_error().into(),
    }
}

/// `module_ident_joined!("@org/name", "^1.0.0")` — joined org/name, version required.
///
/// The `@org/name` literal is parsed into typed [`Org`] / [`Package`]
/// segments at compile time; version is validated as a [`SemVerRange`].
/// Use [`module_ident_joined_any_version!`] when the version isn't
/// pinned, [`module_ident!`] when the org and name are already separate
/// string literals.
#[proc_macro]
pub fn module_ident_joined(input: TokenStream) -> TokenStream {
    let args = parse_macro_input!(input as ModuleIdentJoinedArgs);
    match expand_module_ident_joined(&args.joined, Some(&args.version)) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.to_compile_error().into(),
    }
}

/// `module_ident_joined_any_version!("@org/name")` — joined org/name, any version.
///
/// Equivalent to `module_ident_joined!("@org/name", "*")`.
#[proc_macro]
pub fn module_ident_joined_any_version(input: TokenStream) -> TokenStream {
    let args = parse_macro_input!(input as ModuleIdentJoinedAnyArgs);
    match expand_module_ident_joined(&args.joined, None) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.to_compile_error().into(),
    }
}

struct ModuleIdentArgs {
    org: LitStr,
    name: LitStr,
    version: LitStr,
}

impl Parse for ModuleIdentArgs {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let org: LitStr = input.parse()?;
        input.parse::<Token![,]>()?;
        let name: LitStr = input.parse()?;
        input.parse::<Token![,]>()?;
        let version: LitStr = input.parse()?;
        let _ = input.parse::<Token![,]>();
        Ok(Self { org, name, version })
    }
}

struct ModuleIdentAnyArgs {
    org: LitStr,
    name: LitStr,
}

impl Parse for ModuleIdentAnyArgs {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let org: LitStr = input.parse()?;
        input.parse::<Token![,]>()?;
        let name: LitStr = input.parse()?;
        let _ = input.parse::<Token![,]>();
        Ok(Self { org, name })
    }
}

struct ModuleIdentJoinedArgs {
    joined: LitStr,
    version: LitStr,
}

impl Parse for ModuleIdentJoinedArgs {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let joined: LitStr = input.parse()?;
        input.parse::<Token![,]>()?;
        let version: LitStr = input.parse()?;
        let _ = input.parse::<Token![,]>();
        Ok(Self { joined, version })
    }
}

struct ModuleIdentJoinedAnyArgs {
    joined: LitStr,
}

impl Parse for ModuleIdentJoinedAnyArgs {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let joined: LitStr = input.parse()?;
        let _ = input.parse::<Token![,]>();
        Ok(Self { joined })
    }
}

/// Expand the split-args variants. `version` is `None` for any-version
/// (`*`); `Some` for pinned. Each segment is validated at expansion time.
fn expand_module_ident_split(
    org: &LitStr,
    name: &LitStr,
    version: Option<&LitStr>,
) -> syn::Result<proc_macro2::TokenStream> {
    let org_str = org.value();
    let name_str = name.value();

    Org::new(&org_str).map_err(|e| {
        syn::Error::new(
            org.span(),
            format!("invalid org `{}`: {}", org_str, e),
        )
    })?;
    Package::new(&name_str).map_err(|e| {
        syn::Error::new(
            name.span(),
            format!("invalid package `{}`: {}", name_str, e),
        )
    })?;

    emit_module_ident(&org_str, &name_str, version)
}

/// Expand the joined-args variants. The `@org/name` literal is split at
/// the first `/`; `@` prefix is required. `version` is `None` for
/// any-version (`*`); `Some` for pinned.
fn expand_module_ident_joined(
    joined: &LitStr,
    version: Option<&LitStr>,
) -> syn::Result<proc_macro2::TokenStream> {
    let raw = joined.value();
    let stripped = raw.strip_prefix('@').ok_or_else(|| {
        syn::Error::new(
            joined.span(),
            format!(
                "invalid joined module ref `{}`: must start with '@' (e.g. `\"@org/name\"`)",
                raw
            ),
        )
    })?;
    let (org_str, name_str) = stripped.split_once('/').ok_or_else(|| {
        syn::Error::new(
            joined.span(),
            format!(
                "invalid joined module ref `{}`: must contain '/' between org and name \
                 (e.g. `\"@org/name\"`)",
                raw
            ),
        )
    })?;
    if name_str.contains('@') || name_str.contains('/') {
        return Err(syn::Error::new(
            joined.span(),
            format!(
                "invalid joined module ref `{}`: name segment must not contain '@' or '/' \
                 (the version goes in the second arg, e.g. `module_ident_joined!(\"@org/name\", \"^1.0.0\")`)",
                raw
            ),
        ));
    }

    Org::new(org_str).map_err(|e| {
        syn::Error::new(
            joined.span(),
            format!("invalid org `{}` in `{}`: {}", org_str, raw, e),
        )
    })?;
    Package::new(name_str).map_err(|e| {
        syn::Error::new(
            joined.span(),
            format!("invalid package `{}` in `{}`: {}", name_str, raw, e),
        )
    })?;

    emit_module_ident(org_str, name_str, version)
}

/// Validate the version range (if any) and emit the
/// `ModuleIdent::new(...)` expression. The runtime types
/// (`Org` / `Package` / `SemVerRange` / `ModuleIdent`) are re-validated
/// at runtime via the canonical `*::new` / `from_str` constructors —
/// the macro just guarantees they'll succeed.
fn emit_module_ident(
    org_str: &str,
    name_str: &str,
    version: Option<&LitStr>,
) -> syn::Result<proc_macro2::TokenStream> {
    let version_expr = match version {
        Some(lit) => {
            let v_str = lit.value();
            SemVerRange::from_str(&v_str).map_err(|e| {
                syn::Error::new(
                    lit.span(),
                    format!("invalid semver range `{}`: {}", v_str, e),
                )
            })?;
            quote! {
                ::streamlib::sdk::descriptors::SemVerRange::from_str(#v_str)
                    .expect("validated by macro")
            }
        }
        None => quote! { ::streamlib::sdk::descriptors::SemVerRange::Any },
    };

    Ok(quote! {
        ::streamlib::sdk::descriptors::ModuleIdent::new(
            ::streamlib::sdk::descriptors::Org::new(#org_str).expect("validated by macro"),
            ::streamlib::sdk::descriptors::Package::new(#name_str).expect("validated by macro"),
            #version_expr,
        )
    })
}

fn parse_semver(s: &str) -> Result<(u32, u32, u32), String> {
    let mut parts = s.split('.');
    let major = parse_part(parts.next())?;
    let minor = parse_part(parts.next())?;
    let patch = parse_part(parts.next())?;
    if parts.next().is_some() {
        return Err("expected exactly three dot-separated integers (e.g. 1.0.0)".into());
    }
    Ok((major, minor, patch))
}

fn parse_part(part: Option<&str>) -> Result<u32, String> {
    let p = part.ok_or_else(|| "expected three dot-separated integers".to_string())?;
    p.parse::<u32>()
        .map_err(|_| format!("`{}` is not a non-negative integer", p))
}

/// Derive macro for ConfigDescriptor trait.
///
/// Generates a `ConfigDescriptor` implementation for config structs,
/// enabling automatic config field metadata extraction for processor descriptors.
///
/// # Field Handling
///
/// - `Option<T>` fields are marked as `required: false`
/// - All other fields are marked as `required: true`
/// - Doc comments on fields become the `description`
///
/// # Example
///
/// ```ignore
/// use streamlib::sdk::ConfigDescriptor;
///
/// #[derive(ConfigDescriptor)]
/// pub struct CameraConfig {
///     /// Camera device identifier
///     pub device_id: Option<String>,
///     /// Target width in pixels
///     pub width: u32,
///     /// Target height in pixels
///     pub height: u32,
/// }
/// ```
#[proc_macro_derive(ConfigDescriptor)]
pub fn derive_config_descriptor(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);

    match config_descriptor::derive_config_descriptor(input) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.to_compile_error().into(),
    }
}
