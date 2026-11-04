// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The renderer half of `tatolab-browser`: the `window.tatolab` bridge every
//! page is given before its own scripts run, and the CEF application its child
//! processes run under.
//!
//! A page calls `window.tatolab.publish(object)`; the bridge sends the object as
//! JSON to the browser process in one process message, which the node publishes
//! as one msgpack map on `events`.

use cef::rc::Rc as _;
use cef::*;

/// The process message the renderer sends each published object in, as one
/// JSON string argument.
pub const PAGE_PUBLISH_PROCESS_MESSAGE_NAME: &str = "tatolab.publish";

/// The DOM event a `data` message reaches the page as.
pub const PAGE_DATA_DOM_EVENT_NAME: &str = "tatolab:data";

/// The global the bootstrap reads the native publish function from, then
/// deletes.
const NATIVE_PUBLISH_JSON_FUNCTION_NAME: &str = "__tatolabPublishJson";

/// The process message one page input chunk reaches the renderer in, its
/// [`PageInputChunk`] in the message's shared memory.
pub const PAGE_INPUT_PROCESS_MESSAGE_NAME: &str = "tatolab.page_input";

/// The key of the browser's extra info the page's wired inputs reach its
/// renderer under, as JSON text: `[{"input_port_name", "kind"}]`.
pub const PAGE_INPUT_TRACKS_EXTRA_INFO_KEY: &str = "tatolab_page_input_tracks";

/// The global the bootstrap leaves the page-input deliverer on, for the
/// renderer to call.
const PAGE_INPUT_DELIVERER_FUNCTION_NAME: &str = "__tatolabDeliverPageInput";

/// The text the bootstrap's wired inputs are spliced in at.
const PAGE_INPUT_TRACKS_PLACEHOLDER: &str = "__TATOLAB_PAGE_INPUT_TRACKS_JSON__";

/// Runs in every frame's context before the page's own scripts, the page's
/// wired inputs spliced in.
const PAGE_BRIDGE_BOOTSTRAP_SCRIPT: &str = r#"(() => {
  const publishJson = window.__tatolabPublishJson;
  delete window.__tatolabPublishJson;
  const publish = (object) => {
    if (typeof object !== "object" || object === null || Array.isArray(object)) {
      throw new TypeError("tatolab.publish takes one plain object, published as one map");
    }
    const json = JSON.stringify(object);
    if (typeof json !== "string" || json[0] !== "{") {
      throw new TypeError("tatolab.publish takes one plain object, published as one map");
    }
    publishJson(json);
  };
  const wiredInputs = new Map(
    JSON.parse(__TATOLAB_PAGE_INPUT_TRACKS_JSON__).map((track) => [track.input_port_name, track.kind])
  );
  const MOST_CHUNKS_WAITING_BY_KIND = { video: 2, audio: 32 };
  const openInputs = new Map();
  const inputTrack = (inputPortName) => {
    const kind = wiredInputs.get(inputPortName);
    if (kind === undefined || typeof MediaStreamTrackGenerator !== "function") {
      return null;
    }
    let openInput = openInputs.get(inputPortName);
    if (openInput === undefined) {
      const generator = new MediaStreamTrackGenerator({ kind });
      openInput = { generator, writer: generator.writable.getWriter(), chunksWaiting: 0 };
      openInputs.set(inputPortName, openInput);
    }
    return openInput.generator;
  };
  const deliverPageInput = (kind, inputPortName, widthOrSampleRate, heightOrChannelCount, timestampMicroseconds, payload) => {
    const openInput = openInputs.get(inputPortName);
    if (openInput === undefined || openInput.chunksWaiting >= MOST_CHUNKS_WAITING_BY_KIND[kind]) {
      return;
    }
    const chunk = kind === "video"
      ? new VideoFrame(new Uint8Array(payload), {
          format: "RGBA",
          codedWidth: widthOrSampleRate,
          codedHeight: heightOrChannelCount,
          timestamp: timestampMicroseconds,
        })
      : new AudioData({
          format: "f32",
          sampleRate: widthOrSampleRate,
          numberOfChannels: heightOrChannelCount,
          numberOfFrames: payload.byteLength / 4 / heightOrChannelCount,
          timestamp: timestampMicroseconds,
          data: new Float32Array(payload),
        });
    openInput.chunksWaiting += 1;
    openInput.writer.write(chunk).then(
      () => { openInput.chunksWaiting -= 1; },
      () => { openInput.chunksWaiting -= 1; chunk.close(); }
    );
  };
  Object.defineProperty(window, "__tatolabDeliverPageInput", {
    value: deliverPageInput,
    enumerable: false,
  });
  Object.defineProperty(window, "tatolab", {
    value: Object.freeze({ publish, inputTrack }),
    enumerable: false,
  });
})();"#;

