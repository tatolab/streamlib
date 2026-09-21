// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The GUI application macOS holds responsible for this process — the one a
//! privacy prompt names and the one the user enables in System Settings.
//!
//! `tccd` walks the responsibility chain up to the GUI host that launched the
//! process and attributes every camera request there: from a terminal, the
//! terminal emulator is the subject, never the interpreter or the engine.

/// How deep the parent walk goes before giving up; a real launch chain is a
/// handful of processes.
const MOST_ANCESTORS_WALKED: usize = 64;

/// The name to tell a user to look for in System Settings, or `None` when
/// neither the process's ancestry nor its environment names a GUI host.
pub(crate) fn responsible_gui_application_name() -> Option<String> {
    let non_empty_environment_variable =
        |name: &str| std::env::var(name).ok().filter(|value| !value.is_empty());
    app_bundle_of_the_nearest_gui_ancestor()
        .or_else(|| non_empty_environment_variable("__CFBundleIdentifier"))
        .or_else(|| non_empty_environment_variable("TERM_PROGRAM"))
}

/// The nearest ancestor running from inside an `.app` bundle, named by the
/// outermost bundle its executable lies in. A detached parent — a `tmux`
/// server, `nohup` — ends the walk at `launchd` with nothing found.
fn app_bundle_of_the_nearest_gui_ancestor() -> Option<String> {
    // SAFETY: `getppid` takes no arguments and cannot fail.
    let mut pid = unsafe { libc::getppid() };
    for _ in 0..MOST_ANCESTORS_WALKED {
        if pid <= 1 {
            return None;
        }
        if let Some(bundle_name) =
            executable_path_of_pid(pid).and_then(|path| outermost_app_bundle_name_in(&path))
        {
            return Some(bundle_name);
        }
        pid = parent_pid_of(pid)?;
    }
    None
}

/// The outermost `.app` bundle an executable path lies in — for a helper
/// nested inside its application's bundle, the application.
fn outermost_app_bundle_name_in(executable_path: &str) -> Option<String> {
    executable_path
        .split('/')
        .find_map(|component| component.strip_suffix(".app"))
        .filter(|bundle_name| !bundle_name.is_empty())
        .map(str::to_owned)
}

fn executable_path_of_pid(pid: libc::pid_t) -> Option<String> {
    let mut path = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    // SAFETY: `path` is a writable buffer of the length passed.
    let written = unsafe {
        libc::proc_pidpath(
            pid,
            path.as_mut_ptr().cast(),
            libc::PROC_PIDPATHINFO_MAXSIZE as u32,
        )
    };
    let written = usize::try_from(written)
        .ok()
        .filter(|&written| written > 0)?;
    path.truncate(written);
    String::from_utf8(path).ok()
}

fn parent_pid_of(pid: libc::pid_t) -> Option<libc::pid_t> {
    // SAFETY: `proc_bsdinfo` is plain data for which all-zero is valid.
    let mut process_info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let process_info_size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    // SAFETY: `process_info` is a writable `proc_bsdinfo`, the size passed is
    // its own, and `PROC_PIDTBSDINFO` fills exactly that struct.
    let written = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            (&mut process_info as *mut libc::proc_bsdinfo).cast(),
            process_info_size,
        )
    };
    if written != process_info_size {
        return None;
    }
    libc::pid_t::try_from(process_info.pbi_ppid).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_terminal_is_named_by_its_bundle() {
        assert_eq!(
            outermost_app_bundle_name_in("/Applications/iTerm.app/Contents/MacOS/iTerm2"),
            Some("iTerm".to_owned())
        );
    }

    #[test]
    fn a_helper_nested_in_its_applications_bundle_names_the_application() {
        assert_eq!(
            outermost_app_bundle_name_in(
                "/Applications/Visual Studio Code.app/Contents/Frameworks/\
                 Code Helper (Plugin).app/Contents/MacOS/Code Helper (Plugin)"
            ),
            Some("Visual Studio Code".to_owned())
        );
    }

    #[test]
    fn a_bare_executable_is_no_gui_application() {
        assert_eq!(
            outermost_app_bundle_name_in("/opt/homebrew/bin/python3.12"),
            None
        );
    }

    #[test]
    fn this_processs_own_executable_path_is_readable() {
        // SAFETY: `getpid` takes no arguments and cannot fail.
        let own_pid = unsafe { libc::getpid() };
        let path = executable_path_of_pid(own_pid).expect("a live process has a path");
        assert!(path.starts_with('/'), "{path}");
        // SAFETY: as above.
        let parent = unsafe { libc::getppid() };
        assert_eq!(parent_pid_of(own_pid), Some(parent));
    }
}
