// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The display server a runtime started with none in its environment takes
//! from the systemd user manager's, which a desktop session imports at login.
//!
//! A user service inherits the manager's environment as it stood when the
//! service started, so a runtime started before its desktop session imported a
//! display never sees it. winit reads the display from the process environment
//! and offers no other way to name one, so what the manager holds is copied
//! into it — before the GPU context is created, never after: NVIDIA's driver
//! reads `DISPLAY` once, at `vkCreateInstance`, and an X11 swapchain made on an
//! instance created without it jumps to a null pointer (595.99, measured
//! 2026-10-11). A runtime whose GPU context was created naming no display
//! says so in every window refusal instead.

use std::sync::Mutex;

use zbus::blocking::{Connection, Proxy};

const SYSTEMD_MANAGER_SERVICE: &str = "org.freedesktop.systemd1";
const SYSTEMD_MANAGER_PATH: &str = "/org/freedesktop/systemd1";
const SYSTEMD_MANAGER_INTERFACE: &str = "org.freedesktop.systemd1.Manager";
const SYSTEMD_MANAGER_ENVIRONMENT_PROPERTY: &str = "Environment";

/// The variables winit picks a backend from, Wayland's first as winit tries it.
const DISPLAY_SERVER_ENVIRONMENT_VARIABLES: [&str; 2] = ["WAYLAND_DISPLAY", "DISPLAY"];

/// Taken beside the display: an X server a display manager started admits
/// clients by the cookie file this names, which is rarely `~/.Xauthority`.
const X11_AUTHORITY_FILE_ENVIRONMENT_VARIABLE: &str = "XAUTHORITY";

/// Serialises the copy, so two GPU contexts created at once copy it once.
static COPY_OF_THE_DISPLAY_SERVER_ENVIRONMENT: Mutex<()> = Mutex::new(());

/// Whether a variable is set to something winit would act on.
fn is_set_and_non_empty(value: Option<&str>) -> bool {
    value.is_some_and(|value| !value.is_empty())
}

/// What this process must take from the user manager's environment, given
/// what the process already holds: each display variable the manager sets,
/// and `XAUTHORITY` beside them, never one the process already sets. Empty
/// when the manager names no display server.
pub fn display_server_variables_to_take_from_the_user_manager(
    user_manager_environment: &[String],
    this_process_variable: impl Fn(&str) -> Option<String>,
) -> Vec<(String, String)> {
    let user_manager_variable = |name: &str| {
        user_manager_environment.iter().find_map(|assignment| {
            assignment
                .strip_prefix(name)
                .and_then(|rest| rest.strip_prefix('='))
                .filter(|value| !value.is_empty())
        })
    };
    if !DISPLAY_SERVER_ENVIRONMENT_VARIABLES
        .iter()
        .any(|name| user_manager_variable(name).is_some())
    {
        return Vec::new();
    }
    DISPLAY_SERVER_ENVIRONMENT_VARIABLES
        .iter()
        .chain(std::iter::once(&X11_AUTHORITY_FILE_ENVIRONMENT_VARIABLE))
        .filter(|name| !is_set_and_non_empty(this_process_variable(name).as_deref()))
        .filter_map(|name| {
            user_manager_variable(name).map(|value| ((*name).to_owned(), value.to_owned()))
        })
        .collect()
}

fn this_process_names_a_display_server() -> bool {
    DISPLAY_SERVER_ENVIRONMENT_VARIABLES
        .iter()
        .any(|name| is_set_and_non_empty(std::env::var(name).ok().as_deref()))
}

fn read_the_user_managers_environment() -> zbus::Result<Vec<String>> {
    let session_bus = Connection::session()?;
    let systemd_manager = Proxy::new(
        &session_bus,
        SYSTEMD_MANAGER_SERVICE,
        SYSTEMD_MANAGER_PATH,
        SYSTEMD_MANAGER_INTERFACE,
    )?;
    systemd_manager.get_property::<Vec<String>>(SYSTEMD_MANAGER_ENVIRONMENT_PROPERTY)
}

