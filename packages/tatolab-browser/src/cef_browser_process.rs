// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! CEF's browser process, inside the node's helper: one thread that owns CEF
//! for the life of the process, and the windowless page it renders, whose
//! paints, audio and published objects go out through the module host table.
//!
//! CEF initializes once per process and runs every call on the thread that
//! initialized it, so that thread outlives any one page; a node's start opens
//! a page on it and its stop closes the page.

use std::ffi::CString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, SyncSender};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use cef::rc::Rc as _;
use cef::*;
use tatolab_browser_page_bridge::{PAGE_PUBLISH_PROCESS_MESSAGE_NAME, page_data_dispatch_script};
use tatolab_module_sdk::{
    ModuleHost, ModuleHostHostMemoryPixels, ModuleHostInputPort, ModuleHostLogLevel,
    ModuleHostNodeOutputTextureRequest, ModuleHostOutputPort,
};

use crate::page_audio_blocks::{PageAudioStreamTimeline, interleave_planar_samples};
use crate::page_data_messages::{
    PageDataScriptsAwaitingTheLoadedDocument, events_msgpack_map_of_published_json,
    page_data_detail_json_of_msgpack_map,
};
use crate::video_frame_record::PaintedPageVideoFrame;

/// The target this module's records reach the node's log under.
pub const BROWSER_LOG_TARGET: &str = "tatolab_browser";

/// The binary CEF re-executes for its child processes, beside the CEF runtime.
pub const BROWSER_SUBPROCESS_EXECUTABLE_NAME: &str = "tatolab-browser-subprocess";

const PAINTED_FRAME_POOL_KEY: &str = "painted_page";
const PAINTED_FRAME_TEXTURE_FORMAT: &str = "rgba8_unorm";
const PAINTED_FRAME_TEXTURE_USAGE: [&str; 2] = ["texture_binding", "storage_binding"];
/// CEF hands `OnPaint` BGRA rows, top first, packed.
const PAINTED_PAGE_PIXEL_FORMAT: &str = "bgra32";

/// The longest the CEF thread sleeps when CEF asked for no pump sooner.
const LONGEST_PUMP_INTERVAL: Duration = Duration::from_millis(8);
const PAGE_CLOSE_TIMEOUT: Duration = Duration::from_secs(5);
const DATA_INPUT_WAIT: Duration = Duration::from_millis(50);
const PAINTS_BETWEEN_REPORTS: u64 = 600;

/// What a page is opened with.
#[derive(Debug, Clone)]
pub struct BrowserPageConfig {
    pub url: String,
    pub width: u32,
    pub height: u32,
    pub frame_rate: u32,
    pub transparent_background: bool,
    pub audio_sample_rate: u32,
    pub output_pool_rotation_depth: u32,
}

/// The ports one page publishes on and reads from, opened from the lent table.
pub struct BrowserPagePorts {
    pub module_host: ModuleHost,
    pub video_output_port: ModuleHostOutputPort,
    pub events_output_port: ModuleHostOutputPort,
    pub audio_output_port: ModuleHostOutputPort,
    pub data_input_port: ModuleHostInputPort,
}

/// How much a page has published and dropped since it opened.
#[derive(Debug, Default)]
pub struct BrowserPageCounters {
    pub frames_published: AtomicU64,
    pub frames_dropped: AtomicU64,
    pub audio_blocks_published: AtomicU64,
    pub events_published: AtomicU64,
    pub data_messages_delivered: AtomicU64,
    pub data_messages_refused: AtomicU64,
}

/// Everything the CEF thread's handlers publish through, shared between them.
struct PagePublishing {
    config: BrowserPageConfig,
    module_host: ModuleHost,
    video_output_port: ModuleHostOutputPort,
    events_output_port: ModuleHostOutputPort,
    audio_output_port: ModuleHostOutputPort,
    counters: Arc<BrowserPageCounters>,
    audio_stream: Mutex<Option<PageAudioStreamTimeline>>,
    interleaved_audio: Mutex<Vec<f32>>,
    first_frame_refusal_logged: AtomicBool,
    publishing_stopped: AtomicBool,
    main_frame_document_loaded: AtomicBool,
    page_closed: Mutex<Option<SyncSender<()>>>,
}

impl PagePublishing {
    fn log(&self, level: ModuleHostLogLevel, message: &str) {
        self.module_host.log(level, BROWSER_LOG_TARGET, message);
    }