/// The bootstrap with `page_input_tracks_json` spliced in as a string literal
/// the bootstrap parses.
pub fn page_bridge_bootstrap_script(page_input_tracks_json: &str) -> String {
    PAGE_BRIDGE_BOOTSTRAP_SCRIPT.replace(
        PAGE_INPUT_TRACKS_PLACEHOLDER,
        &javascript_string_literal(page_input_tracks_json),
    )
}

/// What a [`PageInputChunk`] carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageInputChunkKind {
    /// RGBA rows, top first, `width × 4` bytes apart.
    VideoFrameRgba,
    /// Interleaved little-endian `f32` samples.
    AudioBlockF32,
}

impl PageInputChunkKind {
    const fn wire_value(self) -> u32 {
        match self {
            Self::VideoFrameRgba => 1,
            Self::AudioBlockF32 => 2,
        }
    }

    fn of_wire_value(value: u32) -> Option<Self> {
        match value {
            1 => Some(Self::VideoFrameRgba),
            2 => Some(Self::AudioBlockF32),
            _ => None,
        }
    }

    fn javascript_name(self) -> &'static str {
        match self {
            Self::VideoFrameRgba => "video",
            Self::AudioBlockF32 => "audio",
        }
    }
}

/// One video frame or audio block of a page input, as it crosses from the
/// browser process to the renderer.
///
/// In shared memory it is, little-endian: `u32` kind, `u32` name length,
/// `u32` width or sample rate, `u32` height or channel count, `i64`
/// timestamp in nanoseconds, the input port name padded to 8 bytes, then the
/// payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageInputChunk<'memory> {
    pub kind: PageInputChunkKind,
    pub input_port_name: &'memory str,
    pub width_or_sample_rate: u32,
    pub height_or_channel_count: u32,
    pub timestamp_ns: i64,
    pub payload: &'memory [u8],
}

const PAGE_INPUT_CHUNK_FIXED_HEADER_BYTES: usize = 24;

/// The longest input port name a chunk carries.
pub const MOST_PAGE_INPUT_PORT_NAME_BYTES: usize = 256;

fn padded_to_eight(byte_count: usize) -> usize {
    byte_count.div_ceil(8) * 8
}

impl PageInputChunk<'_> {
    /// The bytes the chunk takes in shared memory.
    pub fn encoded_byte_size(&self) -> usize {
        PAGE_INPUT_CHUNK_FIXED_HEADER_BYTES
            + padded_to_eight(self.input_port_name.len())
            + self.payload.len()
    }

    /// Write the chunk into `memory`, exactly [`Self::encoded_byte_size`] long.
    pub fn encode_into(&self, memory: &mut [u8]) -> Result<(), String> {
        if self.input_port_name.len() > MOST_PAGE_INPUT_PORT_NAME_BYTES {
            return Err(format!(
                "an input port name is at most {MOST_PAGE_INPUT_PORT_NAME_BYTES} bytes"
            ));
        }
        if memory.len() != self.encoded_byte_size() {
            return Err(format!(
                "a {} byte chunk was handed {} bytes to fill",
                self.encoded_byte_size(),
                memory.len()
            ));
        }
        let name_end = PAGE_INPUT_CHUNK_FIXED_HEADER_BYTES + self.input_port_name.len();
        let payload_start =
            PAGE_INPUT_CHUNK_FIXED_HEADER_BYTES + padded_to_eight(self.input_port_name.len());
        memory[0..4].copy_from_slice(&self.kind.wire_value().to_le_bytes());
        memory[4..8].copy_from_slice(&(self.input_port_name.len() as u32).to_le_bytes());
        memory[8..12].copy_from_slice(&self.width_or_sample_rate.to_le_bytes());
        memory[12..16].copy_from_slice(&self.height_or_channel_count.to_le_bytes());
        memory[16..24].copy_from_slice(&self.timestamp_ns.to_le_bytes());
        memory[PAGE_INPUT_CHUNK_FIXED_HEADER_BYTES..name_end]
            .copy_from_slice(self.input_port_name.as_bytes());
        memory[name_end..payload_start].fill(0);
        memory[payload_start..].copy_from_slice(self.payload);
        Ok(())
    }
}

