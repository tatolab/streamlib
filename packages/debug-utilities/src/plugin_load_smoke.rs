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
//! off the `STREAMLIB_PLUGIN` symbol at dlopen time; introspecting every
//! processor in the `export_plugin!` list here proves each one's
//! macro-generated definition is well-formed and that the crate compiles
//! cdylib-safe. `BgraFileSource` is the processor that regressed (its
//! engine-only ring upload broke the cdylib compile since the package went
//! engine-free): reverting its pooled-pixel-buffer fix reintroduces the
//! engine-only call, the crate stops compiling, and this whole test target
//! fails to build.

use streamlib_plugin_sdk::sdk::descriptors::PortDescriptor;
use streamlib_plugin_sdk::sdk::processors::GeneratedProcessor;

fn port_names(ports: &[PortDescriptor]) -> Vec<&str> {
    ports.iter().map(|port| port.name.as_str()).collect()
}

#[test]
fn simple_passthrough_descriptor_loads() {
    let descriptor = <crate::SimplePassthroughProcessor::Processor as GeneratedProcessor>::descriptor()
        .expect("SimplePassthrough must expose a macro-generated descriptor");

    assert_eq!(descriptor.name.r#type.as_str(), "SimplePassthrough");
    assert_eq!(
        port_names(&descriptor.inputs),
        vec!["input"],
        "SimplePassthrough declares a single `input` port"
    );
    assert_eq!(
        port_names(&descriptor.outputs),
        vec!["output"],
        "SimplePassthrough declares a single `output` port"
    );
}

#[test]
fn video_frame_counter_descriptor_loads() {
    let descriptor = <crate::VideoFrameCounterProcessor::Processor as GeneratedProcessor>::descriptor()
        .expect("VideoFrameCounter must expose a macro-generated descriptor");

    assert_eq!(descriptor.name.r#type.as_str(), "VideoFrameCounter");
    assert_eq!(
        port_names(&descriptor.inputs),
        vec!["input"],
        "VideoFrameCounter declares a single `input` port"
    );
    assert!(
        descriptor.outputs.is_empty(),
        "VideoFrameCounter is a sink — it declares no output ports"
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
        port_names(&descriptor.outputs),
        vec!["video"],
        "BgraFileSource declares a single `video` output port"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn jpeg_bytes_source_descriptor_loads() {
    let descriptor = <crate::JpegBytesSourceProcessor::Processor as GeneratedProcessor>::descriptor()
        .expect("JpegBytesSource must expose a macro-generated descriptor");

    assert_eq!(descriptor.name.r#type.as_str(), "JpegBytesSource");
    assert!(
        descriptor.inputs.is_empty(),
        "JpegBytesSource is a source — it declares no input ports"
    );
    assert_eq!(
        port_names(&descriptor.outputs),
        vec!["encoded_jpeg"],
        "JpegBytesSource declares a single `encoded_jpeg` output port"
    );
}
