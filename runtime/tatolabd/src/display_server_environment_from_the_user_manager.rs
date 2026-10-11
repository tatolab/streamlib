// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The display server a runtime started with none in its environment takes
//! from the systemd user manager's, which a desktop session imports at login.
//!
//! A user service inherits the manager's environment as it stood when the
//! service started, so a runtime started before its desktop session imported a
//! display never sees it. winit and the Vulkan driver read the display from the
//! process environment alone, so what the manager holds is copied into it at
//! the start, before any thread or Vulkan instance exists: NVIDIA's driver
//! reads `DISPLAY` once, at `vkCreateInstance`, and an X11 swapchain made on an
//! instance created without it jumps to a null pointer (driver 595.99), so a
//! display taken any later could never be presented to.

use std::ffi::OsString;

/// The variables winit picks a backend from, Wayland's first as winit tries it.
const DISPLAY_SERVER_ENVIRONMENT_VARIABLES: [&str; 2] = ["WAYLAND_DISPLAY", "DISPLAY"];

/// Taken beside the display: an X server a display manager started admits
/// clients by the cookie file this names, which is rarely `~/.Xauthority`.
const X11_AUTHORITY_FILE_ENVIRONMENT_VARIABLE: &str = "XAUTHORITY";

/// One variable the user manager's environment sets that this process takes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DisplayServerVariableFromTheUserManager<'user_manager_environment> {
    pub(crate) name: &'static str,
    pub(crate) value: &'user_manager_environment str,
}

/// Where this runtime's display server came from at its start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DisplayServerEnvironmentAtTheRuntimesStart {
    /// The process was started naming one.
    NamedByThisProcess,
    /// Copied from the user manager's environment; the names taken.
    TakenFromTheUserManager(Vec<&'static str>),
    /// The user manager's environment names none either.
    NamedByNeitherThisProcessNorTheUserManager,
    /// The user manager's environment could not be read; why.
    UserManagerUnreadable(String),
    /// The platform has no display server environment to look for.
    #[cfg(not(target_os = "linux"))]
    NotLookedForOnThisPlatform,
}

impl DisplayServerEnvironmentAtTheRuntimesStart {
    /// Say where the display came from, and when there is none, that every
    /// window is refused until the runtime restarts inside a desktop session.
    pub(crate) fn log_where_the_display_server_came_from(&self) {
        const RESTART_INSIDE_A_DESKTOP_SESSION: &str = "every window this runtime is asked for \
             is refused until it restarts inside a desktop session (`systemctl --user restart \
             tatolabd`, or `tatolabd` in a terminal of its own)";
        match self {
            Self::NamedByThisProcess => {}
            #[cfg(not(target_os = "linux"))]
            Self::NotLookedForOnThisPlatform => {}
            Self::TakenFromTheUserManager(taken) => tracing::info!(
                ?taken,
                "this runtime's environment named no display server; took it from the systemd \
                 user manager's"
            ),
            Self::NamedByNeitherThisProcessNorTheUserManager => tracing::warn!(
                "neither this runtime's environment nor the systemd user manager's names a \
                 display server (WAYLAND_DISPLAY or DISPLAY): {RESTART_INSIDE_A_DESKTOP_SESSION}"
            ),
            Self::UserManagerUnreadable(reason) => tracing::warn!(
                %reason,
                "this runtime's environment names no display server (WAYLAND_DISPLAY or \
                 DISPLAY), and the systemd user manager's could not be read for one: \
                 {RESTART_INSIDE_A_DESKTOP_SESSION}"
            ),
        }
    }
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn is_set_and_non_empty(value: Option<&str>) -> bool {
    value.is_some_and(|value| !value.is_empty())
}

/// What this process takes from the user manager's environment — the
/// `NAME=value` lines `systemctl --user show-environment` prints — given what
/// the process already holds: each display variable the manager sets, and
/// `XAUTHORITY` beside them, never one the process already sets. Empty when the
/// manager names no display server. A value systemd had to quote is skipped,
/// never unquoted.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn display_server_variables_to_take_from_the_user_manager(
    user_manager_environment: &str,
    this_process_variable: impl Fn(&str) -> Option<OsString>,
) -> Vec<DisplayServerVariableFromTheUserManager<'_>> {
    let user_manager_variable = |name: &str| {
        user_manager_environment.lines().find_map(|assignment| {
            assignment
                .strip_prefix(name)
                .and_then(|rest| rest.strip_prefix('='))
                .filter(|value| !value.starts_with(['$', '\'', '"']))
                .filter(|value| is_set_and_non_empty(Some(value)))
        })
    };
    if !DISPLAY_SERVER_ENVIRONMENT_VARIABLES
        .iter()
        .any(|name| user_manager_variable(name).is_some())
    {
        return Vec::new();
    }
    DISPLAY_SERVER_ENVIRONMENT_VARIABLES
        .into_iter()
        .chain([X11_AUTHORITY_FILE_ENVIRONMENT_VARIABLE])
        .filter(|name| this_process_variable(name).is_none_or(|value| value.is_empty()))
        .filter_map(|name| {
            user_manager_variable(name)
                .map(|value| DisplayServerVariableFromTheUserManager { name, value })
        })
        .collect()
}