impl<'memory> PageInputChunk<'memory> {
    /// Read a chunk [`Self::encode_into`] wrote.
    pub fn decode(memory: &'memory [u8]) -> Result<Self, String> {
        let header = memory
            .get(..PAGE_INPUT_CHUNK_FIXED_HEADER_BYTES)
            .ok_or_else(|| format!("a {} byte chunk holds no header", memory.len()))?;
        let u32_at = |offset: usize| {
            u32::from_le_bytes(header[offset..offset + 4].try_into().expect("four bytes"))
        };
        let kind = PageInputChunkKind::of_wire_value(u32_at(0))
            .ok_or_else(|| format!("chunk kind {} is not a page input's", u32_at(0)))?;
        let name_length = u32_at(4) as usize;
        if name_length > MOST_PAGE_INPUT_PORT_NAME_BYTES {
            return Err(format!("a chunk names a {name_length} byte input port"));
        }
        let payload_start = PAGE_INPUT_CHUNK_FIXED_HEADER_BYTES + padded_to_eight(name_length);
        if memory.len() < payload_start {
            return Err(format!(
                "a {} byte chunk is shorter than its header",
                memory.len()
            ));
        }
        let input_port_name = std::str::from_utf8(
            &memory[PAGE_INPUT_CHUNK_FIXED_HEADER_BYTES
                ..PAGE_INPUT_CHUNK_FIXED_HEADER_BYTES + name_length],
        )
        .map_err(|_| "a chunk's input port name is not UTF-8".to_string())?;
        Ok(Self {
            kind,
            input_port_name,
            width_or_sample_rate: u32_at(8),
            height_or_channel_count: u32_at(12),
            timestamp_ns: i64::from_le_bytes(header[16..24].try_into().expect("eight bytes")),
            payload: &memory[payload_start..],
        })
    }
}

/// The script that hands one `data` message to the page, its detail already
/// JSON text.
pub fn page_data_dispatch_script(detail_json: &str) -> String {
    format!(
        "window.dispatchEvent(new CustomEvent({}, {{ detail: {detail_json} }}));",
        javascript_string_literal(PAGE_DATA_DOM_EVENT_NAME)
    )
}

fn javascript_string_literal(text: &str) -> String {
    let mut literal = String::with_capacity(text.len() + 2);
    literal.push('"');
    for character in text.chars() {
        match character {
            '"' => literal.push_str("\\\""),
            '\\' => literal.push_str("\\\\"),
            '\n' => literal.push_str("\\n"),
            '\r' => literal.push_str("\\r"),
            '\u{2028}' => literal.push_str("\\u2028"),
            '\u{2029}' => literal.push_str("\\u2029"),
            control if control < ' ' => literal.push_str(&format!("\\u{:04x}", control as u32)),
            other => literal.push(other),
        }
    }
    literal.push('"');
    literal
}

/// The switch the browser process appends to every child's command line,
/// naming the browser process whose life each child is bound to.
pub const BROWSER_PROCESS_PID_SWITCH_NAME: &str = "tatolab-browser-process-pid";

/// Ends this process when the browser process that launched it dies.
pub fn die_with_the_browser_process() {
    let Some(browser_process_pid) = browser_process_pid_named_in(std::env::args()) else {
        return;
    };
    // Armed before the parent check: a browser process that died earlier fails the
    // check, and one that dies later fires the watch.
    arm_the_browser_process_exit_watch(browser_process_pid);
    // SAFETY: getppid takes nothing and cannot fail.
    if unsafe { libc::getppid() } != browser_process_pid {
        end_this_process_with_its_browser_process();
    }
}

fn browser_process_pid_named_in(
    command_line_arguments: impl IntoIterator<Item = String>,
) -> Option<libc::pid_t> {
    let switch_prefix = format!("--{BROWSER_PROCESS_PID_SWITCH_NAME}=");
    command_line_arguments.into_iter().find_map(|argument| {
        argument
            .strip_prefix(&switch_prefix)
            .and_then(|pid_text| pid_text.parse().ok())
    })
}

