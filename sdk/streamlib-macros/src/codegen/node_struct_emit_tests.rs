// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use streamlib_node_schema::{NodePortSchema, NodeSchema, PortFamily, PortFamilyDeclaration};

fn minimal_schema() -> NodeSchema {
    NodeSchema {
        name: "MinimalProbe".to_string(),
        description: None,
        runtime: Default::default(),
        entrypoint: None,
        execution: Default::default(),
        scheduling: None,
        paced_by_real_time: false,
        config: None,
        state: Vec::new(),
        inputs: Vec::new(),
        outputs: Vec::new(),
    }
}

#[test]
fn node_struct_does_not_carry_audio_field() {
    let schema = minimal_schema();
    let rendered = generate_node_struct_from_schema(
        &schema,
        &NodeSettingsEmitted::of(None),
        &[],
        &quote! { pub },
    )
    .to_string();
    assert!(
        !rendered.contains("audio"),
        "generated Node struct must not declare an `audio` field — got: {}",
        rendered
    );
    assert!(
        !rendered.contains("NodeAudioConverter"),
        "generated Node struct must not reference NodeAudioConverter — got: {}",
        rendered
    );
}

#[test]
fn from_config_initializer_does_not_construct_audio_converter() {
    let schema = minimal_schema();
    let rendered =
        generate_from_config_from_schema(&schema, &NodeSettingsEmitted::of(None), &[]).to_string();
    assert!(
        !rendered.contains("NodeAudioConverter"),
        "from_config must not reference NodeAudioConverter — got: {}",
        rendered
    );
    assert!(
        !rendered.contains("audio :"),
        "from_config must not initialize an `audio` field — got: {}",
        rendered
    );
}

/// `TokenStream::to_string` spacing is not part of any contract, so
/// assertions compare against a whitespace-free rendering.
fn render_token_stream_without_whitespace(tokens: TokenStream) -> String {
    tokens
        .to_string()
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect()
}

fn attribute_path_ident(attribute: &syn::Attribute) -> String {
    let path = attribute.path();
    path.get_ident()
        .map(Ident::to_string)
        .unwrap_or_else(|| render_token_stream_without_whitespace(quote! { #path }))
}

/// The path ident of each attribute, in authored order.
fn attribute_path_idents(attributes: &[syn::Attribute]) -> Vec<String> {
    attributes.iter().map(attribute_path_ident).collect()
}

fn attributes_authored_across_probe_custom_fields(
    custom_fields: &[CustomField],
) -> Vec<&syn::Attribute> {
    custom_fields
        .iter()
        .flat_map(|custom_field| custom_field.attributes_authored_on_node_struct_field.iter())
        .collect()
}

/// The attribute path idents one emission site's filter keeps — the
/// decision the filter actually makes, read without going through a
/// rendering.
fn forwarded_attribute_path_idents(
    custom_fields: &[CustomField],
    is_forwarded_onto_emission_site: impl Fn(&syn::Attribute) -> bool,
) -> Vec<String> {
    attributes_authored_across_probe_custom_fields(custom_fields)
        .into_iter()
        .filter(|attribute| is_forwarded_onto_emission_site(attribute))
        .map(attribute_path_ident)
        .collect()
}

fn parse_generated_node_struct(tokens: TokenStream) -> ItemStruct {
    syn::parse2(tokens).expect("the struct emitter must emit a parseable `struct Node`")
}

fn declared_field_names(generated_struct: &ItemStruct) -> Vec<String> {
    match &generated_struct.fields {
        syn::Fields::Named(fields) => fields
            .named
            .iter()
            .filter_map(|field| field.ident.as_ref().map(Ident::to_string))
            .collect(),
        _ => Vec::new(),
    }
}

fn declared_field<'a>(
    generated_struct: &'a ItemStruct,
    field_name: &str,
) -> Option<&'a syn::Field> {
    match &generated_struct.fields {
        syn::Fields::Named(fields) => fields.named.iter().find(|field| {
            field
                .ident
                .as_ref()
                .is_some_and(|ident| ident == field_name)
        }),
        _ => None,
    }
}

fn type_path_last_segment(ty: &syn::Type) -> Option<String> {
    match ty {
        syn::Type::Path(type_path) => type_path
            .path
            .segments
            .last()
            .map(|segment| segment.ident.to_string()),
        _ => None,
    }
}

/// The `Self { .. }` literal the emitted `from_config` returns, so field
/// assertions read the struct expression rather than its rendering.
fn parse_generated_from_config_struct_expression(tokens: TokenStream) -> syn::ExprStruct {
    let from_config: syn::ImplItemFn =
        syn::parse2(tokens).expect("the from_config emitter must emit a parseable method");
    let tail = from_config
        .block
        .stmts
        .last()
        .expect("from_config must have a body");
    let syn::Stmt::Expr(syn::Expr::Call(ok_call), _) = tail else {
        panic!("from_config must end in an `Ok(..)` tail expression — got: {tail:?}");
    };
    match ok_call.args.first() {
        Some(syn::Expr::Struct(struct_expression)) => struct_expression.clone(),
        other => panic!("from_config must wrap a `Self {{ .. }}` literal — got: {other:?}"),
    }
}

fn initialized_field_names(struct_expression: &syn::ExprStruct) -> Vec<String> {
    struct_expression
        .fields
        .iter()
        .filter_map(|field_value| match &field_value.member {
            syn::Member::Named(ident) => Some(ident.to_string()),
            syn::Member::Unnamed(_) => None,
        })
        .collect()
}

fn initialized_field<'a>(
    struct_expression: &'a syn::ExprStruct,
    field_name: &str,
) -> Option<&'a syn::FieldValue> {
    struct_expression
        .fields
        .iter()
        .find(|field_value| match &field_value.member {
            syn::Member::Named(ident) => ident == field_name,
            syn::Member::Unnamed(_) => false,
        })
}

