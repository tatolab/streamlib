// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `tatolab enable-virtual-camera`: the one-time grant behind the virtual camera's loopback door.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};

use crate::TatolabCommandFailure;

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
    print!("{}", render_virtual_camera_grant_for_hand_install());
    Ok(0)
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
    println!(
        "Installing the virtual camera permission via {helper_executable_name} — this is the one \
         privileged step, and it asks for your password."
    );
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
    println!(
        "Done: {} is writable by this user. A VirtualCameraSink now creates its own camera; \
         re-running this command is harmless.",
        loopback_control_node_path.display()
    );
    Ok(0)
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    use super::*;

    const SCRIPTED_KERNEL_RELEASE: &str = "9.9.9-test";

    /// A Linux machine with nothing installed, no helper on PATH, no session, and a runner that
    /// fails the test if anything is run; each scenario overrides what it is about.
    fn scripted_linux_machine<'machine>(
        scratch_directory: &Path,
    ) -> VirtualCameraGrantTargetMachine<'machine> {
        VirtualCameraGrantTargetMachine {
            operating_system_name: LINUX_OPERATING_SYSTEM_NAME.to_owned(),
            kernel_release: SCRIPTED_KERNEL_RELEASE.to_owned(),
            kernel_modules_root_directory: scratch_directory.join("lib-modules"),
            loopback_control_node_path: scratch_directory.join("absent-control-node"),
            find_executable_on_path: Box::new(|_executable_name| None),
            read_environment_variable: Box::new(|_variable_name| None),
            run_privileged_script_through_helper: Box::new(|_helper, _script| {
                panic!("this scenario must run nothing")
            }),
        }
    }

    fn install_scripted_loopback_module(
        kernel_modules_root_directory: &Path,
        kernel_release: &str,
        module_object_file_name: &str,
    ) {
        let dkms_module_directory = kernel_modules_root_directory
            .join(kernel_release)
            .join("updates/dkms");
        fs::create_dir_all(&dkms_module_directory).unwrap();
        fs::write(dkms_module_directory.join(module_object_file_name), b"").unwrap();
    }

    /// The directory [`executables_on_path`] resolves every helper into.
    const SCRIPTED_PATH_DIRECTORY: &str = "/opt/scripted-path";

    fn executables_on_path(
        available_executable_names: &'static [&'static str],
    ) -> ExecutableOnPathFinder<'static> {
        Box::new(move |executable_name| {
            available_executable_names
                .contains(&executable_name)
                .then(|| PathBuf::from(SCRIPTED_PATH_DIRECTORY).join(executable_name))
        })
    }

    /// `privilege_escalation_helper` as [`executables_on_path`] resolves it.
    fn resolved_on_the_scripted_path(
        privilege_escalation_helper: PrivilegeEscalationHelper,
    ) -> ResolvedPrivilegeEscalationHelper {
        ResolvedPrivilegeEscalationHelper {
            privilege_escalation_helper,
            resolved_helper_executable: PathBuf::from(SCRIPTED_PATH_DIRECTORY)
                .join(privilege_escalation_helper.executable_name()),
        }
    }

    fn environment_of(
        set_variables: &'static [(&'static str, &'static str)],
    ) -> EnvironmentVariableReader<'static> {
        Box::new(move |variable_name| {
            set_variables
                .iter()
                .find(|(set_name, _)| *set_name == variable_name)
                .map(|(_, set_value)| OsString::from(set_value))
        })
    }

    fn refusal_message(command_outcome: Result<u8, TatolabCommandFailure>) -> String {
        let command_failure = command_outcome.expect_err("the verb must refuse");
        assert_eq!(command_failure.exit_code, 1);
        command_failure
            .message_for_the_user
            .expect("a refusal names its reason")
    }

    fn make_fifo(fifo_path: &Path) {
        let fifo_path_c_string = CString::new(fifo_path.as_os_str().as_bytes()).unwrap();
        // SAFETY: the path is a NUL-terminated string that outlives the call.
        assert_eq!(
            unsafe { libc::mkfifo(fifo_path_c_string.as_ptr(), 0o600) },
            0,
            "mkfifo {}: {}",
            fifo_path.display(),
            io::Error::last_os_error()
        );
    }

    #[test]
    fn print_writes_the_three_files_and_the_root_commands_and_reads_no_machine_fact() {
        let rendered_grant = render_virtual_camera_grant_for_hand_install();
        for destination_path in [
            "/etc/modules-load.d/streamlib-virtual-camera.conf",
            "/etc/modprobe.d/streamlib-virtual-camera.conf",
            "/etc/udev/rules.d/70-streamlib-virtual-camera.rules",
        ] {
            assert!(
                rendered_grant.contains(&format!("# ---- {destination_path} ----\n")),
                "{destination_path} missing from:\n{rendered_grant}"
            );
        }
        for grant_file in &VIRTUAL_CAMERA_GRANT_FILES {
            assert!(rendered_grant.contains(grant_file.contents));
            assert!(
                grant_file
                    .contents
                    .starts_with("# Installed by `tatolab enable-virtual-camera`: "),
                "{}",
                grant_file.contents
            );
            assert!(!grant_file.contents.contains("StreamLib"));
        }
        assert!(rendered_grant.contains("\nv4l2loopback\n"));
        assert!(rendered_grant.contains("options v4l2loopback devices=0\n"));
        assert!(
            rendered_grant
                .contains("KERNEL==\"v4l2loopback\", SUBSYSTEM==\"misc\", TAG+=\"uaccess\"\n")
        );
        assert!(rendered_grant.ends_with(
            "# ---- then, as root ----\n\
             modprobe v4l2loopback devices=0\n\
             udevadm control --reload\n\
             udevadm trigger --subsystem-match=misc --sysname-match=v4l2loopback\n"
        ));

        assert_eq!(print_virtual_camera_grant_for_hand_install().unwrap(), 0);
    }

    #[test]
    fn the_helper_search_path_is_path_or_the_default_when_path_is_unset() {
        assert_eq!(
            executable_search_path_from(None),
            OsString::from("/bin:/usr/bin")
        );
        assert_eq!(
            executable_search_path_from(Some(OsString::from("/opt/helpers"))),
            OsString::from("/opt/helpers")
        );
        assert_eq!(
            executable_search_path_from(Some(OsString::new())),
            OsString::new(),
            "an empty PATH is set, and searches nothing"
        );
    }

    #[test]
    fn the_helper_is_found_as_an_executable_file_on_the_search_path_and_never_in_an_empty_one() {
        let scratch_directory = tempfile::tempdir().unwrap();
        let first_directory = scratch_directory.path().join("first");
        let second_directory = scratch_directory.path().join("second");
        fs::create_dir_all(&first_directory).unwrap();
        fs::create_dir_all(&second_directory).unwrap();
        fs::write(first_directory.join("sudo"), b"not executable").unwrap();
        fs::create_dir(first_directory.join("pkexec")).unwrap();
        let executable_sudo = second_directory.join("sudo");
        fs::write(&executable_sudo, b"#!/bin/sh\n").unwrap();
        fs::set_permissions(&executable_sudo, fs::Permissions::from_mode(0o755)).unwrap();
        let search_path = std::env::join_paths([&first_directory, &second_directory]).unwrap();

        assert_eq!(
            find_executable_on_search_path("sudo", &search_path),
            Some(executable_sudo)
        );
        assert_eq!(find_executable_on_search_path("pkexec", &search_path), None);
        assert_eq!(
            find_executable_on_search_path("sudo", std::ffi::OsStr::new("")),
            None
        );
    }

    #[test]
    fn the_privileged_script_writes_each_file_by_heredoc_then_loads_and_retriggers() {
        assert_eq!(
            virtual_camera_grant_privileged_script(),
            "set -eu\n\
             mkdir -p /etc/modules-load.d\n\
             cat > /etc/modules-load.d/streamlib-virtual-camera.conf <<'STREAMLIB_EOF'\n\
             # Installed by `tatolab enable-virtual-camera`: load the loopback module at boot.\n\
             v4l2loopback\n\
             STREAMLIB_EOF\n\
             mkdir -p /etc/modprobe.d\n\
             cat > /etc/modprobe.d/streamlib-virtual-camera.conf <<'STREAMLIB_EOF'\n\
             # Installed by `tatolab enable-virtual-camera`: no pre-made devices — each\n\
             # VirtualCameraSink creates and removes its own.\n\
             options v4l2loopback devices=0\n\
             STREAMLIB_EOF\n\
             mkdir -p /etc/udev/rules.d\n\
             cat > /etc/udev/rules.d/70-streamlib-virtual-camera.rules <<'STREAMLIB_EOF'\n\
             # Installed by `tatolab enable-virtual-camera`: the logged-in seat user may\n\
             # open the loopback control node, so a VirtualCameraSink can add a camera.\n\
             KERNEL==\"v4l2loopback\", SUBSYSTEM==\"misc\", TAG+=\"uaccess\"\n\
             STREAMLIB_EOF\n\
             modprobe v4l2loopback devices=0\n\
             udevadm control --reload\n\
             udevadm trigger --subsystem-match=misc --sysname-match=v4l2loopback\n\
             udevadm settle || true\n"
        );
    }

    #[test]
    fn refuses_by_name_without_pkexec_or_sudo() {
        let scratch_directory = tempfile::tempdir().unwrap();
        let mut grant_target_machine = scripted_linux_machine(scratch_directory.path());
        install_scripted_loopback_module(
            &grant_target_machine.kernel_modules_root_directory,
            SCRIPTED_KERNEL_RELEASE,
            "v4l2loopback.ko",
        );
        grant_target_machine.read_environment_variable = environment_of(&[("DISPLAY", ":1")]);

        let refusal = refusal_message(
            install_virtual_camera_grant_through_privilege_escalation_helper(
                &mut grant_target_machine,
            ),
        );
        assert!(
            refusal.contains("`pkexec`") && refusal.contains("`sudo`"),
            "{refusal}"
        );
        assert!(
            refusal.contains("`tatolab enable-virtual-camera --print`"),
            "the hand-install path is offered: {refusal}"
        );
    }

    #[test]
    fn refuses_by_name_off_linux() {
        let scratch_directory = tempfile::tempdir().unwrap();
        let mut grant_target_machine = scripted_linux_machine(scratch_directory.path());
        grant_target_machine.operating_system_name = "Darwin".to_owned();

        let refusal = refusal_message(
            install_virtual_camera_grant_through_privilege_escalation_helper(
                &mut grant_target_machine,
            ),
        );
        assert_eq!(
            refusal,
            "`tatolab enable-virtual-camera` is Linux-only: the virtual camera is a v4l2loopback \
             device, and this is Darwin."
        );
    }

    #[test]
    fn names_the_package_when_the_module_is_not_installed_for_the_running_kernel() {
        let scratch_directory = tempfile::tempdir().unwrap();
        let mut grant_target_machine = scripted_linux_machine(scratch_directory.path());
        install_scripted_loopback_module(
            &grant_target_machine.kernel_modules_root_directory,
            "1.0.0-another-kernel",
            "v4l2loopback.ko",
        );
        grant_target_machine.find_executable_on_path = executables_on_path(&["pkexec", "sudo"]);

        let refusal = refusal_message(
            install_virtual_camera_grant_through_privilege_escalation_helper(
                &mut grant_target_machine,
            ),
        );
        assert!(refusal.contains("v4l2loopback-dkms"), "{refusal}");
        assert!(refusal.contains("linux-modules-9.9.9-test"), "{refusal}");
    }

    #[test]
    fn the_module_is_found_compressed_anywhere_under_the_running_kernels_tree() {
        let scratch_directory = tempfile::tempdir().unwrap();
        let kernel_modules_root_directory = scratch_directory.path().join("lib-modules");
        assert!(!loopback_module_is_installed_for_kernel(
            &kernel_modules_root_directory,
            SCRIPTED_KERNEL_RELEASE
        ));
        install_scripted_loopback_module(
            &kernel_modules_root_directory,
            SCRIPTED_KERNEL_RELEASE,
            "v4l2loopback.ko.zst",
        );
        assert!(loopback_module_is_installed_for_kernel(
            &kernel_modules_root_directory,
            SCRIPTED_KERNEL_RELEASE
        ));
    }

    #[test]
    fn the_privilege_helper_prefers_pkexec_under_a_session_and_sudo_without_one() {
        let both_helpers = executables_on_path(&["pkexec", "sudo"]);
        let only_pkexec = executables_on_path(&["pkexec"]);
        let no_helper = executables_on_path(&[]);

        assert_eq!(
            choose_privilege_escalation_helper(
                &*both_helpers,
                &*environment_of(&[("DISPLAY", ":1")])
            ),
            Some(resolved_on_the_scripted_path(
                PrivilegeEscalationHelper::Pkexec
            ))
        );
        assert_eq!(
            choose_privilege_escalation_helper(
                &*both_helpers,
                &*environment_of(&[("WAYLAND_DISPLAY", "wayland-0")])
            ),
            Some(resolved_on_the_scripted_path(
                PrivilegeEscalationHelper::Pkexec
            ))
        );
        assert_eq!(
            choose_privilege_escalation_helper(&*both_helpers, &*environment_of(&[])),
            Some(resolved_on_the_scripted_path(
                PrivilegeEscalationHelper::Sudo
            ))
        );
        assert_eq!(
            choose_privilege_escalation_helper(
                &*both_helpers,
                &*environment_of(&[("DISPLAY", "")])
            ),
            Some(resolved_on_the_scripted_path(
                PrivilegeEscalationHelper::Sudo
            )),
            "an empty DISPLAY is no session"
        );
        assert_eq!(
            choose_privilege_escalation_helper(&*only_pkexec, &*environment_of(&[])),
            Some(resolved_on_the_scripted_path(
                PrivilegeEscalationHelper::Pkexec
            ))
        );
        assert_eq!(
            choose_privilege_escalation_helper(&*no_helper, &*environment_of(&[("DISPLAY", ":1")])),
            None
        );
    }

    #[test]
    fn the_control_node_probe_opens_a_character_device_like_path_without_seeking() {
        let scratch_directory = tempfile::tempdir().unwrap();
        let not_seekable_fifo = scratch_directory.path().join("not-seekable");
        make_fifo(&not_seekable_fifo);

        assert!(control_node_is_writable_by_this_user(&not_seekable_fifo));
        assert!(control_node_is_writable_by_this_user(Path::new(
            "/dev/null"
        )));
        assert!(!control_node_is_writable_by_this_user(
            &scratch_directory.path().join("absent")
        ));
    }

    #[test]
    fn installs_through_the_chosen_helper_and_succeeds_once_the_control_node_opens_read_write() {
        let scratch_directory = tempfile::tempdir().unwrap();
        let recorded_helper_runs: RefCell<Vec<(ResolvedPrivilegeEscalationHelper, String)>> =
            RefCell::new(Vec::new());
        let mut grant_target_machine = scripted_linux_machine(scratch_directory.path());
        install_scripted_loopback_module(
            &grant_target_machine.kernel_modules_root_directory,
            SCRIPTED_KERNEL_RELEASE,
            "v4l2loopback.ko",
        );
        grant_target_machine.find_executable_on_path = executables_on_path(&["pkexec", "sudo"]);
        grant_target_machine.read_environment_variable = environment_of(&[("DISPLAY", ":1")]);
        grant_target_machine.loopback_control_node_path = PathBuf::from("/dev/null");
        grant_target_machine.run_privileged_script_through_helper =
            Box::new(|resolved_privilege_escalation_helper, privileged_script| {
                recorded_helper_runs.borrow_mut().push((
                    resolved_privilege_escalation_helper.clone(),
                    privileged_script.to_owned(),
                ));
                Ok(ExitStatus::from_raw(0))
            });

        assert_eq!(
            install_virtual_camera_grant_through_privilege_escalation_helper(
                &mut grant_target_machine
            )
            .unwrap(),
            0
        );
        drop(grant_target_machine);
        assert_eq!(
            recorded_helper_runs.into_inner(),
            [(
                resolved_on_the_scripted_path(PrivilegeEscalationHelper::Pkexec),
                virtual_camera_grant_privileged_script()
            )]
        );
    }

    /// A relative or empty PATH entry never resolves a helper, even one that reaches `/bin/sh`.
    #[test]
    fn a_relative_or_empty_search_path_entry_never_resolves_a_helper() {
        let relative_entry_reaching_slash_bin = format!(
            "{}bin",
            "../".repeat(
                std::path::Path::new(".")
                    .canonicalize()
                    .unwrap()
                    .components()
                    .count()
                    + 1
            )
        );
        assert!(
            std::path::Path::new(&relative_entry_reaching_slash_bin)
                .join("sh")
                .is_file(),
            "the relative entry must reach /bin/sh for this test to mean anything"
        );
        assert_eq!(
            find_executable_on_search_path(
                "sh",
                std::ffi::OsStr::new(&format!(":{relative_entry_reaching_slash_bin}"))
            ),
            None
        );
        assert_eq!(
            find_executable_on_search_path("sh", std::ffi::OsStr::new("/bin")),
            Some(std::path::PathBuf::from("/bin/sh"))
        );
    }

    /// The helper runs from the file its lookup found, not by its name again: the stand-in sits in
    /// a directory this process's PATH does not hold.
    #[test]
    fn the_helper_runs_from_the_path_its_lookup_resolved() {
        let scratch_directory = tempfile::tempdir().unwrap();
        let stand_in_helper = scratch_directory.path().join("sudo");
        let recorded_arguments = scratch_directory
            .path()
            .join("arguments-the-helper-was-given");
        fs::write(
            &stand_in_helper,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\n",
                recorded_arguments.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&stand_in_helper, fs::Permissions::from_mode(0o755)).unwrap();
        let resolved_stand_in_helper = ResolvedPrivilegeEscalationHelper {
            privilege_escalation_helper: PrivilegeEscalationHelper::Sudo,
            resolved_helper_executable: stand_in_helper,
        };

        // Another test's fork can briefly hold the just-written file open, so exec answers
        // ETXTBSY until that child execs in turn.
        let mut busy_executable_retries_left = 500;
        let helper_exit_status = loop {
            match run_privileged_script_through_helper_on_this_terminal(
                &resolved_stand_in_helper,
                "true",
            ) {
                Err(start_failure)
                    if start_failure.raw_os_error() == Some(libc::ETXTBSY)
                        && busy_executable_retries_left > 0 =>
                {
                    busy_executable_retries_left -= 1;
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                started => break started.unwrap(),
            }
        };

        assert!(helper_exit_status.success());
        assert_eq!(
            fs::read_to_string(&recorded_arguments).unwrap(),
            "sh\n-c\ntrue\n"
        );
    }

    #[test]
    fn refuses_naming_the_exit_code_when_the_helper_fails() {
        let scratch_directory = tempfile::tempdir().unwrap();
        let mut grant_target_machine = scripted_linux_machine(scratch_directory.path());
        install_scripted_loopback_module(
            &grant_target_machine.kernel_modules_root_directory,
            SCRIPTED_KERNEL_RELEASE,
            "v4l2loopback.ko",
        );
        grant_target_machine.find_executable_on_path = executables_on_path(&["sudo"]);
        grant_target_machine.run_privileged_script_through_helper =
            Box::new(|_helper, _script| Ok(ExitStatus::from_raw(126 << 8)));

        let refusal = refusal_message(
            install_virtual_camera_grant_through_privilege_escalation_helper(
                &mut grant_target_machine,
            ),
        );
        assert!(
            refusal.starts_with("sudo did not complete the privileged step (exit 126)."),
            "{refusal}"
        );
        assert!(refusal.contains("`--print`"), "{refusal}");
    }

    #[test]
    fn refuses_when_the_control_node_did_not_appear() {
        let scratch_directory = tempfile::tempdir().unwrap();
        let mut grant_target_machine = scripted_linux_machine(scratch_directory.path());
        install_scripted_loopback_module(
            &grant_target_machine.kernel_modules_root_directory,
            SCRIPTED_KERNEL_RELEASE,
            "v4l2loopback.ko",
        );
        grant_target_machine.find_executable_on_path = executables_on_path(&["sudo"]);
        grant_target_machine.run_privileged_script_through_helper =
            Box::new(|_helper, _script| Ok(ExitStatus::from_raw(0)));

        let refusal = refusal_message(
            install_virtual_camera_grant_through_privilege_escalation_helper(
                &mut grant_target_machine,
            ),
        );
        assert!(
            refusal.contains("absent-control-node did not appear after loading the module"),
            "{refusal}"
        );
        assert!(refusal.contains("`modinfo v4l2loopback`"), "{refusal}");
    }

    #[test]
    fn refuses_when_the_control_node_still_does_not_open_read_write() {
        let scratch_directory = tempfile::tempdir().unwrap();
        let mut grant_target_machine = scripted_linux_machine(scratch_directory.path());
        install_scripted_loopback_module(
            &grant_target_machine.kernel_modules_root_directory,
            SCRIPTED_KERNEL_RELEASE,
            "v4l2loopback.ko",
        );
        grant_target_machine.find_executable_on_path = executables_on_path(&["sudo"]);
        // A directory refuses a read-write open even to root.
        grant_target_machine.loopback_control_node_path = scratch_directory.path().to_path_buf();
        grant_target_machine.run_privileged_script_through_helper =
            Box::new(|_helper, _script| Ok(ExitStatus::from_raw(0)));

        let refusal = refusal_message(
            install_virtual_camera_grant_through_privilege_escalation_helper(
                &mut grant_target_machine,
            ),
        );
        assert!(
            refusal.contains("exists but this user still cannot open it read-write"),
            "{refusal}"
        );
        assert!(refusal.contains("log out and back in"), "{refusal}");
    }

    /// The rig check: the re-trigger names the module's misc device, so the freshly written
    /// `uaccess` rule applies to a node that already exists. `--dry-run` touches nothing.
    #[test]
    fn the_udev_trigger_selects_the_control_node() {
        if !cfg!(target_os = "linux") {
            eprintln!("skipped: v4l2loopback and udev are Linux");
            return;
        }
        let loopback_module_is_loaded = fs::read_to_string("/proc/modules")
            .is_ok_and(|loaded_modules| loaded_modules.contains(LOOPBACK_MODULE_NAME));
        if !loopback_module_is_loaded {
            eprintln!("skipped: v4l2loopback is not loaded here");
            return;
        }
        let privileged_script = virtual_camera_grant_privileged_script();
        let trigger_line = privileged_script
            .lines()
            .find(|script_line| script_line.starts_with("udevadm trigger"))
            .unwrap();
        let trigger_words: Vec<&str> = trigger_line.split_whitespace().collect();

        let dry_run_output = Command::new(trigger_words[0])
            .arg(trigger_words[1])
            .args(["--dry-run", "--verbose"])
            .args(&trigger_words[2..])
            .output()
            .unwrap();
        assert!(
            dry_run_output.status.success(),
            "{}",
            String::from_utf8_lossy(&dry_run_output.stderr)
        );
        let triggered_devices = String::from_utf8_lossy(&dry_run_output.stdout);
        assert!(
            triggered_devices.contains("/sys/devices/virtual/misc/v4l2loopback"),
            "the trigger selects nothing; command: {trigger_line}; output: {triggered_devices:?}"
        );
    }
}
