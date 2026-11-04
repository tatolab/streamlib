// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! CEF's browser process on the host's main thread: initialized once, then
//! pumped between the requests the tab connections send it, until the host is
//! asked to stop.

use std::collections::{HashMap, VecDeque};
use std::ffi::CString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cef::rc::Rc as _;
use cef::*;
use tatolab_browser_page_bridge::{BROWSER_PROCESS_PID_SWITCH_NAME, page_data_dispatch_script};
use tatolab_browser_tab_protocol::{BrowserHostTabLogLevel, TabProtocolMessageFromBrowserHost};

use crate::browser_host_page_handlers::browser_host_page_client;
use crate::browser_host_tab::{BrowserHostTab, BrowserHostTabId};
use crate::page_data_scripts_awaiting_the_loaded_document::PageDataScriptsAwaitingTheLoadedDocument;
use crate::tenant_profile_directory::tenant_profile_directory;

/// The binary CEF re-executes for its child processes, beside the host.
pub const BROWSER_SUBPROCESS_EXECUTABLE_NAME: &str = "tatolab-browser-subprocess";

/// The longest the CEF thread sleeps when CEF asked for no pump sooner.
const LONGEST_PUMP_INTERVAL: Duration = Duration::from_millis(8);

/// How long a stopping host waits for CEF to let its pages go.
const PAGES_CLOSE_BUDGET_AT_STOP: Duration = Duration::from_secs(5);

/// How many renderer ends inside [`RENDERER_END_WINDOW`] a tab reloads after;
/// past them it stays blank.
const MOST_RENDERER_ENDS_RELOADED_IN_THE_WINDOW: usize = 5;
const RENDERER_END_WINDOW: Duration = Duration::from_secs(60);

/// One request the CEF thread serves between pumps.
pub enum CefThreadRequest {
    OpenTab {
        tab: Arc<BrowserHostTab>,
        opened: SyncSender<Result<(), String>>,
    },
    DeliverPageData {
        tab_id: BrowserHostTabId,
        detail_json: String,
    },
    CloseTab {
        tab_id: BrowserHostTabId,
    },
    ReloadTabAfterItsRendererEnded {
        tab_id: BrowserHostTabId,
    },
    PumpWithin {
        delay: Duration,
    },
}

/// The way every other thread reaches the CEF thread.
#[derive(Clone)]
pub struct CefThreadRequestSender {
    requests: Arc<Mutex<Sender<CefThreadRequest>>>,
}

impl CefThreadRequestSender {
    /// Queue `request`; `false` once the CEF thread has ended.
    pub fn send(&self, request: CefThreadRequest) -> bool {
        self.requests
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .send(request)
            .is_ok()
    }
}

/// A fresh request channel, its sending side for every other thread.
pub fn cef_thread_request_channel() -> (CefThreadRequestSender, Receiver<CefThreadRequest>) {
    let (requests, requests_to_serve) = std::sync::mpsc::channel();
    (
        CefThreadRequestSender {
            requests: Arc::new(Mutex::new(requests)),
        },
        requests_to_serve,
    )
}

/// Initialize CEF from the runtime in `cef_runtime_directory`, caching under
/// `root_cache_directory`. Called on the thread that then serves requests.
pub fn initialize_cef(
    cef_runtime_directory: &Path,
    root_cache_directory: &Path,
    cef_thread_requests: &CefThreadRequestSender,
) -> Result<(), String> {
    let subprocess_path = cef_runtime_directory.join(BROWSER_SUBPROCESS_EXECUTABLE_NAME);
    if !subprocess_path.is_file() {
        return Err(format!(
            "the CEF runtime at {} holds no {BROWSER_SUBPROCESS_EXECUTABLE_NAME}; the runtime \
             unit stages it beside the host",
            cef_runtime_directory.display()
        ));
    }
    std::fs::create_dir_all(root_cache_directory).map_err(|failure| {
        format!(
            "CEF's cache directory {} could not be created: {failure}",
            root_cache_directory.display()
        )
    })?;
    let _ = api_hash(sys::CEF_API_VERSION_LAST, 0);
    let path_text = |path: &Path| CefString::from(path.to_string_lossy().as_ref());
    let settings = Settings {
        no_sandbox: 1,
        browser_subprocess_path: path_text(&subprocess_path),
        resources_dir_path: path_text(cef_runtime_directory),
        locales_dir_path: path_text(&cef_runtime_directory.join("locales")),
        root_cache_path: path_text(root_cache_directory),
        log_file: path_text(&root_cache_directory.join("cef.log")),
        log_severity: LogSeverity::WARNING,
        windowless_rendering_enabled: 1,
        external_message_pump: 1,
        command_line_args_disabled: 1,
        persist_session_cookies: 1,
        ..Default::default()
    };
    let program_name = CString::new("tatolab-browser-host").expect("no interior NUL");
    let mut argv = [program_name.as_ptr().cast_mut()];
    let main_args = MainArgs {
        argc: 1,
        argv: argv.as_mut_ptr(),
    };
    let mut app = BrowserHostApp::new(cef_thread_requests.clone());
    if initialize(
        Some(&main_args),
        Some(&settings),
        Some(&mut app),
        std::ptr::null_mut(),
    ) != 1
    {
        return Err(format!(
            "CEF did not initialize from {}; its log is {}",
            cef_runtime_directory.display(),
            root_cache_directory.join("cef.log").display()
        ));
    }
    Ok(())
}

