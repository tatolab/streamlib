// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Cdylib-safe plugin-load smoke for `@tatolab/debug-utilities`.
//!
//! The package is a source-only `.slpkg` built at load time against the
//! plugin SDK twin (`streamlib-plugin-sdk`), never the engine. A processor
//! body that reaches an engine-only primitive — e.g. the host-only
//! `TextureRing::copy_pixel_buffer_to_slot`, which the SDK twin omits by
//! design — makes the whole crate fail to compile, so no processor in it
//! can ever load.
//!
//! Loading each processor's [`ProcessorDescriptor`] is what the host reads
//! off the `STREAMLIB_PLUGIN` symbol at dlopen time; introspecting it here
//! proves every processor's macro-generated definition is well-formed and
//! that the crate compiles cdylib-safe. `BgraFileSource` is the processor
//! that regressed (its engine-only ring upload broke the cdylib compile
//! since the package went engine-free): reverting its pooled-pixel-buffer
//! fix reintroduces the engine-only call, the crate stops compiling, and
//! this whole test target fails to build.

use streamlib_plugin_sdk::sdk::processors::GeneratedProcessor;

#[test]
fn simple_passthrough_descriptor_loads() {
    let descriptor = <crate::SimplePassthroughProcessor::Processor as GeneratedProcessor>::descriptor()
        .expect("SimplePassthrough must expose a macro-generated descriptor");

    assert_eq!(descriptor.name.r#type.as_str(), "SimplePassthrough");
    assert_eq!(
        descriptor.inputs.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
        vec!["input"],
        "SimplePassthrough declares a single `input` port"
    );
    assert_eq!(
        descriptor.outputs.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
        vec!["output"],
        "SimplePassthrough declares a single `output` port"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn bgra_file_source_descriptor_loads_cdylib_safe() {
    // The regressed processor: this only compiles if `bgra_file_source`
    // stages through the pooled pixel buffer (pool id doubles as
    // `surface_id`) instead of the engine-only `TextureRing` CPU upload.
    let descriptor = <crate::BgraFileSourceProcessor::Processor as GeneratedProcessor>::descriptor()
        .expect("BgraFileSource must expose a macro-generated descriptor");

    assert_eq!(descriptor.name.r#type.as_str(), "BgraFileSource");
    assert!(
        descriptor.inputs.is_empty(),
        "BgraFileSource is a source — it declares no input ports"
    );
    assert_eq!(
        descriptor.outputs.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
        vec!["video"],
        "BgraFileSource declares a single `video` output port"
    );
}