    fn publish_painted_frame(&self, painted_bgra_rows: &[u8], width: u32, height: u32) {
        if self.publishing_stopped.load(Ordering::Acquire) {
            return;
        }
        match self.write_painted_frame(painted_bgra_rows, width, height) {
            Ok(()) => {
                let published = self
                    .counters
                    .frames_published
                    .fetch_add(1, Ordering::Relaxed)
                    + 1;
                if published == 1 {
                    self.log(ModuleHostLogLevel::Info, "first frame published");
                }
                if published.is_multiple_of(PAINTS_BETWEEN_REPORTS) {
                    self.log(
                        ModuleHostLogLevel::Info,
                        &format!(
                            "{published} frames published, {} dropped",
                            self.counters.frames_dropped.load(Ordering::Relaxed)
                        ),
                    );
                }
            }
            Err(refusal) => {
                self.counters.frames_dropped.fetch_add(1, Ordering::Relaxed);
                if !self
                    .first_frame_refusal_logged
                    .swap(true, Ordering::Relaxed)
                {
                    self.log(
                        ModuleHostLogLevel::Warn,
                        &format!("dropping a painted frame: {refusal}"),
                    );
                }
            }
        }
    }

    fn write_painted_frame(
        &self,
        painted_bgra_rows: &[u8],
        width: u32,
        height: u32,
    ) -> Result<(), String> {
        let painted_frame_texture = self
            .module_host
            .acquire_texture_from_node_output_pool(&ModuleHostNodeOutputTextureRequest {
                pool_key: PAINTED_FRAME_POOL_KEY,
                rotation_depth: self.config.output_pool_rotation_depth,
                width,
                height,
                texture_format: PAINTED_FRAME_TEXTURE_FORMAT,
                usage: &PAINTED_FRAME_TEXTURE_USAGE,
            })
            .map_err(|refusal| refusal.to_string())?;
        self.module_host
            .write_surface_from_host_memory(
                &painted_frame_texture,
                &ModuleHostHostMemoryPixels {
                    pixels: painted_bgra_rows,
                    pixel_format: PAINTED_PAGE_PIXEL_FORMAT,
                    width,
                    height,
                    row_stride_bytes: u64::from(width) * 4,
                },
            )
            .map_err(|refusal| refusal.to_string())?;
        let timestamp_ns = self.module_host.monotonic_now_ns();
        let record = PaintedPageVideoFrame {
            surface_id: painted_frame_texture.surface_id(),
            width,
            height,
            timestamp_ns,
            frames_per_second: self.config.frame_rate,
        }
        .to_msgpack();
        self.video_output_port
            .write(&record, Some(timestamp_ns), true)
            .map_err(|refusal| refusal.to_string())
    }

    fn publish_page_object(&self, published_json: &str) {
        if self.publishing_stopped.load(Ordering::Acquire) {
            return;
        }
        let written = events_msgpack_map_of_published_json(published_json).and_then(|payload| {
            self.events_output_port
                .write(&payload, None, true)
                .map_err(|refusal| refusal.to_string())
        });
        match written {
            Ok(()) => {
                self.counters
                    .events_published
                    .fetch_add(1, Ordering::Relaxed);
            }
            Err(refusal) => self.log(
                ModuleHostLogLevel::Warn,
                &format!("a published object was not written to events: {refusal}"),
            ),
        }
    }

    fn open_audio_stream(&self, sample_rate: u32, channels: u32) {
        *self
            .audio_stream
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) =
            Some(PageAudioStreamTimeline::new(sample_rate, channels));
        self.log(
            ModuleHostLogLevel::Info,
            &format!("the page opened an audio stream: {sample_rate} Hz × {channels} ch"),
        );
    }

    fn close_audio_stream(&self) {
        self.audio_stream
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
    }

    /// Publish one planar packet; `channel_planes` hold `sample_count` samples each.
    fn publish_audio_packet(&self, channel_planes: &[&[f32]], sample_count: u32) {
        if self.publishing_stopped.load(Ordering::Acquire) {
            return;
        }
        let arrived_at_ns = self.module_host.monotonic_now_ns();
        let mut audio_stream = self
            .audio_stream
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(timeline) = audio_stream.as_mut() else {
            return;
        };
        let first_sample_timestamp_ns = timeline.stamp_the_next_packet(sample_count, arrived_at_ns);
        let mut interleaved = self
            .interleaved_audio
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        interleave_planar_samples(channel_planes, sample_count as usize, &mut interleaved);
        let payload = timeline.audio_block_message_payload(
            &interleaved,
            sample_count,
            first_sample_timestamp_ns,
        );
        match self
            .audio_output_port
            .write(&payload, Some(first_sample_timestamp_ns), true)
        {
            Ok(()) => {
                self.counters
                    .audio_blocks_published
                    .fetch_add(1, Ordering::Relaxed);
            }
            Err(refusal) => self.log(
                ModuleHostLogLevel::Warn,
                &format!("an audio block was not written: {refusal}"),
            ),
        }
    }

    fn report_page_closed(&self) {
        if let Some(page_closed) = self
            .page_closed
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
        {
            let _ = page_closed.send(());
        }
    }
}

