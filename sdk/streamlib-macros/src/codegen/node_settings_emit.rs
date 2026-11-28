// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! What `#[node(settings = …)]` adds to a node: its settings on the `Node` it builds, taken from
//! the values its inputs hold each time it is handed them, and each of them declared as one of
//! its inputs.

use proc_macro2::TokenStream;
use quote::quote;
use syn::Path;

/// What a node declaring `settings = …` is emitted with; empty for a node that declares none.
pub(super) struct NodeSettingsEmitted<'parsed> {
    settings_type: Option<&'parsed Path>,
}

impl<'parsed> NodeSettingsEmitted<'parsed> {
    pub(super) fn of(settings_type: Option<&'parsed Path>) -> Self {
        Self { settings_type }
    }

    pub(super) fn declares_settings(&self) -> bool {
        self.settings_type.is_some()
    }

    /// The generated module's alias of the settings type and its `node` constructor, or, for a
    /// node declaring none, its alias of the config type and the constructor taking one.
    pub(super) fn module_alias_and_constructor(
        &self,
        member_visibility: &TokenStream,
    ) -> TokenStream {
        match self.settings_type {
            Some(settings_type) => quote! {
                #member_visibility type Settings = #settings_type;

                #member_visibility fn node(settings: Settings) -> __streamlib_sdk::nodes::NodeSpec {
                    Node::node(settings)
                }
            },
            None => quote! {
                #member_visibility type Config = __streamlib_sdk::nodes::EmptyConfig;

                #member_visibility fn node(config: Config) -> __streamlib_sdk::nodes::NodeSpec {
                    Node::node(config)
                }
            },
        }
    }

    /// `Node::node`: a spec of the node holding `settings` as the values of its setting inputs,
    /// or, for a node declaring none, holding `config`.
    pub(super) fn node_spec_constructor(&self, member_visibility: &TokenStream) -> TokenStream {
        match self.settings_type {
            Some(settings_type) => quote! {
                #member_visibility fn node(settings: #settings_type) -> __streamlib_sdk::nodes::NodeSpec {
                    let __streamlib_sdk::serde_json::Value::Object(input_values) =
                        __streamlib_sdk::serde_json::to_value(&settings)
                            .expect("a node's settings serialize, as every field a setting holds does")
                    else {
                        unreachable!("#[node_settings] declares a struct of named fields");
                    };
                    __streamlib_sdk::nodes::NodeSpec::new(
                        Self::node_class_import_path(),
                        __streamlib_sdk::serde_json::Value::Object(
                            __streamlib_sdk::serde_json::Map::new(),
                        ),
                    )
                    .with_input_values_and_studio_fields(
                        input_values,
                        __streamlib_sdk::serde_json::Map::new(),
                    )
                }
            },
            None => quote! {
                #member_visibility fn node(config: __streamlib_sdk::nodes::EmptyConfig) -> __streamlib_sdk::nodes::NodeSpec {
                    __streamlib_sdk::nodes::NodeSpec::new(
                        Self::node_class_import_path(),
                        __streamlib_sdk::serde_json::to_value(&config)
                            .expect("Config serialization failed"),
                    )
                }
            },
        }
    }

    /// The `Node` field holding the settings.
    pub(super) fn struct_fields(&self, member_visibility: &TokenStream) -> TokenStream {
        let Some(settings_type) = self.settings_type else {
            return quote! {};
        };
        quote! {
            #member_visibility settings: #settings_type,
        }
    }

    /// The initializer of [`Self::struct_fields`]: the settings' defaults.
    pub(super) fn struct_field_initializers(&self) -> TokenStream {
        if !self.declares_settings() {
            return quote! {};
        }
        quote! {
            settings: ::std::default::Default::default(),
        }
    }

    /// The `GeneratedNode` method taking the settings the input values hold, each time the node
    /// is handed them, before its setup or while it runs.
    pub(super) fn settings_taken_from_the_input_values(&self) -> TokenStream {
        let Some(settings_type) = self.settings_type else {
            return quote! {};
        };
        quote! {
            fn take_the_settings_the_input_values_hold(
                &mut self,
                input_values: &__streamlib_sdk::serde_json::Map<
                    ::std::string::String,
                    __streamlib_sdk::serde_json::Value,
                >,
            ) -> __streamlib_sdk::error::Result<()> {
                self.settings =
                    <#settings_type as __streamlib_sdk::descriptors::NodeSettings>
                        ::read_from_the_input_values(input_values)
                        .map_err(|refusal| {
                            __streamlib_sdk::error::Error::Configuration(format!(
                                "{}: {refusal}",
                                Self::NAME
                            ))
                        })?;
                Ok(())
            }
        }
    }

    /// The descriptor's config schema; none for a node declaring settings, which takes no config.
    pub(super) fn descriptor_config_schema(&self) -> TokenStream {
        if self.declares_settings() {
            return quote! {};
        }
        // Reached through the bound-carrying trait rather than `schemars` directly, so a config
        // type missing the derive fails on a diagnostic that names the fix.
        quote! {
            .with_config_schema(
                <__streamlib_sdk::nodes::EmptyConfig as __streamlib_sdk::descriptors::NodeConfigJsonSchema>
                    ::node_config_schema_document()
            )
        }
    }

    /// Each of the node's settings, declared as one of its inputs after those it declares.
    pub(super) fn descriptor_setting_inputs(&self) -> TokenStream {
        let Some(settings_type) = self.settings_type else {
            return quote! {};
        };
        quote! { .with_the_inputs_of_its_settings::<#settings_type>() }
    }
}