/// Exits at once, running no exit handlers, since CEF's threads may be live.
fn end_this_process_with_its_browser_process() -> ! {
    // SAFETY: _exit ends the process and touches nothing else.
    unsafe { libc::_exit(0) }
}

#[cfg(target_os = "linux")]
fn arm_the_browser_process_exit_watch(_browser_process_pid: libc::pid_t) {
    // SAFETY: prctl with PR_SET_PDEATHSIG takes a signal number and touches
    // nothing else.
    unsafe {
        libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
    }
}

#[cfg(target_os = "macos")]
fn arm_the_browser_process_exit_watch(browser_process_pid: libc::pid_t) {
    // SAFETY: kqueue takes nothing and returns a descriptor or -1.
    let exit_queue = unsafe { libc::kqueue() };
    if exit_queue < 0 {
        end_this_process_with_its_browser_process();
    }
    let browser_process_exit = libc::kevent {
        ident: browser_process_pid as libc::uintptr_t,
        filter: libc::EVFILT_PROC,
        flags: libc::EV_ADD | libc::EV_ONESHOT,
        fflags: libc::NOTE_EXIT,
        data: 0,
        udata: std::ptr::null_mut(),
    };
    // SAFETY: one change is registered from a live kevent and none is read.
    let registered = unsafe {
        libc::kevent(
            exit_queue,
            &browser_process_exit,
            1,
            std::ptr::null_mut(),
            0,
            std::ptr::null(),
        )
    };
    if registered < 0 {
        end_this_process_with_its_browser_process();
    }
    std::thread::Builder::new()
        .name("tatolab-browser-process-exit-watch".to_string())
        .spawn(move || {
            let mut fired = std::mem::MaybeUninit::<libc::kevent>::zeroed();
            loop {
                // SAFETY: waits for one event into storage it owns, with no
                // timeout.
                let received = unsafe {
                    libc::kevent(
                        exit_queue,
                        std::ptr::null(),
                        0,
                        fired.as_mut_ptr(),
                        1,
                        std::ptr::null(),
                    )
                };
                if received > 0
                    || std::io::Error::last_os_error().raw_os_error() != Some(libc::EINTR)
                {
                    end_this_process_with_its_browser_process();
                }
            }
        })
        .unwrap_or_else(|_| end_this_process_with_its_browser_process());
}

wrap_v8_handler! {
    struct PagePublishJsonV8Handler;

    impl V8Handler {
        fn execute(
            &self,
            _name: Option<&CefString>,
            _object: Option<&mut V8Value>,
            arguments: Option<&[Option<V8Value>]>,
            _retval: Option<&mut Option<V8Value>>,
            exception: Option<&mut CefString>,
        ) -> i32 {
            let published_json = arguments
                .and_then(|arguments| arguments.first())
                .and_then(|argument| argument.as_ref())
                .filter(|argument| argument.is_string() != 0)
                .map(|argument| CefString::from(&argument.string_value()).to_string());
            let Some(published_json) = published_json else {
                if let Some(exception) = exception {
                    *exception = CefString::from("tatolab.publish was handed no JSON text");
                }
                return 1;
            };
            let frame = v8_context_get_current_context().and_then(|context| context.frame());
            let message =
                process_message_create(Some(&CefString::from(PAGE_PUBLISH_PROCESS_MESSAGE_NAME)));
            match (frame, message) {
                (Some(frame), Some(mut message)) => {
                    if let Some(arguments) = message.argument_list() {
                        arguments.set_string(0, Some(&CefString::from(published_json.as_str())));
                    }
                    frame.send_process_message(ProcessId::BROWSER, Some(&mut message));
                }
                _ => {
                    if let Some(exception) = exception {
                        *exception =
                            CefString::from("tatolab.publish ran outside a frame that can publish");
                    }
                }
            }
            1
        }
    }
}

/// Each live browser's wired inputs, as the JSON its extra info carried, by
/// browser identifier. Read and written on the renderer's main thread.
static PAGE_INPUT_TRACKS_JSON_BY_BROWSER: std::sync::Mutex<
    std::collections::BTreeMap<i32, String>,
> = std::sync::Mutex::new(std::collections::BTreeMap::new());