/// Copy the display server from the systemd user manager's environment into
/// this process's when it names none. Called before the GPU context's Vulkan
/// instance is created; a manager naming none, or none reachable, leaves the
/// process headless, which a GPU context needs nothing more than.
pub fn take_the_display_server_from_the_user_manager_when_this_process_names_none() {
    if this_process_names_a_display_server() {
        return;
    }
    let _copy_in_progress = COPY_OF_THE_DISPLAY_SERVER_ENVIRONMENT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if this_process_names_a_display_server() {
        return;
    }

    let user_manager_environment = match read_the_user_managers_environment() {
        Ok(user_manager_environment) => user_manager_environment,
        Err(reason) => {
            tracing::debug!(
                %reason,
                "this runtime's environment names no display server, and the systemd user \
                 manager's could not be read for one; it runs headless"
            );
            return;
        }
    };
    let variables_to_take =
        display_server_variables_to_take_from_the_user_manager(&user_manager_environment, |name| {
            std::env::var(name).ok()
        });
    if variables_to_take.is_empty() {
        tracing::debug!(
            "this runtime's environment names no display server, and neither does the systemd \
             user manager's; it runs headless"
        );
        return;
    }
    for (name, value) in &variables_to_take {
        // SAFETY: std serialises this against every `std::env` reader in the
        // process. The foreign readers of these variables — the Vulkan driver,
        // winit and its X11 and Wayland clients — first read them when the GPU
        // context this precedes is created, on this thread, and the window
        // event pump that winit runs on is started only after that.
        unsafe { std::env::set_var(name, value) };
    }
    tracing::info!(
        taken = ?variables_to_take.iter().map(|(name, _)| name.as_str()).collect::<Vec<_>>(),
        "this runtime's environment named no display server; took it from the systemd user \
         manager's"
    );
}

/// Why this process names no display server, when it names none: its GPU
/// context was created without one, so no later one can be taken.
pub fn why_this_process_names_no_display_server() -> Option<&'static str> {
    if this_process_names_a_display_server() {
        return None;
    }
    Some(
        "this runtime named no display server when its GPU context was created: neither \
         WAYLAND_DISPLAY nor DISPLAY was set in its environment or in the systemd user manager's. \
         A display a desktop session brings later cannot be presented to by this runtime; \
         restart it from inside the session (`systemctl --user restart tatolabd`, or `tatolabd` \
         in a terminal of its own)",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user_manager_environment(assignments: &[&str]) -> Vec<String> {
        assignments
            .iter()
            .map(|&assignment| assignment.to_owned())
            .collect()
    }

    fn a_process_holding(
        held: &'static [(&'static str, &'static str)],
    ) -> impl Fn(&str) -> Option<String> {
        move |name| {
            held.iter()
                .find(|(held_name, _)| *held_name == name)
                .map(|(_, value)| (*value).to_owned())
        }
    }

    #[test]
    fn an_x11_desktop_hands_over_its_display_and_its_authority_file() {
        let taken = display_server_variables_to_take_from_the_user_manager(
            &user_manager_environment(&[
                "HOME=/home/someone",
                "DISPLAY=:1",
                "XAUTHORITY=/run/user/1000/gdm/Xauthority",
            ]),
            a_process_holding(&[]),
        );
        assert_eq!(
            taken,
            vec![
                ("DISPLAY".to_owned(), ":1".to_owned()),
                (
                    "XAUTHORITY".to_owned(),
                    "/run/user/1000/gdm/Xauthority".to_owned()
                ),
            ]
        );
    }

    #[test]
    fn a_wayland_desktop_hands_over_both_displays_wayland_first() {
        let taken = display_server_variables_to_take_from_the_user_manager(
            &user_manager_environment(&["DISPLAY=:0", "WAYLAND_DISPLAY=wayland-0"]),
            a_process_holding(&[]),
        );
        assert_eq!(
            taken,
            vec![
                ("WAYLAND_DISPLAY".to_owned(), "wayland-0".to_owned()),
                ("DISPLAY".to_owned(), ":0".to_owned()),
            ]
        );
    }

    #[test]
    fn a_manager_naming_no_display_hands_over_nothing_not_even_its_authority_file() {
        let taken = display_server_variables_to_take_from_the_user_manager(
            &user_manager_environment(&["XAUTHORITY=/home/someone/.Xauthority", "DISPLAY="]),
            a_process_holding(&[]),
        );
        assert!(taken.is_empty(), "{taken:?}");
    }

    #[test]
    fn a_variable_the_process_already_holds_is_never_overwritten() {
        let taken = display_server_variables_to_take_from_the_user_manager(
            &user_manager_environment(&["DISPLAY=:1", "XAUTHORITY=/run/user/1000/gdm/Xauthority"]),
            a_process_holding(&[("XAUTHORITY", "/home/someone/.Xauthority")]),
        );
        assert_eq!(taken, vec![("DISPLAY".to_owned(), ":1".to_owned())]);
    }

    #[test]
    fn a_name_sharing_a_prefix_is_not_the_variable() {
        let taken = display_server_variables_to_take_from_the_user_manager(
            &user_manager_environment(&["DISPLAY_SCALE=2", "WAYLAND_DISPLAY_FALLBACK=x"]),
            a_process_holding(&[]),
        );
        assert!(taken.is_empty(), "{taken:?}");
    }
}
