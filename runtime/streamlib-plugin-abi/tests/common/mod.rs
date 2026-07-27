// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Stub processor types standing in for the anchor surface the `#[processor]`
//! macro emits, so `export_plugin!`'s emitted wire shape is testable inside the
//! plugin ABI crate — which by design names no SDK path and cannot depend on
//! one.

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

/// How many times a generated register callback called
/// `__streamlib_install_host_services`. The macro contract is exactly once per
/// invocation, from the first entry that survives cfg-stripping.
pub static HOST_SERVICES_INSTALL_CALL_COUNT: AtomicUsize = AtomicUsize::new(0);

/// The processor type name that performed the host-services install, if any.
pub static HOST_SERVICES_INSTALL_ANCHOR_TYPE_NAME: Mutex<Option<&'static str>> = Mutex::new(None);

/// Processor type names registered through the helper, in call order.
pub static REGISTERED_PROCESSOR_TYPE_NAMES: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());

/// Reset every recorder so one test file can drive the register callback more
/// than once (the null-host-services leg after the positive leg).
pub fn reset_plugin_registration_recorders() {
    HOST_SERVICES_INSTALL_CALL_COUNT.store(0, Ordering::SeqCst);
    *HOST_SERVICES_INSTALL_ANCHOR_TYPE_NAME.lock().unwrap() = None;
    REGISTERED_PROCESSOR_TYPE_NAMES.lock().unwrap().clear();
}

/// Stand-in for the SDK's `RegisterHelper` — the opaque token
/// `install_host_services` hands back and every entry registers through.
pub struct StubPluginRegisterHelper {
    pub installed_from_processor_type_name: &'static str,
}

/// Declare a stub processor type carrying the four associated items
/// `export_plugin!` resolves against a real `#[processor]`-generated type.
/// Distinct fingerprint / identity values per type make "which entry anchored
/// the declaration" observable.
macro_rules! declare_stub_processor_type {
    ($type_name:ident, $abi_layout_fingerprint:expr, $build_identity:expr) => {
        pub struct $type_name;

        impl $type_name {
            pub const __STREAMLIB_ABI_LAYOUT_FINGERPRINT: u64 = $abi_layout_fingerprint;
            pub const __STREAMLIB_BUILD_IDENTITY: &'static str = $build_identity;

            /// # Safety
            ///
            /// Mirrors the SDK signature. The pointer is only tested for null,
            /// never dereferenced.
            pub unsafe fn __streamlib_install_host_services(
                host_services: *const ::core::ffi::c_void,
            ) -> ::core::option::Option<$crate::common::StubPluginRegisterHelper> {
                $crate::common::record_host_services_install(
                    ::core::stringify!($type_name),
                    host_services,
                )
            }

            pub fn __streamlib_register(
                register_helper: &$crate::common::StubPluginRegisterHelper,
            ) {
                $crate::common::record_processor_registration(
                    ::core::stringify!($type_name),
                    register_helper,
                );
            }
        }
    };
}

pub(crate) use declare_stub_processor_type;

/// Record one install attempt. A null host-services pointer refuses the install
/// the way the SDK does, so the generated callback takes its early-return leg.
pub fn record_host_services_install(
    processor_type_name: &'static str,
    host_services: *const core::ffi::c_void,
) -> Option<StubPluginRegisterHelper> {
    HOST_SERVICES_INSTALL_CALL_COUNT.fetch_add(1, Ordering::SeqCst);
    if host_services.is_null() {
        return None;
    }
    *HOST_SERVICES_INSTALL_ANCHOR_TYPE_NAME.lock().unwrap() = Some(processor_type_name);
    Some(StubPluginRegisterHelper {
        installed_from_processor_type_name: processor_type_name,
    })
}

/// Record one registration, asserting it routed through the helper the install
/// leg produced rather than a second, per-entry install.
pub fn record_processor_registration(
    processor_type_name: &'static str,
    register_helper: &StubPluginRegisterHelper,
) {
    let anchor = HOST_SERVICES_INSTALL_ANCHOR_TYPE_NAME.lock().unwrap();
    assert_eq!(
        *anchor,
        Some(register_helper.installed_from_processor_type_name),
        "registration of {processor_type_name} used a helper from an entry that \
         did not perform the install"
    );
    drop(anchor);
    REGISTERED_PROCESSOR_TYPE_NAMES
        .lock()
        .unwrap()
        .push(processor_type_name);
}

/// A non-null host-services pointer. The stubs never dereference it, so any
/// non-null address models "the host handed us a payload".
pub fn non_null_host_services_pointer() -> *const core::ffi::c_void {
    &HOST_SERVICES_INSTALL_CALL_COUNT as *const AtomicUsize as *const core::ffi::c_void
}