/// One request the CEF thread serves between pumps.
enum CefThreadRequest {
    OpenPage {
        publishing: Arc<PagePublishing>,
        opened: SyncSender<Result<(), String>>,
    },
    ClosePage,
    DispatchPageData {
        script: String,
    },
    PumpWithin {
        delay: Duration,
    },
}

/// The process's CEF thread: running once CEF initialized, and for the rest of
/// the process.
struct CefBrowserProcessThread {
    requests: Mutex<Sender<CefThreadRequest>>,
}

impl CefBrowserProcessThread {
    fn send(&self, request: CefThreadRequest) -> Result<(), String> {
        self.requests
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .send(request)
            .map_err(|_| "the CEF thread of this helper has ended".to_string())
    }
}

static CEF_BROWSER_PROCESS_THREAD: OnceLock<Result<CefBrowserProcessThread, String>> =
    OnceLock::new();

/// The CEF thread, started and CEF initialized from `cef_runtime_directory` on
/// first ask; a later ask reuses it whatever directory it names.
fn cef_browser_process_thread(
    cef_runtime_directory: &Path,
) -> Result<&'static CefBrowserProcessThread, String> {
    CEF_BROWSER_PROCESS_THREAD
        .get_or_init(|| start_the_cef_browser_process_thread(cef_runtime_directory.to_path_buf()))
        .as_ref()
        .map_err(Clone::clone)
}

fn start_the_cef_browser_process_thread(
    cef_runtime_directory: PathBuf,
) -> Result<CefBrowserProcessThread, String> {
    let (requests, requests_to_serve) = std::sync::mpsc::channel();
    let (initialized, initialization_outcome) = std::sync::mpsc::sync_channel(1);
    let pump_requests = requests.clone();
    std::thread::Builder::new()
        .name("tatolab-browser-cef".to_string())
        .spawn(
            move || match initialize_cef(&cef_runtime_directory, pump_requests) {
                Ok(()) => {
                    let _ = initialized.send(Ok(()));
                    serve_the_cef_thread(requests_to_serve);
                }
                Err(refusal) => {
                    let _ = initialized.send(Err(refusal));
                }
            },
        )
        .map_err(|spawn_failure| format!("the CEF thread did not start: {spawn_failure}"))?;
    initialization_outcome
        .recv()
        .map_err(|_| "the CEF thread ended before CEF initialized".to_string())??;
    Ok(CefBrowserProcessThread {
        requests: Mutex::new(requests),
    })
}

