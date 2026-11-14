// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! `tatolab enable-virtual-camera`: the one-time grant behind the virtual camera's loopback door.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};

use crate::TatolabCommandFailure;
use crate::verb_standard_output::write_verb_standard_output;

/// The verb as a user types it.
const ENABLE_VIRTUAL_CAMERA_VERB: &str = "tatolab enable-virtual-camera";

/// The kernel module whose devices the virtual camera is.
const LOOPBACK_MODULE_NAME: &str = "v4l2loopback";

/// The file-name prefix of the loopback module's object, compressed or not.
const LOOPBACK_MODULE_OBJECT_FILE_NAME_PREFIX: &str = "v4l2loopback.ko";

/// The node the loopback module creates and removes its devices through.
const LOOPBACK_CONTROL_NODE_PATH: &str = "/dev/v4l2loopback";

/// The directory holding one module tree per kernel release.
const KERNEL_MODULES_ROOT_DIRECTORY: &str = "/lib/modules";

/// The `uname -s` of the one operating system the loopback module exists on.
const LINUX_OPERATING_SYSTEM_NAME: &str = "Linux";

/// The directories searched for the privilege helper when `PATH` is unset.
const DEFAULT_EXECUTABLE_SEARCH_PATH: &str = "/bin:/usr/bin";

/// Variables a desktop session sets; only a session has the polkit agent `pkexec` prompts through.
const DESKTOP_SESSION_ENVIRONMENT_VARIABLES: [&str; 2] = ["DISPLAY", "WAYLAND_DISPLAY"];

const LOAD_LOOPBACK_MODULE_WITHOUT_DEVICES_COMMAND: &str = "modprobe v4l2loopback devices=0";
const RELOAD_UDEV_RULES_COMMAND: &str = "udevadm control --reload";
/// Must select the control node, or a rule written after the module loaded never applies to it.
const RETRIGGER_UDEV_ON_THE_CONTROL_NODE_COMMAND: &str =
    "udevadm trigger --subsystem-match=misc --sysname-match=v4l2loopback";
const WAIT_FOR_UDEV_TO_SETTLE_COMMAND: &str = "udevadm settle || true";

const GRANT_FILE_HEREDOC_DELIMITER: &str = "STREAMLIB_EOF";

/// One file the grant installs.
struct VirtualCameraGrantFile {
    /// The directory the file is written into, created when absent.
    destination_directory: &'static str,
    /// The file's name inside its destination directory.
    destination_file_name: &'static str,
    /// What the file holds.
    contents: &'static str,
}

impl VirtualCameraGrantFile {
    fn destination_path(&self) -> String {
        format!(
            "{}/{}",
            self.destination_directory, self.destination_file_name
        )
    }
}

/// `modules-load.d` loads the module at boot, `modprobe.d` keeps it device-less so each sink
/// creates its own, and the udev rule hands the seat's user the control node.
const VIRTUAL_CAMERA_GRANT_FILES: [VirtualCameraGrantFile; 3] = [
    VirtualCameraGrantFile {
        destination_directory: "/etc/modules-load.d",
        destination_file_name: "streamlib-virtual-camera.conf",
        contents: "# Installed by `tatolab enable-virtual-camera`: load the loopback module at boot.\n\
                   v4l2loopback\n",
    },
    VirtualCameraGrantFile {
        destination_directory: "/etc/modprobe.d",
        destination_file_name: "streamlib-virtual-camera.conf",
        contents: "# Installed by `tatolab enable-virtual-camera`: no pre-made devices — each\n\
                   # VirtualCameraSink creates and removes its own.\n\
                   options v4l2loopback devices=0\n",
    },
    VirtualCameraGrantFile {
        destination_directory: "/etc/udev/rules.d",
        destination_file_name: "70-streamlib-virtual-camera.rules",
        contents: "# Installed by `tatolab enable-virtual-camera`: the logged-in seat user may\n\
                   # open the loopback control node, so a VirtualCameraSink can add a camera.\n\
                   KERNEL==\"v4l2loopback\", SUBSYSTEM==\"misc\", TAG+=\"uaccess\"\n",
    },
];

