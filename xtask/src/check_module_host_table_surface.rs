// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

//! CI gate keeping the module host table data-plane only.
//!
//! Every `#[repr(C)]` field and every type alias in
//! `sdk/tatolab-module-sdk/src/module_host_table_abi.rs` is built from plain
//! data alone: integers, `c_void`, raw pointers and arrays of those, the
//! table's own `ModuleHostTable*` types, and `extern "C"` function pointers
//! over them. An engine or RHI type — a device, a texture, a kernel, anything
//! from a `streamlib` crate — is refused naming the field. The SDK's own
//! `Cargo.toml` names no engine crate.

use anyhow::{Context, Result};
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

const MODULE_HOST_TABLE_ABI_SOURCE: &str = "sdk/tatolab-module-sdk/src/module_host_table_abi.rs";
const MODULE_SDK_MANIFEST: &str = "sdk/tatolab-module-sdk/Cargo.toml";

/// The scalar types a table field may be built from.
const PLAIN_DATA_TYPE_NAMES: &[&str] = &[
    "u8", "u16", "u32", "u64", "usize", "i8", "i16", "i32", "i64", "isize", "f32", "f64", "c_void",
];

/// The prefix every type the table defines for itself carries.
const MODULE_HOST_TABLE_OWN_TYPE_PREFIX: &str = "ModuleHostTable";

/// Crate-name fragments the SDK may never depend on.
const ENGINE_CRATE_NAME_FRAGMENTS: &[&str] = &["streamlib", "vulkanalia", "ash", "metal"];

pub fn run(workspace_root: &Path) -> Result<()> {
    let abi_source_path = workspace_root.join(MODULE_HOST_TABLE_ABI_SOURCE);
    let abi_source = fs::read_to_string(&abi_source_path)
        .with_context(|| format!("reading {}", abi_source_path.display()))?;
    let manifest_path = workspace_root.join(MODULE_SDK_MANIFEST);
    let manifest = fs::read_to_string(&manifest_path)
        .with_context(|| format!("reading {}", manifest_path.display()))?;
    let mut refusals = refusals_in_the_module_host_table_abi_source(&abi_source)?;
    refusals.extend(refusals_in_the_module_sdk_manifest(&manifest)?);
    if refusals.is_empty() {
        println!(
            "✓ check-module-host-table-surface: the module host table carries plain data and \
             handles only, and the module SDK depends on no engine crate."
        );
        return Ok(());
    }
    eprintln!(
        "✗ check-module-host-table-surface: {} refusal(s):",
        refusals.len()
    );
    for refusal in &refusals {
        eprintln!("  {refusal}");
    }
    eprintln!(
        "\nThe module host table crosses plain data and opaque handles only — never an engine or \
         RHI object. A module that needs GPU work does it on its own device, on memory it \
         imports; see docs/plan/ARCHITECTURE.md §Packages & extension model, the module host table."
    );
    anyhow::bail!("check-module-host-table-surface failed");
}

