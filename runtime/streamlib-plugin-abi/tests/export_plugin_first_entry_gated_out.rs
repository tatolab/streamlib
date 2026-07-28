// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `export_plugin!` with the first entry stripped by its own `#[cfg]`.
//!
//! This is the shape a crate-root generator emits for a package whose first
//! processor arm is platform-gated: the anchor must slide to the first entry
//! that SURVIVES cfg-stripping, not stay pinned to the syntactically-first one.
//! Mentally revert the anchors to `$first` and this fails to compile (the
//! stripped type is not in scope for the declaration).

mod common;

use std::sync::atomic::Ordering;

use common::declare_stub_processor_type;
use streamlib_plugin_abi::STREAMLIB_ABI_VERSION;

declare_stub_processor_type!(GatedOutStubProcessor, "identity-gated-out");
declare_stub_processor_type!(SurvivingStubProcessor, "identity-surviving");
declare_stub_processor_type!(TrailingStubProcessor, "identity-trailing");

streamlib_plugin_abi::export_plugin!(
    #[cfg(any())]
    GatedOutStubProcessor,
    SurvivingStubProcessor,
    #[cfg(all())]
    TrailingStubProcessor,
);

#[test]
fn the_first_surviving_entry_anchors_the_declaration() {
    assert_eq!(STREAMLIB_PLUGIN.abi_version, STREAMLIB_ABI_VERSION);
    assert_eq!(
        STREAMLIB_PLUGIN.abi_layout_fingerprint,
        SurvivingStubProcessor::__STREAMLIB_ABI_LAYOUT_FINGERPRINT
    );
    assert_eq!(
        STREAMLIB_PLUGIN.build_identity_len,
        SurvivingStubProcessor::__STREAMLIB_BUILD_IDENTITY.len()
    );
    // The build identity is what makes the anchor slide observable: every entry
    // agrees on the fingerprint by construction (`export_plugin!` const-asserts
    // it), so only the identity distinguishes which entry anchored.
    let identity = common::declaration_build_identity(&STREAMLIB_PLUGIN);
    assert_eq!(identity, SurvivingStubProcessor::__STREAMLIB_BUILD_IDENTITY);
    assert_ne!(identity, GatedOutStubProcessor::__STREAMLIB_BUILD_IDENTITY);
}

#[test]
fn only_the_surviving_entries_register() {
    common::reset_plugin_registration_recorders();

    // SAFETY: the stubs never dereference the pointer.
    unsafe { (STREAMLIB_PLUGIN.register)(common::non_null_host_services_pointer()) };

    assert_eq!(
        common::HOST_SERVICES_INSTALL_CALL_COUNT.load(Ordering::SeqCst),
        1
    );
    assert_eq!(
        *common::HOST_SERVICES_INSTALL_ANCHOR_TYPE_NAME
            .lock()
            .unwrap(),
        Some("SurvivingStubProcessor")
    );
    assert_eq!(
        *common::REGISTERED_PROCESSOR_TYPE_NAMES.lock().unwrap(),
        vec!["SurvivingStubProcessor", "TrailingStubProcessor"]
    );
}