/// The program that runs the grant's one privileged step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PrivilegeEscalationHelper {
    /// polkit's `pkexec`, which prompts through the desktop session's agent.
    Pkexec,
    /// `sudo`, which prompts on the terminal.
    Sudo,
}

impl PrivilegeEscalationHelper {
    /// The executable looked up on PATH, and named to the user.
    pub(crate) fn executable_name(self) -> &'static str {
        match self {
            PrivilegeEscalationHelper::Pkexec => "pkexec",
            PrivilegeEscalationHelper::Sudo => "sudo",
        }
    }
}

/// A privilege helper and the executable its PATH lookup resolved to, which is the one run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ResolvedPrivilegeEscalationHelper {
    /// Which helper it is.
    pub(crate) privilege_escalation_helper: PrivilegeEscalationHelper,
    /// The file the lookup found.
    pub(crate) resolved_helper_executable: PathBuf,
}

/// Resolves an executable name against PATH.
pub(crate) type ExecutableOnPathFinder<'machine> = Box<dyn Fn(&str) -> Option<PathBuf> + 'machine>;

/// Reads one environment variable.
pub(crate) type EnvironmentVariableReader<'machine> =
    Box<dyn Fn(&str) -> Option<OsString> + 'machine>;

/// Runs `<resolved helper executable> sh -c <script>` and reports how it ended.
pub(crate) type PrivilegedScriptRunner<'machine> =
    Box<dyn FnMut(&ResolvedPrivilegeEscalationHelper, &str) -> io::Result<ExitStatus> + 'machine>;

/// The machine facts `enable-virtual-camera` decides on and the one command it runs, passed in so
/// every decision runs without root.
pub(crate) struct VirtualCameraGrantTargetMachine<'machine> {
    /// The kernel's name for the operating system, as `uname -s` prints it.
    pub(crate) operating_system_name: String,
    /// The running kernel's release, as `uname -r` prints it.
    pub(crate) kernel_release: String,
    /// The directory holding one module tree per kernel release.
    pub(crate) kernel_modules_root_directory: PathBuf,
    /// The loopback module's control node.
    pub(crate) loopback_control_node_path: PathBuf,
    /// Resolves an executable name against PATH.
    pub(crate) find_executable_on_path: ExecutableOnPathFinder<'machine>,
    /// Reads one environment variable.
    pub(crate) read_environment_variable: EnvironmentVariableReader<'machine>,
    /// Runs the privileged grant script through the chosen helper.
    pub(crate) run_privileged_script_through_helper: PrivilegedScriptRunner<'machine>,
}

impl VirtualCameraGrantTargetMachine<'static> {
    /// The machine `tatolab` runs on, with the helper attached to this terminal.
    pub(crate) fn this_machine() -> io::Result<Self> {
        let (operating_system_name, kernel_release) =
            this_kernels_operating_system_name_and_release()?;
        Ok(Self {
            operating_system_name,
            kernel_release,
            kernel_modules_root_directory: PathBuf::from(KERNEL_MODULES_ROOT_DIRECTORY),
            loopback_control_node_path: PathBuf::from(LOOPBACK_CONTROL_NODE_PATH),
            find_executable_on_path: Box::new(find_executable_on_this_processes_path),
            read_environment_variable: Box::new(|variable_name| std::env::var_os(variable_name)),
            run_privileged_script_through_helper: Box::new(
                run_privileged_script_through_helper_on_this_terminal,
            ),
        })
    }
}

