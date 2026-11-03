// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `tatolab-browser-subprocess`: the binary CEF re-executes for every child
//! process of the browser host — renderer, GPU, network and utility.

use cef::args::Args;
use tatolab_browser_page_bridge::{BrowserChildProcessApp, die_with_the_browser_process};

fn main() {
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
