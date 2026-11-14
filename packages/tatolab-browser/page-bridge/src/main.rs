// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! `tatolab-browser-subprocess`: the binary CEF re-executes for every child
//! process of the browser host — renderer, GPU, network and utility.

use cef::args::Args;
use tatolab_browser_page_bridge::{BrowserChildProcessApp, die_with_the_browser_process};

fn main() {
    #[cfg(target_os = "macos")]
    let _cef_framework = cef_framework_of_the_app_bundle_this_helper_sits_in();
    die_with_the_browser_process();
    let _ = cef::api_hash(cef::sys::CEF_API_VERSION_LAST, 0);
    let args = Args::new();
    let mut app = BrowserChildProcessApp::new();
    let exit_code = cef::execute_process(
        Some(args.as_main_args()),
        Some(&mut app),
        std::ptr::null_mut(),
    );
    std::process::exit(exit_code.max(0));
}

/// Load the framework of the app bundle whose `Frameworks/` holds this
/// helper's own bundle; the process exits when it does not load.
#[cfg(target_os = "macos")]
fn cef_framework_of_the_app_bundle_this_helper_sits_in() -> cef::library_loader::LibraryLoader {
    let Ok(executable) = std::env::current_exe() else {
        std::process::exit(1);
    };
    let library_loader = cef::library_loader::LibraryLoader::new(&executable, true);
    if !library_loader.load() {
        std::process::exit(1);
    }
    library_loader
}
