// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The macOS half of starting CEF: the framework loaded from the host's app
//! bundle, and the shared `NSApplication` made an instance of a subclass
//! answering the send-event queries Chromium's message pump makes of it.

use std::sync::atomic::{AtomicBool, Ordering};

use cef::library_loader::LibraryLoader;
use objc2::runtime::{AnyClass, AnyObject, AnyProtocol, Bool, ClassBuilder, Sel};
use objc2::{msg_send, sel};

/// The class `NSApp` is made an instance of.
const BROWSER_HOST_APPLICATION_CLASS_NAME: &std::ffi::CStr = c"TatolabBrowserHostApplication";

/// The protocols CEF declares `NSApp` conforms to, outermost last.
const CEF_APPLICATION_PROTOCOL_NAMES: [&std::ffi::CStr; 3] =
    [c"CrAppProtocol", c"CrAppControlProtocol", c"CefAppProtocol"];

/// `NSApplicationActivationPolicyAccessory`: no Dock icon and no menu bar.
const NS_APPLICATION_ACTIVATION_POLICY_ACCESSORY: isize = 1;

/// Whether `NSApp` is inside `sendEvent:`; there is one `NSApp` per process.
static NS_APP_IS_HANDLING_SEND_EVENT: AtomicBool = AtomicBool::new(false);

/// Load `Chromium Embedded Framework.framework` from the bundle the running
/// executable sits in; the framework stays loaded while the loader lives.
pub fn load_the_cef_framework_of_this_bundle() -> Result<LibraryLoader, String> {
    let executable = std::env::current_exe()
        .map_err(|failure| format!("the host's own executable was not found: {failure}"))?;
    let framework = executable
        .parent()
        .map(|macos_directory| {
            macos_directory.join("../Frameworks/Chromium Embedded Framework.framework")
        })
        .filter(|framework| framework.is_dir())
        .ok_or_else(|| {
            format!(
                "{} sits in no app bundle holding Chromium Embedded Framework.framework; the \
                 runtime unit stages the host as tatolab-browser-host.app",
                executable.display()
            )
        })?;
    let library_loader = LibraryLoader::new(&executable, false);
    if !library_loader.load() {
        return Err(format!("{} did not load", framework.display()));
    }
    Ok(library_loader)
}

/// Make the shared `NSApplication` a [`BROWSER_HOST_APPLICATION_CLASS_NAME`]
/// with no Dock icon. Called on the main thread before CEF initializes.
pub fn make_the_shared_application_a_cef_application() -> Result<(), String> {
    let browser_host_application_class = browser_host_application_class()?;
    // SAFETY: `sharedApplication` makes `NSApp` an instance of the receiving
    // class the first time it is sent, on the main thread this runs on.
    let shared_application: *mut AnyObject =
        unsafe { msg_send![browser_host_application_class, sharedApplication] };
    if shared_application.is_null() {
        return Err("NSApplication made no shared application".to_string());
    }
    // SAFETY: a live NSApplication, sent a documented selector.
    let _: Bool = unsafe {
        msg_send![
            shared_application,
            setActivationPolicy: NS_APPLICATION_ACTIVATION_POLICY_ACCESSORY
        ]
    };
    Ok(())
}

fn browser_host_application_class() -> Result<&'static AnyClass, String> {
    if let Some(registered) = AnyClass::get(BROWSER_HOST_APPLICATION_CLASS_NAME) {
        return Ok(registered);
    }
    let ns_application = AnyClass::get(c"NSApplication")
        .ok_or_else(|| "AppKit's NSApplication class is not loaded".to_string())?;
    let mut class_builder = ClassBuilder::new(BROWSER_HOST_APPLICATION_CLASS_NAME, ns_application)
        .ok_or_else(|| "the browser host's NSApplication subclass was not declared".to_string())?;
    extern "C-unwind" fn is_handling_send_event(_application: &AnyObject, _selector: Sel) -> Bool {
        Bool::new(NS_APP_IS_HANDLING_SEND_EVENT.load(Ordering::Acquire))
    }
    extern "C-unwind" fn set_handling_send_event(
        _application: &AnyObject,
        _selector: Sel,
        handling_send_event: Bool,
    ) {
        NS_APP_IS_HANDLING_SEND_EVENT.store(handling_send_event.as_bool(), Ordering::Release);
    }
    // SAFETY: each function's signature is the selector's: `-(BOOL)` and
    // `-(void)(BOOL)`.
    unsafe {
        class_builder.add_method(
            sel!(isHandlingSendEvent),
            is_handling_send_event as extern "C-unwind" fn(_, _) -> _,
        );
        class_builder.add_method(
            sel!(setHandlingSendEvent:),
            set_handling_send_event as extern "C-unwind" fn(_, _, _),
        );
    }
    for protocol_name in CEF_APPLICATION_PROTOCOL_NAMES {
        if let Some(protocol) = AnyProtocol::get(protocol_name) {
            class_builder.add_protocol(protocol);
        }
    }
    Ok(class_builder.register())
}
