// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Procedural macros for streamlib.
//!
//! - `#[streamlib::processor("@org/package/Type", execution = …, …)]`
//!   — processor definition. The attribute is the single source of truth:
//!   identity, execution mode, and input/output ports are declared in code,
//!   read from no file at expansion. See [`streamlib_processor_extract::grammar`] for the full
//!   grammar. An identity string omitted from the attribute synthesizes an
//!   `@app/local/<StructName>` identity so a bare crate with no
//!   `streamlib.yaml` compiles.
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

mod codegen;
mod config_descriptor;

// The `#[processor(...)]` grammar lives in `streamlib-processor-extract` so the
// source-scan extractor and this macro parse it through one shared parser (a
// `proc-macro = true` crate cannot export the grammar as a library). #1411.
use streamlib_processor_extract::grammar as attribute_grammar;

use proc_macro::TokenStream;
use quote::quote;
use streamlib_processor_schema::{Org, Package, SemVer, TypeName};

// Range parsing for `module_ident!*` macros. The streamlib_idents crate
// owns the SemVerRange grammar; the macro just forwards the input through
// it at expansion time so invalid ranges surface as compile errors.
use streamlib_idents::SemVerRange;
use syn::{
    DeriveInput, ItemStruct, LitStr, Token,
    parse::{Parse, ParseStream},
    parse_macro_input,
};

/// Main processor attribute macro.
///
/// The attribute is the single source of truth for a processor's identity,
/// execution mode, and ports — see [`streamlib_processor_extract::grammar`] for the grammar. It
/// reads no file at expansion. An omitted identity string synthesizes an
/// `@app/local/<StructName>` identity so a bare crate compiles with no
/// `streamlib.yaml`.
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

    let parsed = match attribute_grammar::parse2(attr.into(), &item_struct.ident) {
        Ok(parsed) => parsed,
        Err(err) => return err.to_compile_error().into(),
    };

    let schema = parsed.to_processor_schema();
    let schema_ident = parsed.ident.clone();
    let config_field_name = parsed
        .config_type
        .as_ref()
        .map(|_| parsed.config_field_name.clone());

    let generated = codegen::generate_from_processor_schema(
        &item_struct,
        &schema,
        &schema_ident,
        parsed.config_type.as_ref(),
        config_field_name.as_deref(),
        parsed.config_schema_id.as_deref(),
        sdk_root(),
    );

    TokenStream::from(generated)
}

/// Resolve the path to the `sdk` module the emitted code authors against.
///
/// Plugin packages depend on `streamlib-plugin-sdk` (the engine-free SDK) by
/// its real name; hosts depend on the `streamlib` facade. Detected per
/// invocation from the consumer's `Cargo.toml` (the `serde_derive` pattern),
/// so emitted paths use the consumer's real crate name with no `streamlib`
/// aliasing. Falls back to `::streamlib::sdk` for in-engine macro use, which
/// resolves via the engine's `extern crate self as streamlib`.
fn sdk_root() -> proc_macro2::TokenStream {
    use proc_macro_crate::{FoundCrate, crate_name};
    fn as_sdk_path(found: FoundCrate) -> proc_macro2::TokenStream {
        match found {
            FoundCrate::Itself => quote! { crate::sdk },
            FoundCrate::Name(name) => {
                let ident = proc_macro2::Ident::new(&name, proc_macro2::Span::call_site());
                quote! { ::#ident::sdk }
            }
        }
    }
    // Prefer the engine-free plugin SDK — packages depend on it by real name.
    if let Ok(found) = crate_name("streamlib-plugin-sdk") {
        return as_sdk_path(found);
    }
    // Host consumers depend on the `streamlib` facade.
    if let Ok(found) = crate_name("streamlib") {
        return as_sdk_path(found);
    }
    // In-engine macro use: `extern crate self as streamlib` resolves this.
    quote! { ::streamlib::sdk }
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
        syn::Error::new(args.org.span(), format!("invalid org `{}`: {}", org_str, e))
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

/// `processor_type_ref!("org", "package", "Type")` — a **version-free**
/// processor-type reference for the lazy-discovery world (app code that never
/// calls `add_module`).
///
/// Expands to a `ProcessorTypeReference::ResolveToInstalled` value with no
/// version and **no registry lookup at the call site**, so the reference
/// reaches `add_processor`'s lazy hook and resolves to the single installed
/// provider — loading its package from `streamlib_modules/` on first
/// reference. This is the canonical form for referencing a processor by
/// `@org/package/Type` with no version.
///
/// Distinct from [`schema_ident_any_version!`], which resolves a `SchemaIdent`
/// *now* against the already-registered processor types (the post-`add_module`
/// / power-caller form). Reach for `processor_type_ref!` when you want lazy
/// loading; reach for `schema_ident_any_version!` when the provider is already
/// registered.
///
/// ```ignore
/// // No version at the reference site, no `?`, no add_module call:
/// runtime.add_processor(streamlib::sdk::processors::ProcessorSpec::new(
///     streamlib::sdk::processor_type_ref!("tatolab", "camera", "Camera"),
///     serde_json::json!({}),
/// ))?;
/// ```
#[proc_macro]
pub fn processor_type_ref(input: TokenStream) -> TokenStream {
    let args = parse_macro_input!(input as SchemaIdentAnyVersionArgs);
    match expand_processor_type_ref(&args) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.to_compile_error().into(),
    }
}

fn expand_processor_type_ref(
    args: &SchemaIdentAnyVersionArgs,
) -> syn::Result<proc_macro2::TokenStream> {
    let org_str = args.org.value();
    let package_str = args.package.value();
    let type_str = args.type_name.value();

    Org::new(&org_str).map_err(|e| {
        syn::Error::new(args.org.span(), format!("invalid org `{}`: {}", org_str, e))
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
        ::streamlib::sdk::processors::ProcessorTypeReference::ResolveToInstalled {
            org: ::streamlib::sdk::descriptors::Org::new(#org_str).expect("validated by macro"),
            package: ::streamlib::sdk::descriptors::Package::new(#package_str).expect("validated by macro"),
            r#type: ::streamlib::sdk::descriptors::TypeName::new(#type_str).expect("validated by macro"),
        }
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
        syn::Error::new(args.org.span(), format!("invalid org `{}`: {}", org_str, e))
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

    Org::new(&org_str)
        .map_err(|e| syn::Error::new(org.span(), format!("invalid org `{}`: {}", org_str, e)))?;
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

/// Parse a `schema_ident!` version string. Schema-ident versions are
/// release-only by invariant — a `-dev.N` / `-rc.N` prerelease is
/// rejected here (the package-dependency axis accepts prereleases via
/// `SemVerRange::from_str`, not this parser).
fn parse_semver(s: &str) -> Result<(u32, u32, u32), String> {
    if s.contains('-') {
        return Err(format!(
            "schema-ident version `{s}` must be a release `MAJOR.MINOR.PATCH`; \
             prerelease (`-dev.N` / `-rc.N`) versions are not valid for schema idents"
        ));
    }
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
