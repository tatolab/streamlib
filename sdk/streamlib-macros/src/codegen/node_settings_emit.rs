// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! What `#[node(settings = …)]` adds to a node: its settings on the `Node` it builds, taken from
//! the values its inputs hold until it is set up, and each of them declared as one of its inputs.

use proc_macro2::TokenStream;
use quote::quote;
use syn::Path;

/// The field holding whether the node was set up, after which a value set on one of its settings
/// rebuilds it rather than reaching it.
const SET_UP_FIELD: &str = "__streamlib_settings_were_read_at_setup";

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

    fn set_up_field() -> syn::Ident {
        syn::Ident::new(SET_UP_FIELD, proc_macro2::Span::call_site())
    }

    /// The generated module's alias of the settings type and its `node` constructor, or, for a
    /// node declaring none, its alias of the config type and the constructor taking one.
    pub(super) fn module_alias_and_constructor(
        &self,
        member_visibility: &TokenStream,
        config_type: &TokenStream,
    ) -> TokenStream {
        match self.settings_type {
            Some(settings_type) => quote! {
                #member_visibility type Settings = #settings_type;

                #member_visibility fn node(settings: Settings) -> __streamlib_sdk::nodes::NodeSpec {
                    Node::node(settings)
                }
            },
            None => quote! {
                #member_visibility type Config = #config_type;

                #member_visibility fn node(config: Config) -> __streamlib_sdk::nodes::NodeSpec {
                    Node::node(config)
                }
            },
        }
    }

    /// `Node::node`: a spec of the node holding `settings` as the values of its setting inputs,
    /// or, for a node declaring none, holding `config`.
    pub(super) fn node_spec_constructor(
        &self,
        member_visibility: &TokenStream,
        config_type: &TokenStream,
    ) -> TokenStream {
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
                    .with_pins_and_studio_fields(
                        input_values,
                        __streamlib_sdk::serde_json::Map::new(),
                    )
                }
            },
            None => quote! {
                #member_visibility fn node(config: #config_type) -> __streamlib_sdk::nodes::NodeSpec {
                    __streamlib_sdk::nodes::NodeSpec::new(
                        Self::node_class_import_path(),
                        __streamlib_sdk::serde_json::to_value(&config)
                            .expect("Config serialization failed"),
                    )
                }
            },
        }
    }

    /// The `Node` fields holding the settings and whether the node was set up.
    pub(super) fn struct_fields(&self, member_visibility: &TokenStream) -> TokenStream {
        let Some(settings_type) = self.settings_type else {
            return quote! {};
        };
        let set_up_field = Self::set_up_field();
        quote! {
            #member_visibility settings: #settings_type,
            #set_up_field: bool,
        }
    }

    /// The initializers of [`Self::struct_fields`]: the settings' defaults, not set up yet.
    pub(super) fn struct_field_initializers(&self) -> TokenStream {
        if !self.declares_settings() {
            return quote! {};
        }
        let set_up_field = Self::set_up_field();
        quote! {
            settings: ::std::default::Default::default(),
            #set_up_field: false,
        }
    }

    /// The `GeneratedNode` method taking the settings the input values hold, until the node is
    /// set up.
    pub(super) fn settings_taken_from_the_input_values(&self) -> TokenStream {
        let Some(settings_type) = self.settings_type else {
            return quote! {};
        };
        let set_up_field = Self::set_up_field();
        quote! {
            fn take_the_settings_the_input_values_hold(
                &mut self,
                input_values: &__streamlib_sdk::serde_json::Map<
                    ::std::string::String,
                    __streamlib_sdk::serde_json::Value,
                >,
            ) -> __streamlib_sdk::error::Result<()> {
                if self.#set_up_field {
                    return Ok(());
                }
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

    /// The statement `__generated_setup` opens with: the node is set up, so its settings stay
    /// those it was set up with.
    pub(super) fn setup_marked(&self) -> TokenStream {
        if !self.declares_settings() {
            return quote! {};
        }
        let set_up_field = Self::set_up_field();
        quote! { self.#set_up_field = true; }
    }

    /// The descriptor's config schema; none for a node declaring settings, which takes no config.
    pub(super) fn descriptor_config_schema(&self, config_type: &TokenStream) -> TokenStream {
        if self.declares_settings() {
            return quote! {};
        }
        // Reached through the bound-carrying trait rather than `schemars` directly, so a config
        // type missing the derive fails on a diagnostic that names the fix.
        quote! {
            .with_config_schema(
                <#config_type as __streamlib_sdk::descriptors::NodeConfigJsonSchema>
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