/// A page the CEF thread renders.
struct OpenTab {
    browser: Browser,
    tab: Arc<BrowserHostTab>,
    data_scripts_awaiting_the_loaded_document: PageDataScriptsAwaitingTheLoadedDocument,
    renderer_ends_in_the_window: VecDeque<Instant>,
}

impl OpenTab {
    fn main_frame_document_loaded(&self) -> bool {
        self.tab.main_frame_document_loaded.load(Ordering::Acquire)
    }

    fn dispatch_or_hold_data_script(&mut self, script: String) {
        if self.main_frame_document_loaded() {
            self.dispatch_data_script(&script);
        } else if self.data_scripts_awaiting_the_loaded_document.hold(script) {
            self.tab.log_to_the_node(
                BrowserHostTabLogLevel::Warn,
                "the page has not loaded, and the oldest data message held for it was dropped",
            );
        }
    }

    fn dispatch_data_scripts_held_for_the_loaded_document(&mut self) {
        if !self.main_frame_document_loaded() {
            return;
        }
        for script in self
            .data_scripts_awaiting_the_loaded_document
            .take_in_arrival_order()
        {
            self.dispatch_data_script(&script);
        }
    }

    fn dispatch_data_script(&self, script: &str) {
        let Some(frame) = self.browser.main_frame() else {
            return;
        };
        frame.execute_java_script(
            Some(&CefString::from(script)),
            Some(&CefString::from("tatolab://data")),
            1,
        );
    }

    fn close(&self) {
        if let Some(host) = self.browser.host() {
            host.close_browser(1);
        }
    }

    /// Reload after the renderer ended, unless it ended too often lately.
    fn reload_after_the_renderer_ended(&mut self) {
        let now = Instant::now();
        while self
            .renderer_ends_in_the_window
            .front()
            .is_some_and(|ended_at| now.duration_since(*ended_at) > RENDERER_END_WINDOW)
        {
            self.renderer_ends_in_the_window.pop_front();
        }
        self.renderer_ends_in_the_window.push_back(now);
        if self.renderer_ends_in_the_window.len() > MOST_RENDERER_ENDS_RELOADED_IN_THE_WINDOW {
            self.tab
                .send_to_the_node(&TabProtocolMessageFromBrowserHost::PageRenderingGaveUp {
                    reason: format!(
                        "the page's renderer ended {} times in {RENDERER_END_WINDOW:?}; the page \
                         is no longer reloaded",
                        self.renderer_ends_in_the_window.len()
                    ),
                });
            return;
        }
        self.tab.log_to_the_node(
            BrowserHostTabLogLevel::Info,
            format!(
                "the page reloads after its renderer ended ({} in {RENDERER_END_WINDOW:?})",
                self.renderer_ends_in_the_window.len()
            ),
        );
        self.browser.reload();
    }
}