fn platform_conditional_field_struct() -> ItemStruct {
    syn::parse_quote! {
        struct PlatformConditionalFieldProbe {
            #[cfg(target_os = "linux")]
            linux_only_backend_state: Option<u32>,
            platform_agnostic_state: u64,
        }
    }
}

fn schema_declaring_iceoryx2_input_and_output_ports() -> NodeSchema {
    let port = |name: &str| NodePortSchema {
        name: name.to_string(),
        description: None,
        delivery_profile: None,
        borrow_quota: None,
        audio_window: None,
        family: PortFamilyDeclaration::of_family(PortFamily::Image),
        pin_type: None,
        pin_type_from_setting: None,
        lifetime: streamlib_node_schema::NodeValueLifetime::Kept,
        flow: None,
        former_names: Vec::new(),
        may_be_unset: false,
        takes_a_value: false,
    };
    NodeSchema {
        inputs: vec![port("video_in")],
        outputs: vec![port("video_out")],
        ..minimal_schema()
    }
}

fn struct_with_every_custom_field_compiled_out() -> ItemStruct {
    syn::parse_quote! {
        struct EveryCustomFieldCompiledOutProbe {
            #[cfg(any())]
            never_compiled_backend_state: Option<u32>,
            #[cfg(any())]
            never_compiled_frame_counter: u64,
        }
    }
}

