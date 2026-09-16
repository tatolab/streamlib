// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Bans the raw `libc` calls that create a file descriptor a spawned process
//! would inherit: `dup` and `pipe` outright, and `pipe2`, `epoll_create1`,
//! `timerfd_create`, `eventfd` and `recvmsg` without their close-on-exec flag.
//!
//! `docs/plan/ARCHITECTURE.md` §Processor model: nothing an app starts may hold
//! the app's output open or delay its exit. A descriptor created without
//! close-on-exec reaches every child the process ever spawns — the stdio
//! interceptor's `dup`s of the app's own stdout and stderr did, and a grandchild
//! holding one kept anything reading the app's output waiting after the app had
//! died. So close-on-exec is set where the descriptor is born, atomically, and
//! never afterwards with `fcntl`: a spawn on another thread can land between the
//! two calls.
//!
//! `open`, `openat`, `memfd_create`, `socket`, `socketpair` and `accept` are
//! not gated.
//!
//! Cheap substring scan (no `syn`/compile) over every Rust file git knows under
//! `runtime/`, `sdk/` and `adapters/`, test code included — a test that leaks an
//! inheritable pipe into a child it spawns hangs on the same shape. Whole-line
//! comments are skipped. There is no per-line pragma: a test that means a child
//! to inherit a descriptor clears the flag on it by name.

use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

use crate::source_call_site_scan::{blank_out_lines, call_sites_of, is_a_whole_line_comment};

/// Workspace trees whose descriptor creation this gate owns.
const SCAN_ROOTS: &[&str] = &["runtime", "sdk", "adapters"];

/// A `libc` call that creates a descriptor, and what makes it close-on-exec.
struct DescriptorCreatingCall {
    callee: &'static str,
    /// The flag the call must carry, or `None` when the call has no
    /// close-on-exec form at all and is refused outright.
    close_on_exec_flag: Option<&'static str>,
    close_on_exec_spelling: &'static str,
}

const DESCRIPTOR_CREATING_CALLS: &[DescriptorCreatingCall] = &[
    DescriptorCreatingCall {
        callee: "libc::dup",
        close_on_exec_flag: None,
        close_on_exec_spelling: "libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0)",
    },
    DescriptorCreatingCall {
        callee: "libc::pipe",
        close_on_exec_flag: None,
        close_on_exec_spelling: "libc::pipe2(fds, libc::O_CLOEXEC)",
    },
    DescriptorCreatingCall {
        callee: "libc::pipe2",
        close_on_exec_flag: Some("O_CLOEXEC"),
        close_on_exec_spelling: "libc::pipe2(fds, libc::O_CLOEXEC)",
    },
    DescriptorCreatingCall {
        callee: "libc::epoll_create",
        close_on_exec_flag: None,
        close_on_exec_spelling: "libc::epoll_create1(libc::EPOLL_CLOEXEC)",
    },
    DescriptorCreatingCall {
        callee: "libc::epoll_create1",
        close_on_exec_flag: Some("EPOLL_CLOEXEC"),
        close_on_exec_spelling: "libc::epoll_create1(libc::EPOLL_CLOEXEC)",
    },
    DescriptorCreatingCall {
        callee: "libc::timerfd_create",
        close_on_exec_flag: Some("TFD_CLOEXEC"),
        close_on_exec_spelling: "libc::timerfd_create(clock, libc::TFD_CLOEXEC | …)",
    },
    DescriptorCreatingCall {
        callee: "libc::eventfd",
        close_on_exec_flag: Some("EFD_CLOEXEC"),
        close_on_exec_spelling: "libc::eventfd(initial, libc::EFD_CLOEXEC | …)",
    },
    DescriptorCreatingCall {
        callee: "libc::recvmsg",
        close_on_exec_flag: Some("MSG_CMSG_CLOEXEC"),
        close_on_exec_spelling: "libc::recvmsg(socket, &mut message, libc::MSG_CMSG_CLOEXEC)",
    },
];

#[derive(Debug, PartialEq, Eq)]
pub struct InheritableDescriptorViolation {
    pub file: PathBuf,
    pub line: usize,
    pub call_text: String,
    pub close_on_exec_spelling: &'static str,
}