fn initialize_cef(
    cef_runtime_directory: &Path,
    pump_requests: Sender<CefThreadRequest>,
) -> Result<(), String> {
    let subprocess_path = cef_runtime_directory.join(BROWSER_SUBPROCESS_EXECUTABLE_NAME);
    if !subprocess_path.is_file() {
        return Err(format!(
            "the CEF runtime at {} holds no {BROWSER_SUBPROCESS_EXECUTABLE_NAME}; the wheel \
             ships it beside libcef",
            cef_runtime_directory.display()
        ));
    }
    let _ = api_hash(sys::CEF_API_VERSION_LAST, 0);
    remove_cef_cache_directories_whose_helper_has_exited(&std::env::temp_dir());
    let root_cache_directory = create_an_owner_only_cef_cache_directory()?;
    let path_text = |path: &Path| CefString::from(path.to_string_lossy().as_ref());
    let settings = Settings {
        no_sandbox: 1,
        browser_subprocess_path: path_text(&subprocess_path),
        resources_dir_path: path_text(cef_runtime_directory),
        locales_dir_path: path_text(&cef_runtime_directory.join("locales")),
        root_cache_path: path_text(&root_cache_directory),
        log_file: path_text(&root_cache_directory.join("cef.log")),
        log_severity: LogSeverity::WARNING,
        windowless_rendering_enabled: 1,
        external_message_pump: 1,
        command_line_args_disabled: 1,
        ..Default::default()
    };
    // CEF reads its switches from the app below, never from the helper's own
    // argv, which is Python's.
    let program_name = CString::new("tatolab-browser").expect("no interior NUL");
    let mut argv = [program_name.as_ptr().cast_mut()];
    let main_args = MainArgs {
        argc: 1,
        argv: argv.as_mut_ptr(),
    };
    let mut app = BrowserProcessApp::new(Arc::new(Mutex::new(pump_requests)));
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

/// What every CEF cache directory's name starts with.
const CEF_CACHE_DIRECTORY_NAME_PREFIX: &str = "tatolab-browser-";

/// The file in a CEF cache directory naming the helper that holds it.
const CEF_CACHE_DIRECTORY_OWNER_PID_FILE_NAME: &str = "helper.pid";

/// Remove the CEF cache directories under `temporary_directory` whose helper
/// is gone. CEF holds its cache until the helper exits, often by SIGKILL, so
/// the next helper to start removes what earlier ones left.
fn remove_cef_cache_directories_whose_helper_has_exited(temporary_directory: &Path) {
    let Ok(entries) = std::fs::read_dir(temporary_directory) else {
        return;
    };
    for entry in entries.flatten() {
        if !entry
            .file_name()
            .to_string_lossy()
            .starts_with(CEF_CACHE_DIRECTORY_NAME_PREFIX)
        {
            continue;
        }
        let cache_directory = entry.path();
        let owner_pid =
            std::fs::read_to_string(cache_directory.join(CEF_CACHE_DIRECTORY_OWNER_PID_FILE_NAME))
                .ok()
                .and_then(|text| text.trim().parse::<libc::pid_t>().ok());
        if owner_pid.is_some_and(|pid| !process_is_alive(pid)) {
            let _ = std::fs::remove_dir_all(&cache_directory);
        }
    }
}

fn process_is_alive(pid: libc::pid_t) -> bool {
    // SAFETY: signal 0 sends nothing; it only asks whether `pid` exists.
    let answer = unsafe { libc::kill(pid, 0) };
    answer == 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

/// A fresh directory only this user can open, named unpredictably, under the
/// system temporary directory, naming this helper as its holder. CEF holds it
/// for the helper's whole life.
fn create_an_owner_only_cef_cache_directory() -> Result<PathBuf, String> {
    let template = std::env::temp_dir().join(format!("{CEF_CACHE_DIRECTORY_NAME_PREFIX}XXXXXX"));
    let mut template_bytes = CString::new(template.as_os_str().as_encoded_bytes())
        .map_err(|_| format!("{} holds a NUL", template.display()))?
        .into_bytes_with_nul();
    // SAFETY: mkdtemp rewrites the trailing XXXXXX of a NUL-terminated buffer
    // it is handed exclusively, and creates the directory with mode 0700.
    let created = unsafe { libc::mkdtemp(template_bytes.as_mut_ptr().cast()) };
    if created.is_null() {
        return Err(format!(
            "no CEF cache directory could be created from {}: {}",
            template.display(),
            std::io::Error::last_os_error()
        ));
    }
    template_bytes.pop();
    let cache_directory = PathBuf::from(std::ffi::OsString::from(
        String::from_utf8_lossy(&template_bytes).into_owned(),
    ));
    std::fs::write(
        cache_directory.join(CEF_CACHE_DIRECTORY_OWNER_PID_FILE_NAME),
        std::process::id().to_string(),
    )
    .map_err(|write_failure| {
        format!(
            "{} could not name its helper: {write_failure}",
            cache_directory.display()
        )
    })?;
    Ok(cache_directory)
}

/// The page the CEF thread renders, while one is open.
struct OpenPage {
    browser: Browser,
    publishing: Arc<PagePublishing>,
    data_scripts_awaiting_the_loaded_document: PageDataScriptsAwaitingTheLoadedDocument,
}

impl OpenPage {
    fn main_frame_document_loaded(&self) -> bool {
        self.publishing
            .main_frame_document_loaded
            .load(Ordering::Acquire)
    }

    /// Run `script` in the main frame's document, or hold it until that
    /// document has loaded, so the page's `tatolab:data` listeners exist.
    fn dispatch_or_hold_data_script(&mut self, script: String) {
        if self.main_frame_document_loaded() {
            self.dispatch_data_script(&script);
        } else if self.data_scripts_awaiting_the_loaded_document.hold(script) {
            self.publishing
                .counters
                .data_messages_refused
                .fetch_add(1, Ordering::Relaxed);
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
        self.publishing
            .counters
            .data_messages_delivered
            .fetch_add(1, Ordering::Relaxed);
    }
}

fn serve_the_cef_thread(requests: Receiver<CefThreadRequest>) {
    let mut open_page: Option<OpenPage> = None;
    let mut next_pump_at = Instant::now();
    loop {
        let wait = next_pump_at.saturating_duration_since(Instant::now());
        match requests.recv_timeout(wait) {
            Ok(CefThreadRequest::PumpWithin { delay }) => {
                next_pump_at = next_pump_at.min(Instant::now() + delay);
                continue;
            }
            Ok(CefThreadRequest::OpenPage { publishing, opened }) => {
                let outcome = if open_page.is_some() {
                    Err("this helper already renders a page; a browser node opens one".to_string())
                } else {
                    open_a_page(publishing).map(|page| {
                        open_page = Some(page);
                    })
                };
                let _ = opened.send(outcome);
            }
            Ok(CefThreadRequest::ClosePage) => {
                if let Some(page) = open_page.take()
                    && let Some(host) = page.browser.host()
                {
                    host.close_browser(1);
                }
            }
            Ok(CefThreadRequest::DispatchPageData { script }) => {
                if let Some(page) = open_page.as_mut() {
                    page.dispatch_or_hold_data_script(script);
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        if Instant::now() >= next_pump_at {
            do_message_loop_work();
            next_pump_at = Instant::now() + LONGEST_PUMP_INTERVAL;
        }
        if let Some(page) = open_page.as_mut() {
            page.dispatch_data_scripts_held_for_the_loaded_document();
        }
    }
}

fn open_a_page(publishing: Arc<PagePublishing>) -> Result<OpenPage, String> {
    let config = publishing.config.clone();
    let window_info = WindowInfo {
        windowless_rendering_enabled: 1,
        shared_texture_enabled: 0,
        bounds: Rect {
            x: 0,
            y: 0,
            width: config.width as i32,
            height: config.height as i32,
        },
        ..Default::default()
    };
    let browser_settings = BrowserSettings {
        windowless_frame_rate: config.frame_rate as i32,
        background_color: if config.transparent_background {
            0x0000_0000
        } else {
            0xFFFF_FFFF
        },
        ..Default::default()
    };
    let mut client = PageClient::new(
        PageRenderHandler::new(Arc::clone(&publishing)),
        PageAudioHandler::new(Arc::clone(&publishing)),
        PageLifeSpanHandler::new(Arc::clone(&publishing)),
        PageLoadHandler::new(Arc::clone(&publishing)),
        Arc::clone(&publishing),
    );
    let browser = browser_host_create_browser_sync(
        Some(&window_info),
        Some(&mut client),
        Some(&CefString::from(config.url.as_str())),
        Some(&browser_settings),
        None,
        None,
    )
    .ok_or_else(|| format!("CEF refused to open {}", config.url))?;
    Ok(OpenPage {
        browser,
        publishing,
        data_scripts_awaiting_the_loaded_document:
            PageDataScriptsAwaitingTheLoadedDocument::default(),
    })
}

wrap_browser_process_handler! {
    struct PumpSchedulingBrowserProcessHandler {
        pump_requests: Arc<Mutex<Sender<CefThreadRequest>>>,
    }

    impl BrowserProcessHandler {
        fn on_schedule_message_pump_work(&self, delay_ms: i64) {
            let _ = self
                .pump_requests
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .send(CefThreadRequest::PumpWithin {
                    delay: Duration::from_millis(delay_ms.max(0) as u64),
                });
        }
    }
}

wrap_app! {
    struct BrowserProcessApp {
        pump_requests: Arc<Mutex<Sender<CefThreadRequest>>>,
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
            Some(PumpSchedulingBrowserProcessHandler::new(Arc::clone(&self.pump_requests)))
        }
    }
}

wrap_render_handler! {
    struct PageRenderHandler {
        publishing: Arc<PagePublishing>,
    }

    impl RenderHandler {
        fn view_rect(&self, _browser: Option<&mut Browser>, rect: Option<&mut Rect>) {
            if let Some(rect) = rect {
                *rect = Rect {
                    x: 0,
                    y: 0,
                    width: self.publishing.config.width as i32,
                    height: self.publishing.config.height as i32,
                };
            }
        }

        fn on_paint(
            &self,
            _browser: Option<&mut Browser>,
            type_: PaintElementType,
            _dirty_rects: Option<&[Rect]>,
            buffer: *const u8,
            width: ::std::os::raw::c_int,
            height: ::std::os::raw::c_int,
        ) {
            if type_ != PaintElementType::VIEW || buffer.is_null() || width <= 0 || height <= 0 {
                return;
            }
            let (width, height) = (width as u32, height as u32);
            // SAFETY: CEF hands `width × height` packed BGRA pixels, alive for
            // the length of this call.
            let painted_bgra_rows = unsafe {
                std::slice::from_raw_parts(buffer, width as usize * height as usize * 4)
            };
            self.publishing.publish_painted_frame(painted_bgra_rows, width, height);
        }
    }
}

wrap_audio_handler! {
    struct PageAudioHandler {
        publishing: Arc<PagePublishing>,
    }

    impl AudioHandler {
        fn audio_parameters(
            &self,
            _browser: Option<&mut Browser>,
            params: Option<&mut AudioParameters>,
        ) -> ::std::os::raw::c_int {
            let Some(params) = params else {
                return 0;
            };
            let sample_rate = self.publishing.config.audio_sample_rate;
            params.sample_rate = sample_rate as i32;
            params.channel_layout = ChannelLayout::LAYOUT_STEREO;
            params.frames_per_buffer = (sample_rate / 100).max(1) as i32;
            1
        }

        fn on_audio_stream_started(
            &self,
            _browser: Option<&mut Browser>,
            params: Option<&AudioParameters>,
            channels: ::std::os::raw::c_int,
        ) {
            let sample_rate = params
                .map(|params| params.sample_rate.max(1) as u32)
                .unwrap_or(self.publishing.config.audio_sample_rate);
            self.publishing.open_audio_stream(sample_rate, channels.max(1) as u32);
        }

        fn on_audio_stream_packet(
            &self,
            _browser: Option<&mut Browser>,
            data: *mut *const f32,
            frames: ::std::os::raw::c_int,
            _pts: i64,
        ) {
            if data.is_null() || frames <= 0 {
                return;
            }
            let channels = match self
                .publishing
                .audio_stream
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .as_ref()
            {
                Some(timeline) => timeline.channels() as usize,
                None => return,
            };
            // SAFETY: CEF hands one plane pointer per channel of the stream
            // it started, each `frames` samples long, alive for this call.
            let channel_planes: Vec<&[f32]> = (0..channels)
                .map(|channel| unsafe {
                    std::slice::from_raw_parts(*data.add(channel), frames as usize)
                })
                .collect();
            self.publishing.publish_audio_packet(&channel_planes, frames as u32);
        }

        fn on_audio_stream_stopped(&self, _browser: Option<&mut Browser>) {
            self.publishing.close_audio_stream();
        }

        fn on_audio_stream_error(&self, _browser: Option<&mut Browser>, message: Option<&CefString>) {
            self.publishing.close_audio_stream();
            self.publishing.log(
                ModuleHostLogLevel::Warn,
                &format!(
                    "the page's audio stream failed: {}",
                    message.map(ToString::to_string).unwrap_or_default()
                ),
            );
        }
    }
}

wrap_life_span_handler! {
    struct PageLifeSpanHandler {
        publishing: Arc<PagePublishing>,
    }

    impl LifeSpanHandler {
        fn on_before_close(&self, _browser: Option<&mut Browser>) {
            self.publishing.report_page_closed();
        }
    }
}

wrap_load_handler! {
    struct PageLoadHandler {
        publishing: Arc<PagePublishing>,
    }

    impl LoadHandler {
        fn on_load_start(
            &self,
            _browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            _transition_type: TransitionType,
        ) {
            if frame.is_some_and(|frame| frame.is_main() != 0) {
                self.publishing
                    .main_frame_document_loaded
                    .store(false, Ordering::Release);
            }
        }

        fn on_load_end(
            &self,
            _browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            _http_status_code: ::std::os::raw::c_int,
        ) {
            if frame.is_some_and(|frame| frame.is_main() != 0) {
                self.publishing
                    .main_frame_document_loaded
                    .store(true, Ordering::Release);
            }
        }
    }
}

wrap_client! {
    struct PageClient {
        render_handler: RenderHandler,
        audio_handler: AudioHandler,
        life_span_handler: LifeSpanHandler,
        load_handler: LoadHandler,
        publishing: Arc<PagePublishing>,
    }

    impl Client {
        fn render_handler(&self) -> Option<RenderHandler> {
            Some(self.render_handler.clone())
        }

        fn audio_handler(&self) -> Option<AudioHandler> {
            Some(self.audio_handler.clone())
        }

        fn life_span_handler(&self) -> Option<LifeSpanHandler> {
            Some(self.life_span_handler.clone())
        }

        fn load_handler(&self) -> Option<LoadHandler> {
            Some(self.load_handler.clone())
        }

        fn on_process_message_received(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            _source_process: ProcessId,
            message: Option<&mut ProcessMessage>,
        ) -> ::std::os::raw::c_int {
            let Some(message) = message else {
                return 0;
            };
            if CefString::from(&message.name()).to_string() != PAGE_PUBLISH_PROCESS_MESSAGE_NAME {
                return 0;
            }
            if let Some(arguments) = message.argument_list() {
                let published_json = CefString::from(&arguments.string(0)).to_string();
                self.publishing.publish_page_object(&published_json);
            }
            1
        }
    }
}

/// A page rendering for one node: open from `open` until `close`.
pub struct BrowserPageRendering {
    cef_thread: &'static CefBrowserProcessThread,
    publishing: Arc<PagePublishing>,
    counters: Arc<BrowserPageCounters>,
    data_input_stop: Arc<AtomicBool>,
    data_input_thread: Option<JoinHandle<()>>,
    page_closed: Mutex<Option<Receiver<()>>>,
}

impl BrowserPageRendering {
    /// Initialize CEF from `cef_runtime_directory` if this helper has not, open
    /// `config.url` offscreen, and start delivering `data` to it.
    pub fn open(
        cef_runtime_directory: &Path,
        config: BrowserPageConfig,
        ports: BrowserPagePorts,
    ) -> Result<Self, String> {
        refuse_a_config_cef_cannot_render(&config)?;
        let cef_thread = cef_browser_process_thread(cef_runtime_directory)?;
        let counters = Arc::new(BrowserPageCounters::default());
        let (page_closed_sender, page_closed) = std::sync::mpsc::sync_channel(1);
        let publishing = Arc::new(PagePublishing {
            config,
            module_host: ports.module_host.clone(),
            video_output_port: ports.video_output_port,
            events_output_port: ports.events_output_port,
            audio_output_port: ports.audio_output_port,
            counters: Arc::clone(&counters),
            audio_stream: Mutex::new(None),
            interleaved_audio: Mutex::new(Vec::new()),
            first_frame_refusal_logged: AtomicBool::new(false),
            publishing_stopped: AtomicBool::new(false),
            main_frame_document_loaded: AtomicBool::new(false),
            page_closed: Mutex::new(Some(page_closed_sender)),
        });
        let (opened, open_outcome) = std::sync::mpsc::sync_channel(1);
        cef_thread.send(CefThreadRequest::OpenPage {
            publishing: Arc::clone(&publishing),
            opened,
        })?;
        open_outcome
            .recv()
            .map_err(|_| "the CEF thread ended before it opened the page".to_string())??;

        let data_input_stop = Arc::new(AtomicBool::new(false));
        let data_input_thread = std::thread::Builder::new()
            .name("tatolab-browser-data".to_string())
            .spawn({
                let data_input_stop = Arc::clone(&data_input_stop);
                let counters = Arc::clone(&counters);
                let module_host = ports.module_host;
                let data_input_port = ports.data_input_port;
                move || {
                    deliver_data_messages_to_the_page(
                        &module_host,
                        &data_input_port,
                        cef_thread,
                        &counters,
                        &data_input_stop,
                    )
                }
            })
            .map_err(|spawn_failure| format!("the data thread did not start: {spawn_failure}"))?;
        Ok(Self {
            cef_thread,
            publishing,
            counters,
            data_input_stop,
            data_input_thread: Some(data_input_thread),
            page_closed: Mutex::new(Some(page_closed)),
        })
    }

    pub fn counters(&self) -> &BrowserPageCounters {
        &self.counters
    }

    /// Stop publishing and delivering data, then close the page, waiting for
    /// CEF to let it go. Idempotent.
    pub fn close(&mut self) {
        self.publishing
            .publishing_stopped
            .store(true, Ordering::Release);
        self.data_input_stop.store(true, Ordering::Release);
        if let Some(data_input_thread) = self.data_input_thread.take() {
            let _ = data_input_thread.join();
        }
        let Some(page_closed) = self
            .page_closed
            .get_mut()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
        else {
            return;
        };
        if self.cef_thread.send(CefThreadRequest::ClosePage).is_ok()
            && page_closed.recv_timeout(PAGE_CLOSE_TIMEOUT).is_err()
        {
            self.publishing.log(
                ModuleHostLogLevel::Warn,
                &format!(
                    "CEF did not close the page within {PAGE_CLOSE_TIMEOUT:?}; it is abandoned"
                ),
            );
        }
        self.publishing.close_audio_stream();
    }
}

impl Drop for BrowserPageRendering {
    fn drop(&mut self) {
        self.close();
    }
}

/// Refuse what CEF's windowless renderer cannot paint before CEF is touched.
pub fn refuse_a_config_cef_cannot_render(config: &BrowserPageConfig) -> Result<(), String> {
    if config.url.trim().is_empty() {
        return Err("a browser source needs a url".to_string());
    }
    if config.width == 0 || config.height == 0 || config.width > 8192 || config.height > 8192 {
        return Err(format!(
            "a page renders at 1x1 to 8192x8192, not {}x{}",
            config.width, config.height
        ));
    }
    if !(1..=60).contains(&config.frame_rate) {
        return Err(format!(
            "CEF paints a windowless page at 1 to 60 frames a second, not {}",
            config.frame_rate
        ));
    }
    if !(8_000..=192_000).contains(&config.audio_sample_rate) {
        return Err(format!(
            "audio_sample_rate is 8000 to 192000 Hz, not {}",
            config.audio_sample_rate
        ));
    }
    if config.output_pool_rotation_depth == 0 {
        return Err("output_pool_rotation_depth is at least 1".to_string());
    }
    Ok(())
}

fn deliver_data_messages_to_the_page(
    module_host: &ModuleHost,
    data_input_port: &ModuleHostInputPort,
    cef_thread: &CefBrowserProcessThread,
    counters: &BrowserPageCounters,
    stop: &AtomicBool,
) {
    while !stop.load(Ordering::Acquire) && !module_host.stop_requested() {
        match module_host.wait_for_any_input_port(DATA_INPUT_WAIT) {
            Ok(false) => continue,
            Ok(true) => {}
            Err(refusal) => {
                module_host.log(
                    ModuleHostLogLevel::Error,
                    BROWSER_LOG_TARGET,
                    &format!("data stopped reaching the page: {refusal}"),
                );
                return;
            }
        }
        loop {
            let message = match data_input_port.read() {
                Ok(Some(message)) => message,
                Ok(None) => break,
                Err(refusal) => {
                    module_host.log(
                        ModuleHostLogLevel::Error,
                        BROWSER_LOG_TARGET,
                        &format!("data stopped reaching the page: {refusal}"),
                    );
                    return;
                }
            };
            match page_data_detail_json_of_msgpack_map(message.payload()) {
                Ok(detail_json) => {
                    let script = page_data_dispatch_script(&detail_json);
                    if cef_thread
                        .send(CefThreadRequest::DispatchPageData { script })
                        .is_err()
                    {
                        return;
                    }
                }
                Err(refusal) => {
                    if counters
                        .data_messages_refused
                        .fetch_add(1, Ordering::Relaxed)
                        == 0
                    {
                        module_host.log(
                            ModuleHostLogLevel::Warn,
                            BROWSER_LOG_TARGET,
                            &format!("a data message did not reach the page: {refusal}"),
                        );
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cache_directory_held_by(temporary_directory: &Path, name: &str, pid: u32) -> PathBuf {
        let cache_directory = temporary_directory.join(name);
        std::fs::create_dir(&cache_directory).unwrap();
        std::fs::write(
            cache_directory.join(CEF_CACHE_DIRECTORY_OWNER_PID_FILE_NAME),
            pid.to_string(),
        )
        .unwrap();
        cache_directory
    }

    #[test]
    fn only_the_cache_of_an_exited_helper_is_removed() {
        let temporary_directory =
            std::env::temp_dir().join(format!("cef-cache-sweep-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temporary_directory);
        std::fs::create_dir(&temporary_directory).unwrap();
        let mut exited_child = std::process::Command::new("true").spawn().unwrap();
        let exited_pid = exited_child.id();
        exited_child.wait().unwrap();
        let exited_helpers_cache =
            cache_directory_held_by(&temporary_directory, "tatolab-browser-gone00", exited_pid);
        let live_helpers_cache = cache_directory_held_by(
            &temporary_directory,
            "tatolab-browser-alive0",
            std::process::id(),
        );
        let unnamed_cache = temporary_directory.join("tatolab-browser-unnamed");
        std::fs::create_dir(&unnamed_cache).unwrap();

        remove_cef_cache_directories_whose_helper_has_exited(&temporary_directory);

        assert!(!exited_helpers_cache.exists());
        assert!(live_helpers_cache.exists());
        assert!(unnamed_cache.exists());
        std::fs::remove_dir_all(&temporary_directory).unwrap();
    }

    fn config() -> BrowserPageConfig {
        BrowserPageConfig {
            url: "https://example.com".to_string(),
            width: 1920,
            height: 1080,
            frame_rate: 60,
            transparent_background: true,
            audio_sample_rate: 48_000,
            output_pool_rotation_depth: 4,
        }
    }

    #[test]
    fn a_config_cef_can_render_passes() {
        refuse_a_config_cef_cannot_render(&config()).expect("1080p60 renders");
    }

    #[test]
    fn what_cef_cannot_render_is_refused_by_name() {
        for (broken, named) in [
            (
                BrowserPageConfig {
                    url: " ".into(),
                    ..config()
                },
                "needs a url",
            ),
            (
                BrowserPageConfig {
                    width: 0,
                    ..config()
                },
                "0x1080",
            ),
            (
                BrowserPageConfig {
                    frame_rate: 120,
                    ..config()
                },
                "not 120",
            ),
            (
                BrowserPageConfig {
                    audio_sample_rate: 4_000,
                    ..config()
                },
                "not 4000",
            ),
            (
                BrowserPageConfig {
                    output_pool_rotation_depth: 0,
                    ..config()
                },
                "at least 1",
            ),
        ] {
            let refusal = refuse_a_config_cef_cannot_render(&broken).expect_err(named);
            assert!(refusal.contains(named), "{refusal}");
        }
    }
}