#[test]
fn port_fields_stay_unconditional_when_every_custom_field_is_compiled_out() {
    let schema = schema_declaring_iceoryx2_input_and_output_ports();
    let custom_fields = extract_custom_fields(&struct_with_every_custom_field_compiled_out());

    let generated_struct = parse_generated_node_struct(generate_node_struct_from_schema(
        &schema,
        &NodeSettingsEmitted::of(None),
        &custom_fields,
        &quote! { pub },
    ));
    for (port_field_name, port_field_type) in
        [("inputs", "InputMailboxes"), ("outputs", "OutputWriter")]
    {
        let port_field = declared_field(&generated_struct, port_field_name).unwrap_or_else(|| {
            panic!(
                "the generated Node struct must declare the `{}` port \
                     field — it declares {:?}",
                port_field_name,
                declared_field_names(&generated_struct)
            )
        });
        assert!(
            port_field.attrs.is_empty(),
            "the `{}` port field must be unconditional — no authored field attribute may \
                 reach it, but it carries {:?}",
            port_field_name,
            attribute_path_idents(&port_field.attrs)
        );
        assert!(
            matches!(port_field.vis, syn::Visibility::Public(_)),
            "the `{}` port field must stay `pub` — the host patches it after `from_config` \
                 returns",
            port_field_name
        );
        assert_eq!(
            type_path_last_segment(&port_field.ty).as_deref(),
            Some(port_field_type),
            "the `{}` port field must keep its handle type",
            port_field_name
        );
    }

    let compiled_out_field = declared_field(&generated_struct, "never_compiled_backend_state")
        .unwrap_or_else(|| {
            panic!(
                "the probe's compiled-out field must still be emitted so this test exercises \
                     the attribute filter — the struct declares {:?}",
                declared_field_names(&generated_struct)
            )
        });
    assert_eq!(
        attribute_path_idents(&compiled_out_field.attrs),
        vec!["cfg".to_string()],
        "the probe must actually exercise the attribute filter — its compiled-out field has \
             to carry the authored `#[cfg]`"
    );

    let from_config_struct_expression = parse_generated_from_config_struct_expression(
        generate_from_config_from_schema(&schema, &NodeSettingsEmitted::of(None), &custom_fields),
    );
    for port_field_name in ["inputs", "outputs"] {
        let port_initializer = initialized_field(&from_config_struct_expression, port_field_name)
            .unwrap_or_else(|| {
                panic!(
                    "`from_config` must initialize the `{}` port field — \
                             it initializes {:?}",
                    port_field_name,
                    initialized_field_names(&from_config_struct_expression)
                )
            });
        assert!(
            port_initializer.attrs.is_empty(),
            "the `{}` port initializer must stay unconditional — no authored field attribute \
                 may reach it, but it carries {:?}",
            port_field_name,
            attribute_path_idents(&port_initializer.attrs)
        );
    }
}