#[derive(Debug, Default)]
pub struct InheritableDescriptorScanReport {
    pub violations: Vec<InheritableDescriptorViolation>,
    pub files_scanned: usize,
    pub files_scanned_per_scan_root: Vec<(&'static str, usize)>,
}

pub fn run(workspace_root: &Path) -> Result<()> {
    let report = scan(workspace_root)?;
    crate::ensure_source_walking_gate_read_source(
        "check-no-inheritable-descriptor",
        &format!("{SCAN_ROOTS:?}"),
        report.files_scanned,
        "a descriptor every spawned process inherits",
    )?;
    crate::ensure_every_source_walking_gate_scan_root_contributed(
        "check-no-inheritable-descriptor",
        &report.files_scanned_per_scan_root,
    )?;

    let failure_lines: Vec<String> = report
        .violations
        .iter()
        .map(|violation| {
            format!(
                "  {}:{}: `{}` creates a descriptor every process this one spawns inherits, \
                 so a grandchild can hold it open past this process's exit. Create it \
                 close-on-exec: `{}`.",
                violation.file.display(),
                violation.line,
                violation.call_text,
                violation.close_on_exec_spelling,
            )
        })
        .collect();
    anyhow::ensure!(
        failure_lines.is_empty(),
        "check-no-inheritable-descriptor found {} inheritable descriptor(s) created:\n{}",
        failure_lines.len(),
        failure_lines.join("\n"),
    );

    tracing::info!(
        "check-no-inheritable-descriptor: {} Rust file(s) scanned, every raw descriptor is \
         created close-on-exec",
        report.files_scanned,
    );
    Ok(())
}

pub fn scan(workspace_root: &Path) -> Result<InheritableDescriptorScanReport> {
    let mut tracked_rust_files = Vec::new();
    for root in SCAN_ROOTS {
        tracked_rust_files.extend(
            crate::list_repository_files_under(workspace_root, root)?
                .into_iter()
                .filter(|path| path.ends_with(".rs"))
                .map(PathBuf::from),
        );
    }
    scan_files(workspace_root, &tracked_rust_files)
}

pub fn scan_files(
    workspace_root: &Path,
    relative_paths: &[PathBuf],
) -> Result<InheritableDescriptorScanReport> {
    let mut report = InheritableDescriptorScanReport {
        files_scanned_per_scan_root: SCAN_ROOTS.iter().map(|root| (*root, 0)).collect(),
        ..Default::default()
    };

    for relative_path in relative_paths {
        let path = workspace_root.join(relative_path);
        // `git ls-files --cached` lists a file deleted from the worktree but not
        // yet from the index.
        if !path.is_file() {
            continue;
        }
        let Some(scan_root_count) = report
            .files_scanned_per_scan_root
            .iter_mut()
            .find(|(root, _)| relative_path.starts_with(root))
        else {
            continue;
        };
        scan_root_count.1 += 1;
        let body = fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", relative_path.display()))?;
        report.files_scanned += 1;

        report
            .violations
            .extend(inheritable_descriptor_calls(relative_path, &body));
    }
    Ok(report)
}

/// Every descriptor-creating call in `body` that is not close-on-exec, in line
/// order.
fn inheritable_descriptor_calls(
    relative_path: &Path,
    body: &str,
) -> Vec<InheritableDescriptorViolation> {
    let code = blank_out_lines(body, is_a_whole_line_comment);
    let mut violations: Vec<InheritableDescriptorViolation> = DESCRIPTOR_CREATING_CALLS
        .iter()
        .flat_map(|descriptor_creating_call| {
            call_sites_of(&code, descriptor_creating_call.callee)
                .into_iter()
                .filter(|call_site| {
                    !descriptor_creating_call
                        .close_on_exec_flag
                        .is_some_and(|flag| call_site.argument_text.contains(flag))
                })
                .map(|call_site| InheritableDescriptorViolation {
                    file: relative_path.to_path_buf(),
                    line: call_site.line,
                    call_text: call_site.collapsed_call_text,
                    close_on_exec_spelling: descriptor_creating_call.close_on_exec_spelling,
                })
        })
        .collect();
    violations.sort_by_key(|violation| violation.line);
    violations
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Every root gets a file, so a fixture exercises the shape the gate
    /// asserts on the real tree.
    fn scan_one_engine_file(body: &str) -> InheritableDescriptorScanReport {
        let tmp = TempDir::new().unwrap();
        let mut relative_paths = Vec::new();
        for (index, root) in SCAN_ROOTS.iter().enumerate() {
            let relative_path = PathBuf::from(format!("{root}/src/file_{index}.rs"));
            let file_body = if index == 0 { body } else { "pub fn ok() {}\n" };
            write(tmp.path(), &relative_path, file_body);
            relative_paths.push(relative_path);
        }
        scan_files(tmp.path(), &relative_paths).unwrap()
    }

    fn write(root: &Path, relative_path: &Path, body: &str) {
        let path = root.join(relative_path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, body).unwrap();
    }

    #[test]
    fn a_plain_dup_of_the_apps_stdout_is_refused_naming_the_close_on_exec_form() {
        let report = scan_one_engine_file(
            "fn dup_fd(fd: libc::c_int) -> i32 {\n    unsafe { libc::dup(fd) }\n}\n",
        );
        assert_eq!(report.violations.len(), 1, "got {:?}", report.violations);
        assert_eq!(report.violations[0].line, 2);
        assert_eq!(report.violations[0].call_text, "libc::dup(fd)");
        assert!(
            report.violations[0]
                .close_on_exec_spelling
                .contains("F_DUPFD_CLOEXEC")
        );
    }

    #[test]
    fn a_plain_pipe_is_refused_and_a_close_on_exec_pipe2_is_not() {
        let report = scan_one_engine_file(
            "unsafe { libc::pipe(fds.as_mut_ptr()) };\n\
             unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) };\n\
             unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_NONBLOCK) };\n",
        );
        let refused_lines: Vec<usize> = report.violations.iter().map(|v| v.line).collect();
        assert_eq!(refused_lines, vec![1, 3], "got {:?}", report.violations);
    }

    #[test]
    fn an_epoll_created_without_its_flag_is_refused() {
        let report = scan_one_engine_file(
            "let a = unsafe { libc::epoll_create1(0) };\n\
             let b = unsafe { libc::epoll_create1(libc::EPOLL_CLOEXEC) };\n\
             let c = unsafe { libc::epoll_create(1) };\n",
        );
        let refused_lines: Vec<usize> = report.violations.iter().map(|v| v.line).collect();
        assert_eq!(refused_lines, vec![1, 3], "got {:?}", report.violations);
    }

    #[test]
    fn a_timerfd_created_non_blocking_but_not_close_on_exec_is_refused() {
        let report = scan_one_engine_file(
            "let timer = unsafe {\n    libc::timerfd_create(\n        libc::CLOCK_MONOTONIC,\n        \
             libc::TFD_NONBLOCK,\n    )\n};\n",
        );
        assert_eq!(report.violations.len(), 1, "got {:?}", report.violations);
        assert_eq!(report.violations[0].line, 2);
        assert_eq!(
            report.violations[0].call_text,
            "libc::timerfd_create( libc::CLOCK_MONOTONIC, libc::TFD_NONBLOCK, )"
        );
    }

    #[test]
    fn a_multi_line_timerfd_carrying_its_flag_is_accepted() {
        let report = scan_one_engine_file(
            "let timer = unsafe {\n    libc::timerfd_create(\n        libc::CLOCK_MONOTONIC,\n        \
             libc::TFD_CLOEXEC | libc::TFD_NONBLOCK,\n    )\n};\n",
        );
        assert!(report.violations.is_empty(), "got {:?}", report.violations);
    }

    #[test]
    fn descriptors_received_over_a_socket_must_arrive_close_on_exec() {
        let report = scan_one_engine_file(
            "let n = unsafe { libc::recvmsg(stream.as_raw_fd(), &mut msg, 0) };\n\
             let m = unsafe { libc::recvmsg(stream.as_raw_fd(), &mut msg, libc::MSG_CMSG_CLOEXEC) };\n",
        );
        let refused_lines: Vec<usize> = report.violations.iter().map(|v| v.line).collect();
        assert_eq!(refused_lines, vec![1], "got {:?}", report.violations);
    }

    #[test]
    fn an_eventfd_without_its_flag_is_refused() {
        let report = scan_one_engine_file(
            "let a = unsafe { libc::eventfd(0, 0) };\n\
             let b = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC) };\n",
        );
        let refused_lines: Vec<usize> = report.violations.iter().map(|v| v.line).collect();
        assert_eq!(refused_lines, vec![1], "got {:?}", report.violations);
    }

    #[test]
    fn a_comment_naming_the_banned_call_is_not_read() {
        let report = scan_one_engine_file(
            "// `libc::dup(fd)` leaked the app's stdout into every child.\n\
             /// Never `libc::pipe(fds)`.\n\
             pub fn ok() {}\n",
        );
        assert!(report.violations.is_empty(), "got {:?}", report.violations);
    }

    #[test]
    fn dup2_onto_a_standard_stream_is_not_refused() {
        // `dup2` onto fd 1 or 2 is how a stream is redirected, and a standard
        // stream is meant to be inherited.
        let report =
            scan_one_engine_file("unsafe { libc::dup2(pipe_write, libc::STDOUT_FILENO) };\n");
        assert!(report.violations.is_empty(), "got {:?}", report.violations);
    }

    #[test]
    fn a_tree_where_one_scan_root_read_nothing_is_refused() {
        let tmp = TempDir::new().unwrap();
        let relative_path = PathBuf::from(format!("{}/src/lib.rs", SCAN_ROOTS[0]));
        write(tmp.path(), &relative_path, "pub fn ok() {}\n");
        let report = scan_files(tmp.path(), &[relative_path]).unwrap();
        let refusal = crate::ensure_every_source_walking_gate_scan_root_contributed(
            "check-no-inheritable-descriptor",
            &report.files_scanned_per_scan_root,
        )
        .unwrap_err();
        assert!(refusal.to_string().contains(SCAN_ROOTS[1]), "got {refusal}");
    }
}
