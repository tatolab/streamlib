// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

fn expanded(attribute: TokenStream, item: TokenStream) -> syn::Result<String> {
    expand_node_settings(
        attribute,
        syn::parse2(item).expect("a struct"),
        quote! { ::streamlib::sdk },
    )
    .map(|tokens| tokens.to_string())
}

fn refusal(attribute: TokenStream, item: TokenStream) -> String {
    expanded(attribute, item).expect_err("refused").to_string()
}

#[test]
fn each_field_is_declared_as_a_setting_input_in_the_order_written() {
    let rendered = expanded(
        TokenStream::new(),
        quote! {
            #[derive(Default, Serialize, Deserialize)]
            struct BlurSettings {
                #[description = "How many pixels the kernel spans."]
                kernel_size: u32,
                #[description = crate::QUALITY_DESCRIPTION]
                #[options("fast", "fine")]
                quality: String,
            }
        },
    )
    .expect("expands");
    let kernel_size = rendered
        .find("\"kernel_size\" , \"How many pixels the kernel spans.\"")
        .expect("kernel_size is declared");
    let quality = rendered
        .find("\"quality\" , crate :: QUALITY_DESCRIPTION")
        .expect("quality is declared");
    assert!(kernel_size < quality, "{rendered}");
    assert!(
        rendered.contains(
            "< u32 as :: streamlib :: sdk :: descriptors :: SettingPinType > :: setting_pin_type ()"
        ),
        "{rendered}"
    );
    assert!(
        rendered.contains("the_menu_of_a_setting :: < String > (& [\"fast\" , \"fine\"])"),
        "{rendered}"
    );
    assert!(
        rendered
            .contains("impl :: streamlib :: sdk :: descriptors :: NodeSettings for BlurSettings"),
        "{rendered}"
    );
    assert!(!rendered.contains("# [description"), "{rendered}");
    assert!(!rendered.contains("# [options"), "{rendered}");
    assert!(
        rendered.contains("# [derive (Default , Serialize , Deserialize)]"),
        "{rendered}"
    );
}

#[test]
fn a_raw_field_name_is_the_setting_name_without_its_prefix() {
    let rendered = expanded(
        TokenStream::new(),
        quote! {
            struct LoopSettings {
                #[description = "Whether the clip repeats."]
                r#loop: bool,
            }
        },
    )
    .expect("expands");
    assert!(
        rendered.contains("\"loop\" , \"Whether the clip repeats.\""),
        "{rendered}"
    );
}

#[test]
fn a_field_without_a_description_is_refused_by_name() {
    let refused = refusal(
        TokenStream::new(),
        quote! {
            struct BlurSettings {
                kernel_size: u32,
            }
        },
    );
    assert!(refused.contains("setting `kernel_size`"), "{refused}");
    assert!(refused.contains("#[description"), "{refused}");
}

#[test]
fn a_description_that_is_neither_a_literal_nor_a_const_path_is_refused() {
    let refused = refusal(
        TokenStream::new(),
        quote! {
            struct BlurSettings {
                #[description = include_str!("kernel_size.md")]
                kernel_size: u32,
            }
        },
    );
    assert!(
        refused.contains("string literal or the path of a `const`"),
        "{refused}"
    );
}

#[test]
fn arguments_generics_and_unnamed_fields_are_refused() {
    let described = quote! {
        struct BlurSettings {
            #[description = "Size."]
            kernel_size: u32,
        }
    };
    assert!(refusal(quote! { strict }, described).contains("takes no arguments"));
    assert!(
        refusal(
            TokenStream::new(),
            quote! { struct BlurSettings<T> { #[description = "Size."] kernel_size: T } }
        )
        .contains("no generic parameters")
    );
    assert!(
        refusal(TokenStream::new(), quote! { struct BlurSettings(u32); }).contains("named fields")
    );
}

#[test]
fn options_are_refused_unless_one_list_of_string_literals() {
    for (options, refused_for) in [
        (quote! { #[options] }, "string literals"),
        (quote! { #[options(fast, fine)] }, "string literals"),
        (quote! { #[options()] }, "lists none"),
        (
            quote! { #[options("fast")] #[options("fine")] },
            "one `#[options(…)]`",
        ),
    ] {
        let refused = refusal(
            TokenStream::new(),
            quote! {
                struct BlurSettings {
                    #[description = "Quality."]
                    #options
                    quality: String,
                }
            },
        );
        assert!(refused.contains(refused_for), "{options}: {refused}");
    }
}

/// Mental-revert: reading the field's type alone declares a choice among any pin type, which
/// no stream fills with its bundles.
#[test]
fn a_field_marked_as_choosing_among_its_streams_bundles_is_declared_as_that_choice() {
    let rendered = expanded(
        TokenStream::new(),
        quote! {
            struct BundleSettings {
                #[description = "The bundle the node carries."]
                #[among_the_bundles_its_stream_declares]
                bundle_type: Option<PinType>,
            }
        },
    )
    .expect("expands");
    assert!(
        rendered.contains(
            "the_choice_among_the_bundles_its_stream_declares :: < Option < PinType > > ()"
        ),
        "{rendered}"
    );
    assert!(
        !rendered.contains("# [among_the_bundles_its_stream_declares]"),
        "{rendered}"
    );
}

#[test]
fn a_setting_chosen_from_options_and_among_its_streams_bundles_is_refused() {
    let refused = refusal(
        TokenStream::new(),
        quote! {
            struct BundleSettings {
                #[description = "The bundle the node carries."]
                #[options("one")]
                #[among_the_bundles_its_stream_declares]
                bundle_type: String,
            }
        },
    );
    assert!(refused.contains("not both"), "{refused}");
}

#[test]
fn a_marker_of_a_choice_among_its_streams_bundles_taking_arguments_is_refused() {
    let refused = refusal(
        TokenStream::new(),
        quote! {
            struct BundleSettings {
                #[description = "The bundle the node carries."]
                #[among_the_bundles_its_stream_declares(any)]
                bundle_type: Option<PinType>,
            }
        },
    );
    assert!(refused.contains("bare"), "{refused}");
}