/// Every field or alias in the ABI source built from something other than
/// plain data, named.
pub fn refusals_in_the_module_host_table_abi_source(abi_source: &str) -> Result<Vec<String>> {
    let file = syn::parse_file(abi_source).context("the module host table ABI source parses")?;
    let own_type_names: BTreeSet<String> = file
        .items
        .iter()
        .filter_map(|item| match item {
            syn::Item::Struct(item) => Some(item.ident.to_string()),
            syn::Item::Type(item) => Some(item.ident.to_string()),
            _ => None,
        })
        .filter(|name| name.starts_with(MODULE_HOST_TABLE_OWN_TYPE_PREFIX))
        .collect();
    let mut refusals = Vec::new();
    for item in &file.items {
        match item {
            syn::Item::Struct(item) if is_repr_c(&item.attrs) => {
                for (index, field) in item.fields.iter().enumerate() {
                    let field_name = field
                        .ident
                        .as_ref()
                        .map(ToString::to_string)
                        .unwrap_or_else(|| index.to_string());
                    if let Some(refused) =
                        first_type_that_is_not_plain_data(&field.ty, &own_type_names)
                    {
                        refusals.push(format!(
                            "{}.{field_name}: `{refused}` is not plain data",
                            item.ident
                        ));
                    }
                }
            }
            syn::Item::Struct(item)
                if item
                    .ident
                    .to_string()
                    .starts_with(MODULE_HOST_TABLE_OWN_TYPE_PREFIX) =>
            {
                refusals.push(format!(
                    "{}: a table type is `#[repr(C)]`, or its layout is the compiler's to change",
                    item.ident
                ));
            }
            syn::Item::Type(item) => {
                if let Some(refused) = first_type_that_is_not_plain_data(&item.ty, &own_type_names)
                {
                    refusals.push(format!(
                        "type {}: `{refused}` is not plain data",
                        item.ident
                    ));
                }
            }
            syn::Item::Use(item) => {
                let used = quote_tokens(&item.tree);
                if ENGINE_CRATE_NAME_FRAGMENTS
                    .iter()
                    .any(|fragment| used.contains(fragment))
                {
                    refusals.push(format!("use {used}: the table names no engine crate"));
                }
            }
            _ => {}
        }
    }
    Ok(refusals)
}

/// Every engine crate the SDK's manifest depends on, named.
pub fn refusals_in_the_module_sdk_manifest(manifest: &str) -> Result<Vec<String>> {
    let manifest: toml::Value =
        toml::from_str(manifest).context("the module SDK's manifest parses")?;
    let mut dependency_tables: Vec<&toml::value::Table> = Vec::new();
    for section in ["dependencies", "build-dependencies"] {
        if let Some(table) = manifest.get(section).and_then(toml::Value::as_table) {
            dependency_tables.push(table);
        }
    }
    if let Some(targets) = manifest.get("target").and_then(toml::Value::as_table) {
        for target in targets.values() {
            for section in ["dependencies", "build-dependencies"] {
                if let Some(table) = target.get(section).and_then(toml::Value::as_table) {
                    dependency_tables.push(table);
                }
            }
        }
    }
    Ok(dependency_tables
        .into_iter()
        .flat_map(|table| table.keys())
        .filter(|crate_name| {
            ENGINE_CRATE_NAME_FRAGMENTS
                .iter()
                .any(|fragment| crate_name.contains(fragment))
        })
        .map(|crate_name| {
            format!("{MODULE_SDK_MANIFEST}: depends on `{crate_name}`, an engine crate")
        })
        .collect())
}

fn is_repr_c(attributes: &[syn::Attribute]) -> bool {
    attributes.iter().any(|attribute| {
        attribute.path().is_ident("repr") && quote_tokens(&attribute.meta).contains('C')
    })
}

fn quote_tokens(tokens: &impl quote::ToTokens) -> String {
    tokens.to_token_stream().to_string().replace(' ', "")
}

/// The first part of `ty` that is not plain data, rendered, or `None` when
/// all of it is.
fn first_type_that_is_not_plain_data(
    ty: &syn::Type,
    own_type_names: &BTreeSet<String>,
) -> Option<String> {
    match ty {
        syn::Type::Ptr(pointer) => first_type_that_is_not_plain_data(&pointer.elem, own_type_names),
        syn::Type::Array(array) => first_type_that_is_not_plain_data(&array.elem, own_type_names),
        syn::Type::Paren(parenthesized) => {
            first_type_that_is_not_plain_data(&parenthesized.elem, own_type_names)
        }
        syn::Type::Path(path) if path.qself.is_none() => {
            let segments: Vec<&syn::PathSegment> = path.path.segments.iter().collect();
            match segments.as_slice() {
                [only] if matches!(only.arguments, syn::PathArguments::None) => {
                    let name = only.ident.to_string();
                    (PLAIN_DATA_TYPE_NAMES.contains(&name.as_str())
                        || own_type_names.contains(&name))
                    .then_some(())
                    .map_or(Some(name), |()| None)
                }
                [option] if option.ident == "Option" => match &option.arguments {
                    syn::PathArguments::AngleBracketed(arguments) => match arguments.args.first() {
                        Some(syn::GenericArgument::Type(syn::Type::BareFn(function)))
                            if arguments.args.len() == 1 =>
                        {
                            first_type_in_a_function_pointer_that_is_not_plain_data(
                                function,
                                own_type_names,
                            )
                        }
                        _ => Some(quote_tokens(ty)),
                    },
                    _ => Some(quote_tokens(ty)),
                },
                _ => Some(quote_tokens(ty)),
            }
        }
        syn::Type::BareFn(function) => {
            first_type_in_a_function_pointer_that_is_not_plain_data(function, own_type_names)
        }
        _ => Some(quote_tokens(ty)),
    }
}