fn page_input_tracks_json_by_browser()
-> std::sync::MutexGuard<'static, std::collections::BTreeMap<i32, String>> {
    PAGE_INPUT_TRACKS_JSON_BY_BROWSER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Hand one chunk to the frame's page through the bootstrap's deliverer.
fn deliver_the_page_input_chunk(frame: &Frame, chunk: &PageInputChunk<'_>) {
    let Some(context) = frame.v8_context() else {
        return;
    };
    if context.enter() == 0 {
        return;
    }
    let deliverer = context.global().and_then(|global| {
        global.value_bykey(Some(&CefString::from(PAGE_INPUT_DELIVERER_FUNCTION_NAME)))
    });
    if let Some(deliverer) = deliverer.filter(|deliverer| deliverer.is_function() != 0) {
        let payload = v8_value_create_array_buffer_with_copy(
            chunk.payload.as_ptr().cast_mut(),
            chunk.payload.len(),
        );
        let arguments = [
            v8_value_create_string(Some(&CefString::from(chunk.kind.javascript_name()))),
            v8_value_create_string(Some(&CefString::from(chunk.input_port_name))),
            v8_value_create_uint(chunk.width_or_sample_rate),
            v8_value_create_uint(chunk.height_or_channel_count),
            v8_value_create_double(chunk.timestamp_ns as f64 / 1_000.0),
            payload,
        ];
        deliverer.execute_function(None, Some(&arguments));
    }
    context.exit();
}

wrap_render_process_handler! {
    struct PageBridgeRenderProcessHandler;

    impl RenderProcessHandler {
        fn on_browser_created(
            &self,
            browser: Option<&mut Browser>,
            extra_info: Option<&mut DictionaryValue>,
        ) {
            let Some(browser) = browser else {
                return;
            };
            let key = CefString::from(PAGE_INPUT_TRACKS_EXTRA_INFO_KEY);
            let page_input_tracks_json = extra_info
                .filter(|extra_info| extra_info.has_key(Some(&key)) != 0)
                .map(|extra_info| CefString::from(&extra_info.string(Some(&key))).to_string())
                .unwrap_or_else(|| "[]".to_string());
            page_input_tracks_json_by_browser().insert(browser.identifier(), page_input_tracks_json);
        }

        fn on_browser_destroyed(&self, browser: Option<&mut Browser>) {
            if let Some(browser) = browser {
                page_input_tracks_json_by_browser().remove(&browser.identifier());
            }
        }

        fn on_context_created(
            &self,
            browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            context: Option<&mut V8Context>,
        ) {
            let Some(context) = context else {
                return;
            };
            let Some(global) = context.global() else {
                return;
            };
            let page_input_tracks_json = browser
                .and_then(|browser| {
                    page_input_tracks_json_by_browser()
                        .get(&browser.identifier())
                        .cloned()
                })
                .unwrap_or_else(|| "[]".to_string());
            let mut publish_handler = PagePublishJsonV8Handler::new();
            let function_name = CefString::from(NATIVE_PUBLISH_JSON_FUNCTION_NAME);
            let Some(mut publish_function) =
                v8_value_create_function(Some(&function_name), Some(&mut publish_handler))
            else {
                return;
            };
            global.set_value_bykey(
                Some(&function_name),
                Some(&mut publish_function),
                V8Propertyattribute::default(),
            );
            let mut bootstrap_result = None;
            let mut bootstrap_exception = None;
            context.eval(
                Some(&CefString::from(
                    page_bridge_bootstrap_script(&page_input_tracks_json).as_str(),
                )),
                Some(&CefString::from("tatolab://page-bridge")),
                1,
                Some(&mut bootstrap_result),
                Some(&mut bootstrap_exception),
            );
        }

        fn on_process_message_received(
            &self,
            _browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            _source_process: ProcessId,
            message: Option<&mut ProcessMessage>,
        ) -> i32 {
            let Some(message) = message else {
                return 0;
            };
            if CefString::from(&message.name()).to_string() != PAGE_INPUT_PROCESS_MESSAGE_NAME {
                return 0;
            }
            let (Some(frame), Some(region)) = (frame, message.shared_memory_region()) else {
                return 1;
            };
            if region.is_valid() == 0 || region.memory().is_null() {
                return 1;
            }
            // SAFETY: the region maps `size` readable bytes at `memory` for as
            // long as `region` lives, which outlasts this borrow.
            let memory = unsafe {
                std::slice::from_raw_parts(region.memory().cast::<u8>(), region.size())
            };
            if let Ok(chunk) = PageInputChunk::decode(memory) {
                deliver_the_page_input_chunk(frame, &chunk);
            }
            1
        }
    }
}