#[test]
fn node_struct_preserves_cfg_attribute_on_custom_field() {
    let custom_fields = extract_custom_fields(&platform_conditional_field_struct());
    let rendered = render_token_stream_without_whitespace(generate_node_struct_from_schema(
        &minimal_schema(),
        &NodeSettingsEmitted::of(None),
        &custom_fields,
        &quote! { pub },
    ));
    assert!(
        rendered.contains(r#"#[cfg(target_os="linux")]publinux_only_backend_state"#),
        "generated Node struct must carry the authored `#[cfg]` on \
             `linux_only_backend_state` — got: {}",
        rendered
    );
    assert!(
        !rendered.contains(r#"#[cfg(target_os="linux")]pubplatform_agnostic_state"#),
        "the `#[cfg]` must not leak onto the unconditional field — got: {}",
        rendered
    );
}

/// Locks #1588: the `from_config` struct-literal initializer must carry the
/// same `#[cfg]` as the field it initializes — an unconditional initializer
/// for a conditional field does not compile.
#[test]
fn from_config_initializer_preserves_cfg_attribute_on_custom_field() {
    let custom_fields = extract_custom_fields(&platform_conditional_field_struct());
    let rendered = render_token_stream_without_whitespace(generate_from_config_from_schema(
        &minimal_schema(),
        &NodeSettingsEmitted::of(None),
        &custom_fields,
    ));
    assert!(
        rendered.contains(r#"#[cfg(target_os="linux")]linux_only_backend_state:"#),
        "from_config must carry the authored `#[cfg]` on the \
             `linux_only_backend_state` initializer — got: {}",
        rendered
    );
}

fn annotated_field_struct() -> ItemStruct {
    syn::parse_quote! {
        struct AnnotatedFieldProbe {
            /// Authored doc on a node field.
            #[allow(dead_code)]
            #[expect(unused_parens)]
            #[cfg_attr(target_os = "linux", allow(unused))]
            #[serde(skip)]
            annotated_backend_state: Option<u32>,
        }
    }
}

/// The field-definition site forwards `doc` and the lint controls so an
/// author's `///` and `#[allow(dead_code)]` survive expansion; `cfg_attr`
/// and unknown attributes stay dropped.
#[test]
fn node_struct_forwards_doc_and_lint_attributes_but_not_cfg_attr() {
    let custom_fields = extract_custom_fields(&annotated_field_struct());
    let rendered = render_token_stream_without_whitespace(generate_node_struct_from_schema(
        &minimal_schema(),
        &NodeSettingsEmitted::of(None),
        &custom_fields,
        &quote! { pub },
    ));
    assert!(
        rendered.contains("Authoreddoconanodefield."),
        "generated Node struct must carry the authored doc — got: {}",
        rendered
    );
    assert!(
        rendered.contains("#[allow(dead_code)]"),
        "generated Node struct must carry the authored lint control — got: {}",
        rendered
    );
    assert!(
        !rendered.contains("cfg_attr"),
        "`cfg_attr` must not be forwarded — its expansion could change \
             field presence and desync the two emission sites — got: {}",
        rendered
    );
    assert!(
        !rendered.contains("serde"),
        "unknown attributes stay dropped — got: {}",
        rendered
    );
}

/// `expect` is the one lint control that must NOT ride along. The macro
/// re-emits the authored field as `pub`, which changes which lints can fire
/// on it at all, so a forwarded `#[expect(...)]` the author wrote to silence
/// a warning lands unfulfilled and warns `unfulfilled_lint_expectation`
/// instead — the inverse of the silencing they asked for.
#[test]
fn node_struct_does_not_forward_expect_onto_the_generated_pub_field() {
    let custom_fields = extract_custom_fields(&annotated_field_struct());
    let forwarded_attribute_paths = forwarded_attribute_path_idents(
        &custom_fields,
        is_forwarded_onto_generated_field_definition,
    );
    assert!(
        !forwarded_attribute_paths.contains(&"expect".to_string()),
        "`expect` must not reach the generated `pub` field — it would land \
             unfulfilled and warn; forwarded attributes were: {:?}",
        forwarded_attribute_paths
    );
    assert!(
        forwarded_attribute_paths.contains(&"allow".to_string()),
        "the probe must still exercise a forwarded lint control, or this \
             test passes vacuously; forwarded attributes were: {:?}",
        forwarded_attribute_paths
    );
}

/// Every `compile_error!` message in a generated token stream, so a
/// diagnostic test asserts on the text the author will actually read.
fn compile_error_messages(tokens: TokenStream) -> Vec<String> {
    use proc_macro2::TokenTree;

    let mut messages = Vec::new();
    let mut previous_ident_was_compile_error = false;
    for tree in tokens {
        match tree {
            TokenTree::Ident(ident) => {
                previous_ident_was_compile_error = ident == "compile_error";
            }
            TokenTree::Group(group) => {
                if previous_ident_was_compile_error
                    && let Ok(message) = syn::parse2::<syn::LitStr>(group.stream())
                {
                    messages.push(message.value());
                }
                previous_ident_was_compile_error = false;
                messages.extend(compile_error_messages(group.stream()));
            }
            TokenTree::Punct(_) => {}
            TokenTree::Literal(_) => previous_ident_was_compile_error = false,
        }
    }
    messages
}

fn expand_probe_node(item: &ItemStruct) -> TokenStream {
    let parsed = crate::grammar::parse2(
        quote! { execution = manual },
        crate::grammar::DeclaredNodeMeta::default(),
        &item.ident,
    )
    .expect("a minimal attribute parses");
    generate_from_parsed_node_attr(item, &parsed, quote! { streamlib })
}

fn struct_with_cfg_attr_on_a_field() -> ItemStruct {
    syn::parse_quote! {
        struct CfgAttrFieldProbe {
            #[cfg_attr(target_os = "linux", allow(unused))]
            cfg_attr_annotated_backend_state: Option<u32>,
            unannotated_frame_counter: u64,
        }
    }
}

fn struct_with_dropped_derive_helper_attributes_on_a_field() -> ItemStruct {
    syn::parse_quote! {
        struct DeriveHelperAnnotatedFieldProbe {
            #[allow(dead_code)]
            #[serde(skip)]
            #[schemars(skip)]
            derive_helper_annotated_backend_state: Option<u32>,
        }
    }
}

#[test]
fn cfg_attr_on_a_node_field_emits_a_compile_error_naming_the_field() {
    let messages = compile_error_messages(expand_probe_node(&struct_with_cfg_attr_on_a_field()));
    assert_eq!(
        messages.len(),
        1,
        "exactly one field carries `cfg_attr`, so exactly one diagnostic is \
             expected — got: {:?}",
        messages
    );
    assert!(
        messages[0].contains("cfg_attr_annotated_backend_state"),
        "the diagnostic must name the offending field so the author can find \
             it — got: {}",
        messages[0]
    );
    assert!(
        messages[0].contains("cfg_attr"),
        "the diagnostic must name the unsupported attribute — got: {}",
        messages[0]
    );
}

#[test]
fn a_dropped_derive_helper_attribute_on_a_node_field_stays_silent() {
    let messages = compile_error_messages(expand_probe_node(
        &struct_with_dropped_derive_helper_attributes_on_a_field(),
    ));
    assert!(
        messages.is_empty(),
        "only `cfg_attr` is refused; a dropped derive-helper attribute must \
             not raise a diagnostic — got: {:?}",
        messages
    );
}

fn struct_with_a_cfg_alongside_every_other_forwardable_attribute() -> ItemStruct {
    syn::parse_quote! {
        struct CfgAlongsideEveryOtherAttributeProbe {
            #[allow(dead_code)]
            #[expect(unused_parens)]
            #[cfg(target_os = "linux")]
            #[cfg_attr(target_os = "linux", allow(unused))]
            #[serde(skip)]
            cfg_and_lint_annotated_backend_state: Option<u32>,
        }
    }
}

/// The `from_config` struct-literal site takes `cfg` and nothing else — a
/// `doc` on a struct-expression field is an `unused_doc_comments` warning
/// and the gates deny warnings.
#[test]
fn from_config_initializer_forwards_cfg_only() {
    let custom_fields =
        extract_custom_fields(&struct_with_a_cfg_alongside_every_other_forwardable_attribute());
    let from_config_struct_expression =
        parse_generated_from_config_struct_expression(generate_from_config_from_schema(
            &minimal_schema(),
            &NodeSettingsEmitted::of(None),
            &custom_fields,
        ));

    let initializer = initialized_field(
        &from_config_struct_expression,
        "cfg_and_lint_annotated_backend_state",
    )
    .unwrap_or_else(|| {
        panic!(
            "`from_config` must still initialize the authored field — it initializes {:?}",
            initialized_field_names(&from_config_struct_expression)
        )
    });
    assert_eq!(
        attribute_path_idents(&initializer.attrs),
        vec!["cfg".to_string()],
        "the `from_config` initializer must carry the authored `#[cfg]` and nothing else — \
             the probe also authors `doc`, `allow`, `expect`, `cfg_attr`, and a derive helper"
    );
}

/// The initializer's accepted set must stay a subset of the field
/// definition's: a presence-changing attribute on the initializer for a
/// field the definition did not gate the same way initializes a field that
/// isn't there. [`is_forwarded_onto_generated_field_definition`] holds the
/// nesting by construction, so this is the regression pin that catches an
/// edit splitting the two sites back into independent lists.
#[test]
fn every_attribute_the_from_config_initializer_takes_also_reaches_the_field_definition() {
    let custom_fields =
        extract_custom_fields(&struct_with_a_cfg_alongside_every_other_forwardable_attribute());
    let authored = attributes_authored_across_probe_custom_fields(&custom_fields);
    assert!(
        authored
            .iter()
            .any(|attribute| is_forwarded_onto_from_config_initializer(attribute)),
        "the probe must author at least one initializer-forwarded attribute, or this \
             test passes vacuously — it authors: {:?}",
        authored
            .iter()
            .copied()
            .map(attribute_path_ident)
            .collect::<Vec<String>>()
    );
    for attribute in authored {
        if is_forwarded_onto_from_config_initializer(attribute) {
            assert!(
                is_forwarded_onto_generated_field_definition(attribute),
                "`{}` reaches the `from_config` initializer but not the generated \
                     field definition — the initializer would gate a field the definition \
                     does not declare the same way",
                attribute_path_ident(attribute)
            );
        }
    }
}

fn no_declared_meta_or_port_descriptions() -> NodeDeclaredMetaAndPortDescriptions<'static> {
    NodeDeclaredMetaAndPortDescriptions {
        node_description: None,
        category: None,
        conversion: false,
        inputs: Vec::new(),
        outputs: Vec::new(),
    }
}

fn rendered_descriptor() -> String {
    render_token_stream_without_whitespace(generate_descriptor_from_schema(
        &minimal_schema(),
        &no_declared_meta_or_port_descriptions(),
        &NodeSettingsEmitted::of(None),
    ))
}

#[test]
fn only_a_node_paced_by_real_time_declares_it_on_its_descriptor() {
    let rendered_for = |paced_by_real_time: bool| {
        render_token_stream_without_whitespace(generate_descriptor_from_schema(
            &NodeSchema {
                paced_by_real_time,
                ..minimal_schema()
            },
            &no_declared_meta_or_port_descriptions(),
            &NodeSettingsEmitted::of(None),
        ))
    };
    assert!(rendered_for(true).contains(".paced_by_real_time()"));
    assert!(!rendered_for(false).contains("paced_by_real_time"));
}

#[test]
fn only_a_node_that_declares_a_category_sets_one_on_its_descriptor() {
    let rendered_for = |category: Option<&'static str>| {
        render_token_stream_without_whitespace(generate_descriptor_from_schema(
            &minimal_schema(),
            &NodeDeclaredMetaAndPortDescriptions {
                category,
                ..no_declared_meta_or_port_descriptions()
            },
            &NodeSettingsEmitted::of(None),
        ))
    };
    assert!(rendered_for(Some("Effects/Stylize")).contains(".with_category(\"Effects/Stylize\")"));
    assert!(!rendered_for(None).contains("with_category"));
}

#[test]
fn only_a_node_that_states_a_conversion_says_so_on_its_descriptor() {
    let rendered_for = |conversion: bool| {
        render_token_stream_without_whitespace(generate_descriptor_from_schema(
            &minimal_schema(),
            &NodeDeclaredMetaAndPortDescriptions {
                conversion,
                ..no_declared_meta_or_port_descriptions()
            },
            &NodeSettingsEmitted::of(None),
        ))
    };
    assert!(rendered_for(true).contains(".stating_a_conversion()"));
    assert!(!rendered_for(false).contains("stating_a_conversion"));
}

fn rendered_node_impl() -> String {
    rendered_impl_block_declared_as(false)
}

fn rendered_impl_block_declared_as(built_in_node: bool) -> String {
    let schema = minimal_schema();
    render_token_stream_without_whitespace(generate_node_impl_from_schema(
        &schema,
        &quote! { pub },
        &no_declared_meta_or_port_descriptions(),
        &node_type_minted(built_in_node, &schema.name),
        &NodeSettingsEmitted::of(None),
        &[],
    ))
}

#[test]
fn the_descriptor_captures_its_identity_with_module_path() {
    let rendered = rendered_node_impl();
    assert!(
        rendered.contains("module_path!()"),
        "identity must be captured at the expansion site — got: {rendered}"
    );
    // Captured once. The descriptor names the accessor rather than
    // re-expanding `module_path!()`, so the two can never disagree.
    assert_eq!(
        rendered.matches("module_path!()").count(),
        1,
        "identity must be captured in exactly one place — got: {rendered}"
    );
    assert!(
        rendered_descriptor().contains("Node::node_class_import_path()"),
        "the descriptor must take its identity from that one capture"
    );
}

/// A built-in's type is its class in `tatolab.stream`, written whole at
/// expansion, so moving its Rust module never changes it.
#[test]
fn a_built_in_nodes_identity_is_its_class_in_the_stream_package() {
    let rendered = rendered_impl_block_declared_as(true);
    assert!(
        rendered.contains("NodeClassImportPath::of_built_in_node(\"MinimalProbe\""),
        "a built-in's type is its struct's name in `tatolab.stream` — got: {rendered}"
    );
    assert!(
        !rendered.contains("module_path!()"),
        "a built-in's type never reads its module path — got: {rendered}"
    );
}

#[test]
fn a_node_declaring_no_config_still_carries_the_empty_configs_schema() {
    let rendered = rendered_descriptor();
    assert!(
        rendered.contains(
            "with_config_schema(<__streamlib_sdk::nodes::EmptyConfigas\
                 __streamlib_sdk::descriptors::NodeConfigJsonSchema>::\
                 node_config_schema_document())"
        ),
        "a no-config node must still carry a schema — got: {rendered}"
    );
}

#[test]
fn the_descriptor_never_reaches_for_type_name() {
    for rendered in [rendered_descriptor(), rendered_node_impl()] {
        assert!(
            !rendered.contains("type_name"),
            "identity must never be reflected at runtime — got: {rendered}"
        );
    }
}

/// The marker keeps the author's spelling, so `OutputLink::videoOut` still
/// names it, while its `PORT_NAME` is the cast the descriptor registers.
#[test]
fn a_port_marker_is_named_by_its_spelling_and_carries_its_cast() {
    let declared_output_port = ParsedPort {
        declared_marker_ident: Ident::new("videoOut", proc_macro2::Span::call_site()),
        cast_name: "videoout".to_string(),
        description: None,
        delivery_profile: None,
        borrow_quota: None,
        audio_window: None,
        family: PortFamilyDeclaration::of_family(PortFamily::Image),
        pin_type: None,
        pin_type_from_setting: None,
        lifetime: streamlib_node_schema::NodeValueLifetime::Kept,
        flow: None,
        former_names: Vec::new(),
        may_be_unset: false,
        takes_a_value: false,
    };
    let rendered = generate_port_marker_module(
        PortDirection::Output,
        &[declared_output_port],
        &quote! { pub },
        &quote! { pub },
    )
    .to_string();
    assert!(rendered.contains("pub mod OutputLink"), "got: {rendered}");
    assert!(rendered.contains("pub struct videoOut"), "got: {rendered}");
    assert!(
        rendered.contains("OutputPortMarker for videoOut"),
        "got: {rendered}"
    );
    assert!(
        rendered.contains(r#"const PORT_NAME : & 'static str = "videoout""#),
        "got: {rendered}"
    );
}

#[test]
fn the_authors_visibility_is_written_as_seen_from_each_nested_module() {
    use std::num::NonZeroUsize;
    let written_from = |visibility: syn::Visibility, depth: usize| {
        render_token_stream_without_whitespace(author_visibility_written_from_a_nested_module(
            &visibility,
            NonZeroUsize::new(depth).expect("every case names a nested module"),
        ))
    };
    for (visibility, depth, expected) in [
        (syn::parse_quote! { pub }, 1, "pub"),
        (syn::parse_quote! { pub }, 2, "pub"),
        (syn::parse_quote! { pub(crate) }, 2, "pub(crate)"),
        (syn::Visibility::Inherited, 1, "pub(insuper)"),
        (syn::Visibility::Inherited, 2, "pub(insuper::super)"),
        (syn::parse_quote! { pub(self) }, 1, "pub(insuper)"),
        (syn::parse_quote! { pub(super) }, 1, "pub(insuper::super)"),
        (
            syn::parse_quote! { pub(super) },
            2,
            "pub(insuper::super::super)",
        ),
        (syn::parse_quote! { pub(crate) }, 1, "pub(crate)"),
        (
            syn::parse_quote! { pub(in super::super) },
            1,
            "pub(insuper::super::super)",
        ),
        (
            syn::parse_quote! { pub(in super::nodes) },
            2,
            "pub(insuper::super::super::nodes)",
        ),
        (
            syn::parse_quote! { pub(in super::nodes) },
            1,
            "pub(insuper::super::nodes)",
        ),
        (
            syn::parse_quote! { pub(in crate::nodes) },
            2,
            "pub(incrate::nodes)",
        ),
    ] {
        assert_eq!(written_from(visibility, depth), expected);
    }
}

#[test]
fn a_crate_visible_node_generates_crate_visible_items_only() {
    let item: ItemStruct = syn::parse_quote! {
        pub(crate) struct CrateVisibleProbe;
    };
    let rendered = render_token_stream_without_whitespace(expand_probe_node(&item));
    for crate_visible_item in [
        "pub(crate)modCrateVisibleProbe",
        "pub(crate)typeConfig",
        "pub(crate)fnnode(",
        "pub(crate)structNode",
        "pub(crate)fnnode_class_import_path(",
    ] {
        assert!(
            rendered.contains(crate_visible_item),
            "expected `{crate_visible_item}` in: {rendered}"
        );
    }
    assert!(
        !rendered.contains("pubmod")
            && !rendered.contains("pubstruct")
            && !rendered.contains("pubtype"),
        "a pub(crate) node must generate no public item — got: {rendered}"
    );
}

/// A node declaring `settings = …` holds its settings on its `Node`, taken from the input values
/// until it is set up, declares each as an input after those it declares, and takes no config;
/// `node` builds a spec holding the settings as input values.
///
/// Mental-revert: settings taken after setup reach a running node rather than rebuilding it.
#[test]
fn a_node_declaring_settings_holds_them_and_declares_each_as_an_input() {
    let settings_type: syn::Path = syn::parse_quote! { crate::BlurSettings };
    let settings = NodeSettingsEmitted::of(Some(&settings_type));

    let node_struct = render_token_stream_without_whitespace(generate_node_struct_from_schema(
        &minimal_schema(),
        &settings,
        &[],
        &quote! { pub },
    ));
    assert!(
        node_struct.contains("pubsettings:crate::BlurSettings,"),
        "{node_struct}"
    );
    assert!(
        node_struct.contains("__streamlib_settings_were_read_at_setup:bool,"),
        "{node_struct}"
    );

    let descriptor = render_token_stream_without_whitespace(generate_descriptor_from_schema(
        &minimal_schema(),
        &no_declared_meta_or_port_descriptions(),
        &settings,
    ));
    assert!(
        descriptor.contains(".with_the_inputs_of_its_settings::<crate::BlurSettings>()"),
        "{descriptor}"
    );
    assert!(!descriptor.contains("with_config_schema"), "{descriptor}");

    let node_impl = render_token_stream_without_whitespace(generate_node_impl_from_schema(
        &minimal_schema(),
        &quote! { pub },
        &no_declared_meta_or_port_descriptions(),
        &node_type_minted(false, "MinimalProbe"),
        &settings,
        &[],
    ));
    assert!(
        node_impl.contains("fntake_the_settings_the_input_values_hold("),
        "{node_impl}"
    );
    assert!(
        node_impl.contains("ifself.__streamlib_settings_were_read_at_setup{returnOk(());}"),
        "{node_impl}"
    );
    assert!(
        node_impl.contains("self.__streamlib_settings_were_read_at_setup=true;"),
        "{node_impl}"
    );
    assert!(
        node_impl.contains("pubfnnode(settings:crate::BlurSettings)"),
        "{node_impl}"
    );
    assert!(
        node_impl.contains(".with_input_values_and_studio_fields("),
        "{node_impl}"
    );
}
