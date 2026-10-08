// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Every program the grant could run; each stand-in only records that it ran.
const PROGRAMS_THE_GRANT_COULD_RUN: [&str; 5] = ["pkexec", "sudo", "sh", "modprobe", "udevadm"];

const PRINTED_VIRTUAL_CAMERA_GRANT: &str = r#"# ---- /etc/modules-load.d/streamlib-virtual-camera.conf ----
# Installed by `tatolab enable-virtual-camera`: load the loopback module at boot.
v4l2loopback

# ---- /etc/modprobe.d/streamlib-virtual-camera.conf ----
# Installed by `tatolab enable-virtual-camera`: no pre-made devices — each
# VirtualCameraSink creates and removes its own.
options v4l2loopback devices=0

# ---- /etc/udev/rules.d/70-streamlib-virtual-camera.rules ----
# Installed by `tatolab enable-virtual-camera`: the logged-in seat user may
# open the loopback control node, so a VirtualCameraSink can add a camera.
KERNEL=="v4l2loopback", SUBSYSTEM=="misc", TAG+="uaccess"

# ---- then, as root ----
modprobe v4l2loopback devices=0
udevadm control --reload
udevadm trigger --subsystem-match=misc --sysname-match=v4l2loopback
"#;

/// A PATH directory whose every grant program, when run, leaves a `<program>-ran` file behind.
struct RecordingStandInPath {
    stand_in_directory: tempfile::TempDir,
}

impl RecordingStandInPath {
    fn new() -> Self {
        let stand_in_directory = tempfile::tempdir().unwrap();
        for program_name in PROGRAMS_THE_GRANT_COULD_RUN {
            let stand_in_program = stand_in_directory.path().join(program_name);
            fs::write(
                &stand_in_program,
                format!(
                    "#!/bin/sh\n: > '{}'\nexit 0\n",
                    ran_marker_path(stand_in_directory.path(), program_name).display()
                ),
            )
            .unwrap();
            fs::set_permissions(&stand_in_program, fs::Permissions::from_mode(0o755)).unwrap();
        }
        Self { stand_in_directory }
    }

    fn programs_that_ran(&self) -> Vec<&'static str> {
        PROGRAMS_THE_GRANT_COULD_RUN
            .into_iter()
            .filter(|program_name| {
                ran_marker_path(self.stand_in_directory.path(), program_name).exists()
            })
            .collect()
    }

    fn run_tatolab(&self, tatolab_arguments: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_tatolab"))
            .args(tatolab_arguments)
            .env("PATH", self.stand_in_directory.path())
            .env("DISPLAY", ":1")
            .output()
            .unwrap()
    }
}

fn ran_marker_path(stand_in_directory: &Path, program_name: &str) -> PathBuf {
    stand_in_directory.join(format!("{program_name}-ran"))
}

#[test]
fn print_writes_the_three_files_and_the_root_commands_and_runs_nothing() {
    let recording_stand_in_path = RecordingStandInPath::new();

    let print_output = recording_stand_in_path.run_tatolab(&["enable-virtual-camera", "--print"]);

    assert!(
        print_output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&print_output.stderr)
    );
    assert_eq!(
        String::from_utf8(print_output.stdout).unwrap(),
        PRINTED_VIRTUAL_CAMERA_GRANT
    );
    assert_eq!(String::from_utf8(print_output.stderr).unwrap(), "");
    assert_eq!(
        recording_stand_in_path.programs_that_ran(),
        Vec::<&str>::new()
    );
}

#[test]
fn enable_virtual_camera_help_describes_the_grant_and_the_print_flag() {
    let help_output = Command::new(env!("CARGO_BIN_EXE_tatolab"))
        .args(["enable-virtual-camera", "--help"])
        .output()
        .unwrap();

    assert!(help_output.status.success());
    let help_text = String::from_utf8(help_output.stdout).unwrap();
    assert!(
        help_text.contains(
            "Install the standard grant behind the virtual camera's loopback door: load \
             v4l2loopback with no devices"
        ),
        "{help_text}"
    );
    assert!(help_text.contains("--print"), "{help_text}");
    assert!(
        help_text.contains(
            "Write the three files' contents and the commands to stdout and change nothing"
        ),
        "{help_text}"
    );
}

#[cfg(not(target_os = "linux"))]
#[test]
fn refuses_by_name_off_linux_and_runs_nothing() {
    let recording_stand_in_path = RecordingStandInPath::new();

    let refused_output = recording_stand_in_path.run_tatolab(&["enable-virtual-camera"]);

    assert_eq!(refused_output.status.code(), Some(1));
    let refusal = String::from_utf8(refused_output.stderr).unwrap();
    assert!(
        refusal.starts_with(
            "error: `tatolab enable-virtual-camera` is Linux-only: the virtual camera is a \
             v4l2loopback device, and this is "
        ),
        "{refusal}"
    );
    assert_eq!(
        recording_stand_in_path.programs_that_ran(),
        Vec::<&str>::new()
    );
}

/// The rig check: the verb, run for real, leaves the control node openable read-write by this
/// user in this same session — no re-login. It asks for a password, so it is opt-in.
#[cfg(target_os = "linux")]
#[test]
#[allow(clippy::disallowed_macros)]
fn enable_virtual_camera_makes_the_control_node_writable() {
    if std::env::var_os("TATOLAB_RUN_PRIVILEGED_VERB").is_none_or(|opt_in| opt_in != "1") {
        eprintln!(
            "skipped: runs the privileged verb (a password prompt); set \
             TATOLAB_RUN_PRIVILEGED_VERB=1 in a terminal to opt in"
        );
        return;
    }

    let enable_status = Command::new(env!("CARGO_BIN_EXE_tatolab"))
        .arg("enable-virtual-camera")
        .status()
        .unwrap();

    assert!(enable_status.success());
    assert!(
        fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/v4l2loopback")
            .is_ok()
    );
}
