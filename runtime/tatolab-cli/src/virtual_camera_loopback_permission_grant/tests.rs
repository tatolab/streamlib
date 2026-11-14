// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

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

    let refusal = TatolabCommandFailure::refusal_message_of(
        install_virtual_camera_grant_through_privilege_escalation_helper(&mut grant_target_machine),
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

    let refusal = TatolabCommandFailure::refusal_message_of(
        install_virtual_camera_grant_through_privilege_escalation_helper(&mut grant_target_machine),
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

    let refusal = TatolabCommandFailure::refusal_message_of(
        install_virtual_camera_grant_through_privilege_escalation_helper(&mut grant_target_machine),
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
        choose_privilege_escalation_helper(&*both_helpers, &*environment_of(&[("DISPLAY", ":1")])),
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
        choose_privilege_escalation_helper(&*both_helpers, &*environment_of(&[("DISPLAY", "")])),
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
        install_virtual_camera_grant_through_privilege_escalation_helper(&mut grant_target_machine)
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

    let refusal = TatolabCommandFailure::refusal_message_of(
        install_virtual_camera_grant_through_privilege_escalation_helper(&mut grant_target_machine),
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

    let refusal = TatolabCommandFailure::refusal_message_of(
        install_virtual_camera_grant_through_privilege_escalation_helper(&mut grant_target_machine),
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

    let refusal = TatolabCommandFailure::refusal_message_of(
        install_virtual_camera_grant_through_privilege_escalation_helper(&mut grant_target_machine),
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
