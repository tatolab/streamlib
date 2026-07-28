// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `export_plugin!`'s generated `register` is the one `extern "C"` slot the
//! macro emits, and a panic crossing it is UB — the host and the cdylib may be
//! built with different panic strategies. The `catch_unwind` net is what makes
//! that impossible; nothing else in the crate witnesses it.
//!
//! Both bodies the net wraps are exercised: the anchor entry's install (inside
//! the labeled block that resolves the first surviving entry) and the per-entry
//! register loop. Mentally delete the `catch_unwind` and this binary aborts
//! rather than fails.

mod common;

use std::sync::atomic::Ordering;

use common::declare_stub_processor_type;

declare_stub_processor_type!(AnchorInstallStubProcessor, "identity-anchor-install");
declare_stub_processor_type!(
    PanickingRegisterStubProcessor,
    "identity-panicking-register"
);
declare_stub_processor_type!(AfterThePanicStubProcessor, "identity-after-the-panic");

streamlib_plugin_abi::export_plugin!(
    AnchorInstallStubProcessor,
    PanickingRegisterStubProcessor,
    AfterThePanicStubProcessor,
);

/// One test drives both legs sequentially: they share the recorder statics and
/// the process-wide panic hook, so running them as separate `#[test]`s would
/// race.
#[test]
fn a_panicking_entry_is_contained_by_the_panic_safety_net() {
    common::reset_plugin_registration_recorders();
    *common::PANIC_INSIDE_REGISTER_FOR_PROCESSOR_TYPE_NAME
        .lock()
        .unwrap() = Some("PanickingRegisterStubProcessor");

    common::with_the_panic_hook_suppressed(|| {
        // SAFETY: the stubs never dereference the pointer. Returning at all is
        // the assertion — an escaped unwind aborts the process here.
        unsafe { (STREAMLIB_PLUGIN.register)(common::non_null_host_services_pointer()) };
    });

    assert_eq!(
        common::HOST_SERVICES_INSTALL_CALL_COUNT.load(Ordering::SeqCst),
        1
    );
    assert_eq!(
        *common::REGISTERED_PROCESSOR_TYPE_NAMES.lock().unwrap(),
        vec![
            "AnchorInstallStubProcessor",
            "PanickingRegisterStubProcessor"
        ],
        "the net wraps the whole register loop, so a panicking entry costs \
         every later entry its registration"
    );

    common::reset_plugin_registration_recorders();
    common::PANIC_INSIDE_HOST_SERVICES_INSTALL.store(true, Ordering::SeqCst);

    common::with_the_panic_hook_suppressed(|| {
        // SAFETY: as above — the install panics before the pointer is read.
        unsafe { (STREAMLIB_PLUGIN.register)(common::non_null_host_services_pointer()) };
    });

    assert_eq!(
        common::HOST_SERVICES_INSTALL_CALL_COUNT.load(Ordering::SeqCst),
        1,
        "a panicking install is attempted once and never retried by a later entry"
    );
    assert!(
        common::REGISTERED_PROCESSOR_TYPE_NAMES
            .lock()
            .unwrap()
            .is_empty(),
        "a panicking install must register no processor"
    );
}
