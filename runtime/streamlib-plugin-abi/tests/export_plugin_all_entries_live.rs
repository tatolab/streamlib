// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `export_plugin!` where every entry carries a `#[cfg]` that holds — the shape
//! a crate-root generator emits when every processor arm is conditional but the
//! build target satisfies all of them. Attribute-carrying entries must behave
//! exactly like bare ones.

mod common;

use std::sync::atomic::Ordering;

use common::declare_stub_processor_type;
use streamlib_plugin_abi::STREAMLIB_ABI_VERSION;

declare_stub_processor_type!(
    VacuouslyTrueStubProcessor,
    0x4444_4444_4444_4444,
    "identity-vacuously-true"
);
declare_stub_processor_type!(
    NegatedFalseStubProcessor,
    0x5555_5555_5555_5555,
    "identity-negated-false"
);
declare_stub_processor_type!(
    AnyFamilyStubProcessor,
    0x6666_6666_6666_6666,
    "identity-any-family"
);

streamlib_plugin_abi::export_plugin!(
    #[cfg(all())]
    VacuouslyTrueStubProcessor,
    #[cfg(not(any()))]
    NegatedFalseStubProcessor,
    #[cfg(any(unix, windows))]
    AnyFamilyStubProcessor,
);

#[test]
fn every_live_entry_registers_and_the_first_anchors() {
    assert_eq!(STREAMLIB_PLUGIN.abi_version, STREAMLIB_ABI_VERSION);
    assert_eq!(
        STREAMLIB_PLUGIN.abi_layout_fingerprint,
        VacuouslyTrueStubProcessor::__STREAMLIB_ABI_LAYOUT_FINGERPRINT
    );

    common::reset_plugin_registration_recorders();
    // SAFETY: the stubs never dereference the pointer.
    unsafe { (STREAMLIB_PLUGIN.register)(common::non_null_host_services_pointer()) };

    assert_eq!(
        common::HOST_SERVICES_INSTALL_CALL_COUNT.load(Ordering::SeqCst),
        1
    );
    assert_eq!(
        *common::REGISTERED_PROCESSOR_TYPE_NAMES.lock().unwrap(),
        vec![
            "VacuouslyTrueStubProcessor",
            "NegatedFalseStubProcessor",
            "AnyFamilyStubProcessor"
        ]
    );
}