fn this_kernels_operating_system_name_and_release() -> io::Result<(String, String)> {
    // SAFETY: `utsname` is plain character arrays, for which all-zero is a valid value.
    let mut kernel_identification: libc::utsname = unsafe { std::mem::zeroed() };
    // SAFETY: the pointer is to a live `utsname` this frame owns for the call.
    if unsafe { libc::uname(&mut kernel_identification) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((
        utsname_field_text(&kernel_identification.sysname),
        utsname_field_text(&kernel_identification.release),
    ))
}

fn utsname_field_text(utsname_field: &[libc::c_char]) -> String {
    let field_bytes: Vec<u8> = utsname_field
        .iter()
        .map(|&field_character| u8::from_ne_bytes(field_character.to_ne_bytes()))
        .take_while(|&field_byte| field_byte != 0)
        .collect();
    String::from_utf8_lossy(&field_bytes).into_owned()
}

fn find_executable_on_this_processes_path(executable_name: &str) -> Option<PathBuf> {
    find_executable_on_search_path(
        executable_name,
        &executable_search_path_from(std::env::var_os("PATH")),
    )
}

/// `PATH`, or [`DEFAULT_EXECUTABLE_SEARCH_PATH`] when it is unset.
fn executable_search_path_from(path_variable: Option<OsString>) -> OsString {
    path_variable.unwrap_or_else(|| OsString::from(DEFAULT_EXECUTABLE_SEARCH_PATH))
}

/// The first executable file named `executable_name` in `search_path`'s absolute directories.
/// An empty or relative entry names the working directory, which never supplies a privilege
/// helper, so it is skipped and the resolved path is always absolute.
fn find_executable_on_search_path(
    executable_name: &str,
    search_path: &std::ffi::OsStr,
) -> Option<PathBuf> {
    std::env::split_paths(search_path)
        .filter(|path_directory| path_directory.is_absolute())
        .map(|path_directory| path_directory.join(executable_name))
        .find(|candidate_executable| {
            fs::metadata(candidate_executable).is_ok_and(|candidate_metadata| {
                candidate_metadata.is_file() && candidate_metadata.permissions().mode() & 0o111 != 0
            })
        })
}

fn run_privileged_script_through_helper_on_this_terminal(
    resolved_privilege_escalation_helper: &ResolvedPrivilegeEscalationHelper,
    privileged_script: &str,
) -> io::Result<ExitStatus> {
    Command::new(&resolved_privilege_escalation_helper.resolved_helper_executable)
        .args(["sh", "-c", privileged_script])
        .status()
}

/// The three files and the root commands as one printable block, for a user placing them by hand.
fn render_virtual_camera_grant_for_hand_install() -> String {
    let mut rendered_blocks: Vec<String> = VIRTUAL_CAMERA_GRANT_FILES
        .iter()
        .map(|grant_file| {
            format!(
                "# ---- {} ----\n{}",
                grant_file.destination_path(),
                grant_file.contents
            )
        })
        .collect();
    rendered_blocks.push(format!(
        "# ---- then, as root ----\n\
         {LOAD_LOOPBACK_MODULE_WITHOUT_DEVICES_COMMAND}\n\
         {RELOAD_UDEV_RULES_COMMAND}\n\
         {RETRIGGER_UDEV_ON_THE_CONTROL_NODE_COMMAND}\n"
    ));
    rendered_blocks.join("\n")
}

/// One shell script that writes the grant files and reloads udev, run once with privilege.
fn virtual_camera_grant_privileged_script() -> String {
    let mut script_lines = vec!["set -eu".to_owned()];
    for grant_file in &VIRTUAL_CAMERA_GRANT_FILES {
        script_lines.push(format!("mkdir -p {}", grant_file.destination_directory));
        script_lines.push(format!(
            "cat > {} <<'{GRANT_FILE_HEREDOC_DELIMITER}'\n{}{GRANT_FILE_HEREDOC_DELIMITER}",
            grant_file.destination_path(),
            grant_file.contents
        ));
    }
    script_lines.extend(
        [
            LOAD_LOOPBACK_MODULE_WITHOUT_DEVICES_COMMAND,
            RELOAD_UDEV_RULES_COMMAND,
            RETRIGGER_UDEV_ON_THE_CONTROL_NODE_COMMAND,
            WAIT_FOR_UDEV_TO_SETTLE_COMMAND,
        ]
        .map(str::to_owned),
    );
    script_lines.join("\n") + "\n"
}

/// Whether this user can open the control node read-write — the probe a VirtualCameraSink makes
/// at `setup()`. An open that never seeks, since the node is a character device.
pub(crate) fn control_node_is_writable_by_this_user(loopback_control_node_path: &Path) -> bool {
    fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(loopback_control_node_path)
        .is_ok()
}

/// Whether the module tree for `kernel_release` holds a `v4l2loopback.ko*` anywhere beneath it.
fn loopback_module_is_installed_for_kernel(
    kernel_modules_root_directory: &Path,
    kernel_release: &str,
) -> bool {
    let mut directories_to_search = vec![kernel_modules_root_directory.join(kernel_release)];
    while let Some(searched_directory) = directories_to_search.pop() {
        let Ok(directory_entries) = fs::read_dir(&searched_directory) else {
            continue;
        };
        for directory_entry in directory_entries.flatten() {
            if directory_entry
                .file_name()
                .to_string_lossy()
                .starts_with(LOOPBACK_MODULE_OBJECT_FILE_NAME_PREFIX)
            {
                return true;
            }
            // Not followed through symlinks: a release's `build` and `source` link out to whole
            // kernel header trees, which hold no modules.
            if directory_entry
                .file_type()
                .is_ok_and(|entry_type| entry_type.is_dir())
            {
                directories_to_search.push(directory_entry.path());
            }
        }
    }
    false
}

/// `pkexec` under a desktop session, `sudo` in a headless shell, else whichever exists.
///
/// `pkexec` needs a polkit agent to put a password dialog on screen, which a session has and an
/// SSH shell does not; `sudo` prompts wherever there is a terminal.
pub(crate) fn choose_privilege_escalation_helper(
    find_executable_on_path: &dyn Fn(&str) -> Option<PathBuf>,
    read_environment_variable: &dyn Fn(&str) -> Option<OsString>,
) -> Option<ResolvedPrivilegeEscalationHelper> {
    let has_desktop_session =
        DESKTOP_SESSION_ENVIRONMENT_VARIABLES
            .iter()
            .any(|session_variable| {
                read_environment_variable(session_variable)
                    .is_some_and(|session_value| !session_value.is_empty())
            });
    let resolved_on_path = |privilege_escalation_helper: PrivilegeEscalationHelper| {
        find_executable_on_path(privilege_escalation_helper.executable_name()).map(
            |resolved_helper_executable| ResolvedPrivilegeEscalationHelper {
                privilege_escalation_helper,
                resolved_helper_executable,
            },
        )
    };
    if has_desktop_session
        && let Some(resolved_pkexec) = resolved_on_path(PrivilegeEscalationHelper::Pkexec)
    {
        return Some(resolved_pkexec);
    }
    resolved_on_path(PrivilegeEscalationHelper::Sudo)
        .or_else(|| resolved_on_path(PrivilegeEscalationHelper::Pkexec))
}

fn describe_privilege_escalation_helper_ending(helper_exit_status: ExitStatus) -> String {
    match (helper_exit_status.code(), helper_exit_status.signal()) {
        (Some(helper_exit_code), _) => format!("exit {helper_exit_code}"),
        (None, Some(terminating_signal)) => format!("killed by signal {terminating_signal}"),
        (None, None) => helper_exit_status.to_string(),
    }
}

/// `tatolab enable-virtual-camera --print`: write the grant files and the root commands to stdout
/// for a hand install, reading nothing of the machine and changing nothing.
pub(crate) fn print_virtual_camera_grant_for_hand_install() -> Result<u8, TatolabCommandFailure> {
    write_verb_standard_output(&render_virtual_camera_grant_for_hand_install())
}

/// `tatolab enable-virtual-camera`: install the loopback grant in one privileged step, then check
/// the control node opens read-write.
pub(crate) fn install_virtual_camera_grant_through_privilege_escalation_helper(
    grant_target_machine: &mut VirtualCameraGrantTargetMachine<'_>,
) -> Result<u8, TatolabCommandFailure> {
    if grant_target_machine.operating_system_name != LINUX_OPERATING_SYSTEM_NAME {
        return Err(TatolabCommandFailure::refused(format!(
            "`{ENABLE_VIRTUAL_CAMERA_VERB}` is Linux-only: the virtual camera is a \
             {LOOPBACK_MODULE_NAME} device, and this is {}.",
            grant_target_machine.operating_system_name
        )));
    }
    let kernel_release = &grant_target_machine.kernel_release;
    if !loopback_module_is_installed_for_kernel(
        &grant_target_machine.kernel_modules_root_directory,
        kernel_release,
    ) {
        return Err(TatolabCommandFailure::refused(format!(
            "the {LOOPBACK_MODULE_NAME} module is not installed for kernel {kernel_release}. \
             Install `v4l2loopback-dkms` (Debian/Ubuntu; it builds against the running kernel), \
             or on a kernel that ships the module, `linux-modules-{kernel_release}` — then re-run."
        )));
    }
    let Some(resolved_privilege_escalation_helper) = choose_privilege_escalation_helper(
        &*grant_target_machine.find_executable_on_path,
        &*grant_target_machine.read_environment_variable,
    ) else {
        return Err(TatolabCommandFailure::refused(format!(
            "neither `pkexec` nor `sudo` is available to run the one privileged step. Place the \
             files by hand instead: `{ENABLE_VIRTUAL_CAMERA_VERB} --print` writes them and the \
             commands to run as root."
        )));
    };
    let helper_executable_name = resolved_privilege_escalation_helper
        .privilege_escalation_helper
        .executable_name();
    write_verb_standard_output(&format!(
        "Installing the virtual camera permission via {helper_executable_name} — this is the one \
         privileged step, and it asks for your password.\n"
    ))?;
    let helper_exit_status = (grant_target_machine.run_privileged_script_through_helper)(
        &resolved_privilege_escalation_helper,
        &virtual_camera_grant_privileged_script(),
    )
    .map_err(|start_failure| {
        TatolabCommandFailure::refused(format!(
            "cannot start {helper_executable_name}: {start_failure}. Nothing was changed; \
             `--print` shows what it would have written."
        ))
    })?;
    if !helper_exit_status.success() {
        return Err(TatolabCommandFailure::refused(format!(
            "{helper_executable_name} did not complete the privileged step ({}). Nothing else \
             was changed; `--print` shows what it would have written.",
            describe_privilege_escalation_helper_ending(helper_exit_status)
        )));
    }
    let loopback_control_node_path = &grant_target_machine.loopback_control_node_path;
    if !loopback_control_node_path.exists() {
        return Err(TatolabCommandFailure::refused(format!(
            "{} did not appear after loading the module — `modinfo {LOOPBACK_MODULE_NAME}` and \
             `dmesg` say why.",
            loopback_control_node_path.display()
        )));
    }
    if !control_node_is_writable_by_this_user(loopback_control_node_path) {
        return Err(TatolabCommandFailure::refused(format!(
            "{} exists but this user still cannot open it read-write. The udev rule tags it \
             `uaccess`, which logind applies to the active seat: log out and back in, or if this \
             is an SSH session, run a graph from the desktop.",
            loopback_control_node_path.display()
        )));
    }
    write_verb_standard_output(&format!(
        "Done: {} is writable by this user. A VirtualCameraSink now creates its own camera; \
         re-running this command is harmless.\n",
        loopback_control_node_path.display()
    ))
}

#[cfg(test)]
mod tests;