#[cfg(target_os = "linux")]
fn read_the_user_managers_environment() -> Result<String, String> {
    let show_environment = std::process::Command::new("systemctl")
        .args(["--user", "show-environment"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output()
        .map_err(|spawn_failure| format!("`systemctl --user show-environment`: {spawn_failure}"))?;
    if !show_environment.status.success() {
        return Err(format!(
            "`systemctl --user show-environment` failed ({}): {}",
            show_environment.status,
            String::from_utf8_lossy(&show_environment.stderr).trim()
        ));
    }
    String::from_utf8(show_environment.stdout).map_err(|_| {
        "`systemctl --user show-environment` printed an environment that is not UTF-8".to_owned()
    })
}

/// Copy the display server from the systemd user manager's environment into
/// this process's when it names none. Called first in `main`, while the
/// process has no other thread: `systemctl` runs as a child process, so none is
/// left behind.
pub(crate) fn take_the_display_server_from_the_user_manager_before_any_thread_exists()
-> DisplayServerEnvironmentAtTheRuntimesStart {
    #[cfg(not(target_os = "linux"))]
    {
        DisplayServerEnvironmentAtTheRuntimesStart::NotLookedForOnThisPlatform
    }
    #[cfg(target_os = "linux")]
    {
        if DISPLAY_SERVER_ENVIRONMENT_VARIABLES
            .iter()
            .any(|name| std::env::var_os(name).is_some_and(|value| !value.is_empty()))
        {
            return DisplayServerEnvironmentAtTheRuntimesStart::NamedByThisProcess;
        }
        let user_manager_environment = match read_the_user_managers_environment() {
            Ok(user_manager_environment) => user_manager_environment,
            Err(reason) => {
                return DisplayServerEnvironmentAtTheRuntimesStart::UserManagerUnreadable(reason);
            }
        };
        let variables_to_take = display_server_variables_to_take_from_the_user_manager(
            &user_manager_environment,
            |name| std::env::var_os(name),
        );
        if variables_to_take.is_empty() {
            return DisplayServerEnvironmentAtTheRuntimesStart::NamedByNeitherThisProcessNorTheUserManager;
        }
        for DisplayServerVariableFromTheUserManager { name, value } in &variables_to_take {
            // SAFETY: no other thread exists yet to read or write the
            // environment; the `systemctl` child that read the manager's has
            // exited.
            unsafe { std::env::set_var(name, value) };
        }
        DisplayServerEnvironmentAtTheRuntimesStart::TakenFromTheUserManager(
            variables_to_take
                .iter()
                .map(|DisplayServerVariableFromTheUserManager { name, .. }| *name)
                .collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a_process_holding(
        held: &'static [(&'static str, &'static str)],
    ) -> impl Fn(&str) -> Option<OsString> {
        move |name| {
            held.iter()
                .find(|(held_name, _)| *held_name == name)
                .map(|(_, value)| OsString::from(value))
        }
    }

    fn taken_names_and_values(
        taken: &[DisplayServerVariableFromTheUserManager<'_>],
    ) -> Vec<(&'static str, String)> {
        taken
            .iter()
            .map(|taken_variable| (taken_variable.name, taken_variable.value.to_owned()))
            .collect()
    }

    #[test]
    fn an_x11_desktop_hands_over_its_display_and_its_authority_file() {
        let taken = display_server_variables_to_take_from_the_user_manager(
            "HOME=/home/someone\nDISPLAY=:1\nXAUTHORITY=/run/user/1000/gdm/Xauthority\n",
            a_process_holding(&[]),
        );
        assert_eq!(
            taken_names_and_values(&taken),
            vec![
                ("DISPLAY", ":1".to_owned()),
                ("XAUTHORITY", "/run/user/1000/gdm/Xauthority".to_owned()),
            ]
        );
    }

    #[test]
    fn a_wayland_desktop_hands_over_both_displays_wayland_first() {
        let taken = display_server_variables_to_take_from_the_user_manager(
            "DISPLAY=:0\nWAYLAND_DISPLAY=wayland-0\n",
            a_process_holding(&[]),
        );
        assert_eq!(
            taken_names_and_values(&taken),
            vec![
                ("WAYLAND_DISPLAY", "wayland-0".to_owned()),
                ("DISPLAY", ":0".to_owned()),
            ]
        );
    }

    #[test]
    fn a_manager_naming_no_display_hands_over_nothing_not_even_its_authority_file() {
        let taken = display_server_variables_to_take_from_the_user_manager(
            "XAUTHORITY=/home/someone/.Xauthority\nDISPLAY=\n",
            a_process_holding(&[]),
        );
        assert!(taken.is_empty(), "{taken:?}");
    }

    #[test]
    fn a_variable_the_process_already_holds_is_never_overwritten() {
        let taken = display_server_variables_to_take_from_the_user_manager(
            "DISPLAY=:1\nXAUTHORITY=/run/user/1000/gdm/Xauthority\n",
            a_process_holding(&[("XAUTHORITY", "/home/someone/.Xauthority")]),
        );
        assert_eq!(
            taken_names_and_values(&taken),
            vec![("DISPLAY", ":1".to_owned())]
        );
    }

    #[test]
    fn a_process_holding_only_the_x11_display_still_takes_the_wayland_one() {
        let taken = display_server_variables_to_take_from_the_user_manager(
            "WAYLAND_DISPLAY=wayland-0\nDISPLAY=:0\n",
            a_process_holding(&[("DISPLAY", ":0")]),
        );
        assert_eq!(
            taken_names_and_values(&taken),
            vec![("WAYLAND_DISPLAY", "wayland-0".to_owned())]
        );
    }

    #[test]
    fn a_name_sharing_a_prefix_is_not_the_variable() {
        let taken = display_server_variables_to_take_from_the_user_manager(
            "DISPLAY_SCALE=2\nWAYLAND_DISPLAY_FALLBACK=x\n",
            a_process_holding(&[]),
        );
        assert!(taken.is_empty(), "{taken:?}");
    }

    #[test]
    fn a_value_systemd_quoted_is_skipped_never_unquoted() {
        let taken = display_server_variables_to_take_from_the_user_manager(
            "DISPLAY=$':1\\nx'\nXAUTHORITY=/run/user/1000/gdm/Xauthority\n",
            a_process_holding(&[]),
        );
        assert!(taken.is_empty(), "{taken:?}");
    }
}