/// The CEF thread's loop: serve requests and pump CEF until `stop_requested`,
/// then close every page and shut CEF down.
pub fn serve_the_cef_thread_until_stopped(
    requests: Receiver<CefThreadRequest>,
    cef_thread_requests: &CefThreadRequestSender,
    root_cache_directory: &Path,
    stop_requested: &AtomicBool,
) {
    let mut open_tabs: HashMap<BrowserHostTabId, OpenTab> = HashMap::new();
    let mut tabs_closing: Vec<Arc<BrowserHostTab>> = Vec::new();
    let mut tenant_profiles: HashMap<String, RequestContext> = HashMap::new();
    let mut next_pump_at = Instant::now();
    while !stop_requested.load(Ordering::Acquire) {
        let wait = next_pump_at
            .saturating_duration_since(Instant::now())
            .min(LONGEST_PUMP_INTERVAL);
        match requests.recv_timeout(wait) {
            Ok(CefThreadRequest::PumpWithin { delay }) => {
                next_pump_at = next_pump_at.min(Instant::now() + delay);
            }
            Ok(CefThreadRequest::OpenTab { tab, opened }) => {
                let outcome = open_a_tab(
                    &tab,
                    cef_thread_requests,
                    root_cache_directory,
                    &mut tenant_profiles,
                )
                .map(|browser| {
                    tab.answer_that_the_tab_opened();
                    open_tabs.insert(
                        tab.tab_id,
                        OpenTab {
                            browser,
                            tab: Arc::clone(&tab),
                            data_scripts_awaiting_the_loaded_document:
                                PageDataScriptsAwaitingTheLoadedDocument::default(),
                            renderer_ends_in_the_window: VecDeque::new(),
                        },
                    );
                });
                let _ = opened.send(outcome);
            }
            Ok(CefThreadRequest::CloseTab { tab_id }) => {
                if let Some(open_tab) = open_tabs.remove(&tab_id) {
                    open_tab.close();
                    tabs_closing.push(Arc::clone(&open_tab.tab));
                }
                tabs_closing.retain(|tab| !tab.page_closed.load(Ordering::Acquire));
            }
            Ok(CefThreadRequest::DeliverPageData {
                tab_id,
                detail_json,
            }) => {
                if let Some(open_tab) = open_tabs.get_mut(&tab_id) {
                    open_tab.tab.page_data_request_dequeued(detail_json.len());
                    open_tab.dispatch_or_hold_data_script(page_data_dispatch_script(&detail_json));
                }
            }
            Ok(CefThreadRequest::ReloadTabAfterItsRendererEnded { tab_id }) => {
                if let Some(open_tab) = open_tabs.get_mut(&tab_id) {
                    open_tab.reload_after_the_renderer_ended();
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        if Instant::now() >= next_pump_at {
            do_message_loop_work();
            next_pump_at = Instant::now() + LONGEST_PUMP_INTERVAL;
        }
        for open_tab in open_tabs.values_mut() {
            open_tab.dispatch_data_scripts_held_for_the_loaded_document();
        }
    }
    close_every_tab_then_shut_cef_down(open_tabs, tabs_closing);
}

/// Close every open page, wait for CEF to let each one go, the ones already
/// closing included, then shut CEF down.
fn close_every_tab_then_shut_cef_down(
    open_tabs: HashMap<BrowserHostTabId, OpenTab>,
    tabs_closing: Vec<Arc<BrowserHostTab>>,
) {
    for open_tab in open_tabs.values() {
        open_tab.close();
    }
    let every_closing_tab: Vec<Arc<BrowserHostTab>> = open_tabs
        .values()
        .map(|open_tab| Arc::clone(&open_tab.tab))
        .chain(tabs_closing)
        .collect();
    let closing_ends = Instant::now() + PAGES_CLOSE_BUDGET_AT_STOP;
    while Instant::now() < closing_ends
        && !every_closing_tab
            .iter()
            .all(|tab| tab.page_closed.load(Ordering::Acquire))
    {
        do_message_loop_work();
        std::thread::sleep(LONGEST_PUMP_INTERVAL);
    }
    let pages_still_open = every_closing_tab
        .iter()
        .filter(|tab| !tab.page_closed.load(Ordering::Acquire))
        .count();
    if pages_still_open > 0 {
        tracing::warn!(
            "{pages_still_open} pages were still open {PAGES_CLOSE_BUDGET_AT_STOP:?} after the \
             host was asked to stop"
        );
    }
    drop(open_tabs);
    shutdown();
}

fn open_a_tab(
    tab: &Arc<BrowserHostTab>,
    cef_thread_requests: &CefThreadRequestSender,
    root_cache_directory: &Path,
    tenant_profiles: &mut HashMap<String, RequestContext>,
) -> Result<Browser, String> {
    let tab_request = &tab.tab_request;
    let mut tenant_profile = tenant_profile_request_context(
        root_cache_directory,
        &tab_request.tenant_stream_id,
        tenant_profiles,
    )?;
    let window_info = WindowInfo {
        windowless_rendering_enabled: 1,
        shared_texture_enabled: i32::from(tab_request.paint_into_shared_textures),
        bounds: Rect {
            x: 0,
            y: 0,
            width: tab_request.width as i32,
            height: tab_request.height as i32,
        },
        ..Default::default()
    };
    let browser_settings = BrowserSettings {
        windowless_frame_rate: tab_request.frame_rate as i32,
        background_color: if tab_request.transparent_background {
            0x0000_0000
        } else {
            0xFFFF_FFFF
        },
        ..Default::default()
    };
    let mut client = browser_host_page_client(tab, cef_thread_requests);
    browser_host_create_browser_sync(
        Some(&window_info),
        Some(&mut client),
        Some(&CefString::from(tab_request.url.as_str())),
        Some(&browser_settings),
        None,
        Some(&mut tenant_profile),
    )
    .ok_or_else(|| format!("CEF refused to open {}", tab_request.url))
}

/// The profile every tab of `tenant_stream_id` shares, made on its first tab.
fn tenant_profile_request_context(
    root_cache_directory: &Path,
    tenant_stream_id: &str,
    tenant_profiles: &mut HashMap<String, RequestContext>,
) -> Result<RequestContext, String> {
    if let Some(profile) = tenant_profiles.get(tenant_stream_id) {
        return Ok(profile.clone());
    }
    let profile_directory: PathBuf =
        tenant_profile_directory(root_cache_directory, tenant_stream_id);
    std::fs::create_dir_all(&profile_directory).map_err(|failure| {
        format!(
            "the profile of stream {tenant_stream_id} could not be created at {}: {failure}",
            profile_directory.display()
        )
    })?;
    let settings = RequestContextSettings {
        cache_path: CefString::from(profile_directory.to_string_lossy().as_ref()),
        persist_session_cookies: 1,
        ..Default::default()
    };
    let profile = request_context_create_context(Some(&settings), None).ok_or_else(|| {
        format!(
            "CEF refused the profile of stream {tenant_stream_id} at {}",
            profile_directory.display()
        )
    })?;
    tenant_profiles.insert(tenant_stream_id.to_string(), profile.clone());
    Ok(profile)
}

wrap_browser_process_handler! {
    struct PumpSchedulingBrowserProcessHandler {
        cef_thread_requests: CefThreadRequestSender,
    }

    impl BrowserProcessHandler {
        fn on_schedule_message_pump_work(&self, delay_ms: i64) {
            self.cef_thread_requests.send(CefThreadRequest::PumpWithin {
                delay: Duration::from_millis(delay_ms.max(0) as u64),
            });
        }

        fn on_before_child_process_launch(&self, command_line: Option<&mut CommandLine>) {
            if let Some(command_line) = command_line {
                command_line.append_switch_with_value(
                    Some(&CefString::from(BROWSER_PROCESS_PID_SWITCH_NAME)),
                    Some(&CefString::from(std::process::id().to_string().as_str())),
                );
            }
        }
    }
}

wrap_app! {
    struct BrowserHostApp {
        cef_thread_requests: CefThreadRequestSender,
    }

    impl App {
        fn on_before_command_line_processing(
            &self,
            _process_type: Option<&CefString>,
            command_line: Option<&mut CommandLine>,
        ) {
            let Some(command_line) = command_line else {
                return;
            };
            for switch in ["no-zygote", "no-sandbox", "disable-extensions"] {
                command_line.append_switch(Some(&CefString::from(switch)));
            }
            command_line.append_switch_with_value(
                Some(&CefString::from("autoplay-policy")),
                Some(&CefString::from("no-user-gesture-required")),
            );
            if std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
                command_line.append_switch_with_value(
                    Some(&CefString::from("ozone-platform")),
                    Some(&CefString::from("headless")),
                );
            }
        }

        fn browser_process_handler(&self) -> Option<BrowserProcessHandler> {
            Some(PumpSchedulingBrowserProcessHandler::new(self.cef_thread_requests.clone()))
        }
    }
}
