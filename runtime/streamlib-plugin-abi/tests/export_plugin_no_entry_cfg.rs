// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `export_plugin!` at the zero-attribute call shape — the shape every
//! hand-written in-tree caller uses. Pins the emitted `STREAMLIB_PLUGIN` wire
//! bytes, the first-entry anchor, install-exactly-once, registration order, and
//! the null-host-services refusal.

mod common;

use std::sync::atomic::Ordering;

use common::declare_stub_processor_type;
use streamlib_plugin_abi::{PluginDeclaration, STREAMLIB_ABI_VERSION};

declare_stub_processor_type!(FirstStubProcessor, "identity-first");
declare_stub_processor_type!(SecondStubProcessor, "identity-second");
declare_stub_processor_type!(ThirdStubProcessor, "identity-third");

streamlib_plugin_abi::export_plugin!(FirstStubProcessor, SecondStubProcessor, ThirdStubProcessor,);

#[test]
fn declaration_carries_the_pinned_wire_envelope() {
    assert_eq!(STREAMLIB_PLUGIN.abi_version, STREAMLIB_ABI_VERSION);
    assert_eq!(STREAMLIB_PLUGIN._reserved_padding, 0);
    assert_eq!(size_of::<PluginDeclaration>(), 40);
    assert_eq!(align_of::<PluginDeclaration>(), 8);
}

#[test]
fn the_first_entry_anchors_the_fingerprint_and_build_identity() {
    assert_eq!(
        STREAMLIB_PLUGIN.abi_layout_fingerprint,
        FirstStubProcessor::__STREAMLIB_ABI_LAYOUT_FINGERPRINT
    );
    assert!(!STREAMLIB_PLUGIN.build_identity_ptr.is_null());
    assert_eq!(
        STREAMLIB_PLUGIN.build_identity_len,
        FirstStubProcessor::__STREAMLIB_BUILD_IDENTITY.len()
    );
    assert_eq!(
        common::declaration_build_identity(&STREAMLIB_PLUGIN),
        FirstStubProcessor::__STREAMLIB_BUILD_IDENTITY
    );
    assert_ne!(
        common::declaration_build_identity(&STREAMLIB_PLUGIN),
        SecondStubProcessor::__STREAMLIB_BUILD_IDENTITY
    );
}

#[test]
fn register_installs_once_and_registers_every_entry_in_declaration_order() {
    common::reset_plugin_registration_recorders();

    // SAFETY: the stubs never dereference the pointer; a non-null address is
    // all the `PluginRegisterFn` contract needs to model a real host payload.
    unsafe { (STREAMLIB_PLUGIN.register)(common::non_null_host_services_pointer()) };

    assert_eq!(
        common::HOST_SERVICES_INSTALL_CALL_COUNT.load(Ordering::SeqCst),
        1,
        "install must run exactly once, from the first surviving entry"
    );
    assert_eq!(
        *common::HOST_SERVICES_INSTALL_ANCHOR_TYPE_NAME
            .lock()
            .unwrap(),
        Some("FirstStubProcessor")
    );
    assert_eq!(
        *common::REGISTERED_PROCESSOR_TYPE_NAMES.lock().unwrap(),
        vec![
            "FirstStubProcessor",
            "SecondStubProcessor",
            "ThirdStubProcessor"
        ]
    );

    // Null host services: the install refuses, the callback returns without
    // registering anything, and nothing unwinds across the `extern "C"` edge.
    common::reset_plugin_registration_recorders();
    // SAFETY: a null pointer is the documented refusal leg — the macro's
    // early return is exactly what this exercises.
    unsafe { (STREAMLIB_PLUGIN.register)(std::ptr::null()) };
    assert_eq!(
        common::HOST_SERVICES_INSTALL_CALL_COUNT.load(Ordering::SeqCst),
        1
    );
    assert!(
        common::REGISTERED_PROCESSOR_TYPE_NAMES
            .lock()
            .unwrap()
            .is_empty(),
        "a refused install must register no processor"
    );
}