wrap_app! {
    pub struct BrowserChildProcessApp;

    impl App {
        fn render_process_handler(&self) -> Option<RenderProcessHandler> {
            Some(PageBridgeRenderProcessHandler::new())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_browser_process_pid_is_read_from_its_switch_and_nothing_else() {
        let arguments = |listed: &[&str]| {
            listed
                .iter()
                .map(|argument| argument.to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            browser_process_pid_named_in(arguments(&[
                "tatolab-browser-subprocess",
                "--type=renderer",
                "--tatolab-browser-process-pid=4242",
            ])),
            Some(4242)
        );
        assert_eq!(
            browser_process_pid_named_in(arguments(&[
                "tatolab-browser-subprocess",
                "--type=gpu-process"
            ])),
            None
        );
        assert_eq!(
            browser_process_pid_named_in(arguments(&["--tatolab-browser-process-pid=not-a-pid"])),
            None
        );
    }

    #[test]
    fn a_data_dispatch_names_the_dom_event_and_carries_the_detail_verbatim() {
        assert_eq!(
            page_data_dispatch_script(r#"{"title":"Hi"}"#),
            r#"window.dispatchEvent(new CustomEvent("tatolab:data", { detail: {"title":"Hi"} }));"#
        );
    }

    #[test]
    fn a_page_input_chunk_reads_back_as_written() {
        let payload = [9u8, 8, 7, 6, 5, 4, 3, 2];
        let chunk = PageInputChunk {
            kind: PageInputChunkKind::VideoFrameRgba,
            input_port_name: "video_in_0",
            width_or_sample_rate: 2,
            height_or_channel_count: 1,
            timestamp_ns: -5,
            payload: &payload,
        };
        let mut memory = vec![0xFF; chunk.encoded_byte_size()];
        chunk.encode_into(&mut memory).unwrap();

        assert_eq!(memory.len(), 24 + 16 + 8);
        assert_eq!(PageInputChunk::decode(&memory).unwrap(), chunk);
    }

    #[test]
    fn a_page_input_chunk_refuses_memory_of_another_size_and_a_header_past_its_end() {
        let chunk = PageInputChunk {
            kind: PageInputChunkKind::AudioBlockF32,
            input_port_name: "audio_in_0",
            width_or_sample_rate: 48_000,
            height_or_channel_count: 2,
            timestamp_ns: 0,
            payload: &[0; 8],
        };
        let mut short = vec![0; chunk.encoded_byte_size() - 1];
        assert!(chunk.encode_into(&mut short).is_err());

        assert!(PageInputChunk::decode(&[1, 0, 0, 0]).is_err());
        let mut naming_past_the_end = vec![0u8; 24];
        naming_past_the_end[0] = 2;
        naming_past_the_end[4] = 10;
        assert!(PageInputChunk::decode(&naming_past_the_end).is_err());
        let mut unknown_kind = vec![0u8; 24];
        unknown_kind[0] = 3;
        assert!(PageInputChunk::decode(&unknown_kind).is_err());
    }

    #[test]
    fn the_bootstrap_carries_the_wired_inputs_as_one_string_it_parses() {
        let script =
            page_bridge_bootstrap_script(r#"[{"input_port_name":"video_in_0","kind":"video"}]"#);
        assert!(!script.contains(PAGE_INPUT_TRACKS_PLACEHOLDER));
        assert!(script.contains(
            r#"JSON.parse("[{\"input_port_name\":\"video_in_0\",\"kind\":\"video\"}]")"#
        ));
        assert!(script.contains(PAGE_INPUT_DELIVERER_FUNCTION_NAME));
    }

    #[test]
    fn a_string_literal_escapes_what_would_end_or_break_it() {
        assert_eq!(
            javascript_string_literal("a\"b\\c\nd\u{2028}\u{1}"),
            r#""a\"b\\c\nd\u2028\u0001""#
        );
    }
}
