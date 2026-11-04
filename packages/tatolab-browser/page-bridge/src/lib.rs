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

/// Runs in every frame's context before the page's own scripts.
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
  Object.defineProperty(window, "tatolab", {
    value: Object.freeze({ publish }),
    enumerable: false,
  });
})();"#;

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

wrap_render_process_handler! {
    struct PageBridgeRenderProcessHandler;

    impl RenderProcessHandler {
        fn on_context_created(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            context: Option<&mut V8Context>,
        ) {
            let Some(context) = context else {
                return;
            };
            let Some(global) = context.global() else {
                return;
            };
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
                Some(&CefString::from(PAGE_BRIDGE_BOOTSTRAP_SCRIPT)),
                Some(&CefString::from("tatolab://page-bridge")),
                1,
                Some(&mut bootstrap_result),
                Some(&mut bootstrap_exception),
            );
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
    fn a_string_literal_escapes_what_would_end_or_break_it() {
        assert_eq!(
            javascript_string_literal("a\"b\\c\nd\u{2028}\u{1}"),
            r#""a\"b\\c\nd\u2028\u0001""#
        );
    }
}
