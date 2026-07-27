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

declare_stub_processor_type!(
    GatedOutStubProcessor,
    0x1111_1111_1111_1111,
    "identity-gated-out"
);
declare_stub_processor_type!(
    SurvivingStubProcessor,
    0x2222_2222_2222_2222,
    "identity-surviving"
);
declare_stub_processor_type!(
    TrailingStubProcessor,
    0x3333_3333_3333_3333,
    "identity-trailing"
);

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
    assert_ne!(
        STREAMLIB_PLUGIN.abi_layout_fingerprint,
        GatedOutStubProcessor::__STREAMLIB_ABI_LAYOUT_FINGERPRINT
    );
    assert_eq!(
        STREAMLIB_PLUGIN.build_identity_len,
        SurvivingStubProcessor::__STREAMLIB_BUILD_IDENTITY.len()
    );
    // SAFETY: the macro points the pair at a `'static str` owned by the
    // plugin image, per the `PluginDeclaration` contract.
    let identity = unsafe {
        std::str::from_utf8(std::slice::from_raw_parts(
            STREAMLIB_PLUGIN.build_identity_ptr,
            STREAMLIB_PLUGIN.build_identity_len,
        ))
        .unwrap()
    };
    assert_eq!(identity, SurvivingStubProcessor::__STREAMLIB_BUILD_IDENTITY);
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