fn first_type_in_a_function_pointer_that_is_not_plain_data(
    function: &syn::TypeBareFn,
    own_type_names: &BTreeSet<String>,
) -> Option<String> {
    let is_extern_c = function
        .abi
        .as_ref()
        .and_then(|abi| abi.name.as_ref())
        .is_some_and(|name| name.value() == "C");
    if !is_extern_c {
        return Some(format!(
            "{} (a verb is `extern \"C\"`)",
            quote_tokens(function)
        ));
    }
    let return_type = match &function.output {
        syn::ReturnType::Default => None,
        syn::ReturnType::Type(_, return_type) => Some(return_type.as_ref()),
    };
    function
        .inputs
        .iter()
        .map(|argument| &argument.ty)
        .chain(return_type)
        .find_map(|ty| first_type_that_is_not_plain_data(ty, own_type_names))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_module_host_table_in_the_tree_passes() {
        let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        run(workspace_root).unwrap();
    }

    #[test]
    fn an_rhi_texture_in_the_table_is_refused_naming_the_field() {
        let source = r#"
            #[repr(C)]
            pub struct ModuleHostTable {
                pub abi_major: u32,
                pub rhi_texture: *mut VulkanTexture,
            }
        "#;
        assert_eq!(
            refusals_in_the_module_host_table_abi_source(source).unwrap(),
            vec!["ModuleHostTable.rhi_texture: `VulkanTexture` is not plain data".to_string()]
        );
    }

    #[test]
    fn a_verb_taking_an_engine_object_is_refused_naming_it() {
        let source = r#"
            #[repr(C)]
            pub struct ModuleHostTable {
                pub acquire_texture: unsafe extern "C" fn(
                    node_host: *mut c_void,
                    gpu: *const streamlib::GpuContext,
                ) -> i32,
            }
        "#;
        let refusals = refusals_in_the_module_host_table_abi_source(source).unwrap();
        assert_eq!(refusals.len(), 1);
        assert!(
            refusals[0].contains("streamlib::GpuContext"),
            "{refusals:?}"
        );
    }

    #[test]
    fn a_rust_abi_verb_and_a_rust_owned_type_are_refused() {
        let source = r#"
            #[repr(C)]
            pub struct ModuleHostTable {
                pub rust_verb: fn(*mut c_void),
                pub owned_name: String,
            }
        "#;
        let refusals = refusals_in_the_module_host_table_abi_source(source).unwrap();
        assert_eq!(refusals.len(), 2, "{refusals:?}");
    }

    #[test]
    fn a_table_type_without_repr_c_is_refused() {
        let source = "pub struct ModuleHostTableSomething { pub value: u64 }";
        assert_eq!(
            refusals_in_the_module_host_table_abi_source(source)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn an_engine_crate_dependency_is_refused() {
        let manifest = r#"
            [package]
            name = "tatolab-module-sdk"
            [dependencies]
            streamlib-engine = { path = "../../runtime/streamlib-engine" }
            tracing = "0.1"
        "#;
        assert_eq!(
            refusals_in_the_module_sdk_manifest(manifest).unwrap(),
            vec![format!(
                "{MODULE_SDK_MANIFEST}: depends on `streamlib-engine`, an engine crate"
            )]
        );
    }
}
