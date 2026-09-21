// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The application menu the engine installs over the window event pump.
//!
//! Its Quit asks the runtime to shut down, the same request Ctrl-C makes, so
//! `rt.run()` tears the graph down and returns. `terminate:` — what a stock
//! Quit sends — would exit the process from under the run loop instead.

use std::cell::OnceCell;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{NSApplication, NSMenu, NSMenuItem};
use objc2_foundation::{NSObject, NSObjectProtocol, NSProcessInfo, NSString};

define_class!(
    /// The Quit item's target: requests a runtime shutdown.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "StreamlibQuitMenuItemRequestsRuntimeShutdown"]
    struct QuitMenuItemRequestsRuntimeShutdown;

    impl QuitMenuItemRequestsRuntimeShutdown {
        #[unsafe(method(requestRuntimeShutdown:))]
        fn request_runtime_shutdown(&self, _sender: Option<&AnyObject>) {
            if let Err(e) =
                crate::core::runtime::request_runtime_shutdown("Quit from the application menu")
            {
                tracing::warn!(error = %e, "the application menu's Quit could not request a shutdown");
            }
        }
    }

    unsafe impl NSObjectProtocol for QuitMenuItemRequestsRuntimeShutdown {}
);

thread_local! {
    /// Held for the process's life: a menu item keeps only a weak reference to
    /// its target.
    static QUIT_MENU_ITEM_TARGET: OnceCell<Retained<QuitMenuItemRequestsRuntimeShutdown>> =
        const { OnceCell::new() };
}

/// Install the application menu once per process. Its only item is Quit
/// (Cmd+Q), which requests a runtime shutdown.
pub fn install_the_application_menu_whose_quit_requests_a_runtime_shutdown(
    first_thread: MainThreadMarker,
) {
    QUIT_MENU_ITEM_TARGET.with(|quit_menu_item_target| {
        if quit_menu_item_target.get().is_some() {
            return;
        }
        let target: Retained<QuitMenuItemRequestsRuntimeShutdown> = unsafe {
            msg_send![
                QuitMenuItemRequestsRuntimeShutdown::alloc(first_thread),
                init
            ]
        };

        let quit_title = NSString::from_str(&format!(
            "Quit {}",
            NSProcessInfo::processInfo().processName()
        ));
        let quit_item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                first_thread.alloc(),
                &quit_title,
                Some(sel!(requestRuntimeShutdown:)),
                &NSString::from_str("q"),
            )
        };
        unsafe { quit_item.setTarget(Some(&target)) };

        let application_submenu = NSMenu::new(first_thread);
        application_submenu.addItem(&quit_item);
        let application_menu_item = NSMenuItem::new(first_thread);
        application_menu_item.setSubmenu(Some(&application_submenu));
        let menu_bar = NSMenu::new(first_thread);
        menu_bar.addItem(&application_menu_item);
        NSApplication::sharedApplication(first_thread).setMainMenu(Some(&menu_bar));

        let _ = quit_menu_item_target.set(target);
    });
}
