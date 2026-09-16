// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Bans a raw `libc` call that creates a file descriptor a spawned process
//! would inherit.
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
//! Cheap substring scan (no `syn`/compile) over every tracked Rust file under
//! `runtime/`, `sdk/` and `adapters/`, test code included — a test that leaks
//! an inheritable pipe into a child it spawns hangs on the same shape. Whole-line
//! comments are skipped. There is no per-line pragma: no descriptor in the tree
//! is meant to be inherited by accident, and the one a helper is owed has its
//! flag cleared deliberately in `pre_exec`.

use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

/// Workspace trees whose descriptor creation this gate owns.
const SCAN_ROOTS: &[&str] = &["runtime", "sdk", "adapters"];

/// A `libc` call that creates a descriptor, and what makes it close-on-exec.
struct DescriptorCreatingCall {
    call_prefix: &'static str,
    /// The flag the call must carry, or `None` when the call has no
    /// close-on-exec form at all and is refused outright.
    close_on_exec_flag: Option<&'static str>,
    close_on_exec_spelling: &'static str,
}

const DESCRIPTOR_CREATING_CALLS: &[DescriptorCreatingCall] = &[
    DescriptorCreatingCall {
        call_prefix: "libc::dup(",
        close_on_exec_flag: None,
        close_on_exec_spelling: "libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0)",
    },
    DescriptorCreatingCall {
        call_prefix: "libc::pipe(",
        close_on_exec_flag: None,
        close_on_exec_spelling: "libc::pipe2(fds, libc::O_CLOEXEC)",
    },
    DescriptorCreatingCall {
        call_prefix: "libc::pipe2(",
        close_on_exec_flag: Some("O_CLOEXEC"),
        close_on_exec_spelling: "libc::pipe2(fds, libc::O_CLOEXEC)",
    },
    DescriptorCreatingCall {
        call_prefix: "libc::epoll_create(",
        close_on_exec_flag: None,
        close_on_exec_spelling: "libc::epoll_create1(libc::EPOLL_CLOEXEC)",
    },
    DescriptorCreatingCall {
        call_prefix: "libc::epoll_create1(",
        close_on_exec_flag: Some("EPOLL_CLOEXEC"),
        close_on_exec_spelling: "libc::epoll_create1(libc::EPOLL_CLOEXEC)",
    },
    DescriptorCreatingCall {
        call_prefix: "libc::timerfd_create(",
        close_on_exec_flag: Some("TFD_CLOEXEC"),
        close_on_exec_spelling: "libc::timerfd_create(clock, libc::TFD_CLOEXEC | …)",
    },
    DescriptorCreatingCall {
        call_prefix: "libc::eventfd(",
        close_on_exec_flag: Some("EFD_CLOEXEC"),
        close_on_exec_spelling: "libc::eventfd(initial, libc::EFD_CLOEXEC | …)",
    },
    DescriptorCreatingCall {
        call_prefix: "libc::recvmsg(",
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
    ensure_every_scan_root_contributed(&report)?;

    if report.violations.is_empty() {
        println!(
            "✓ check-no-inheritable-descriptor: {} Rust file(s) scanned, every raw \
             descriptor is created close-on-exec",
            report.files_scanned,
        );
        return Ok(());
    }

    eprintln!(
        "✗ check-no-inheritable-descriptor: {} violation(s)",
        report.violations.len()
    );
    for violation in &report.violations {
        eprintln!(
            "  {}:{}: `{}` creates a descriptor every process this one spawns inherits, \
             so a grandchild can hold it open past this process's exit. Create it \
             close-on-exec: `{}`.",
            violation.file.display(),
            violation.line,
            violation.call_text,
            violation.close_on_exec_spelling,
        );
    }
    anyhow::bail!(
        "check-no-inheritable-descriptor: {} inheritable descriptor(s) created",
        report.violations.len()
    );
}

/// A renamed or moved root would leave the others carrying the whole gate,
/// which reads identically to a clean tree.
fn ensure_every_scan_root_contributed(report: &InheritableDescriptorScanReport) -> Result<()> {
    for (root, files_scanned) in &report.files_scanned_per_scan_root {
        anyhow::ensure!(
            *files_scanned > 0,
            "check-no-inheritable-descriptor scanned 0 files under {root} — that scan root \
             moved out from under the gate"
        );
    }
    Ok(())
}

pub fn scan(workspace_root: &Path) -> Result<InheritableDescriptorScanReport> {
    let tracked = tracked_rust_files_under_scan_roots(workspace_root)?;
    scan_files(workspace_root, &tracked)
}

/// Workspace-relative Rust paths git tracks under the scan roots.
///
/// A filesystem walk would descend build trees and virtualenvs, gating
/// third-party sources the project does not own.
fn tracked_rust_files_under_scan_roots(workspace_root: &Path) -> Result<Vec<PathBuf>> {
    let output = std::process::Command::new("git")
        .args(["ls-files", "-z", "--"])
        .args(SCAN_ROOTS)
        .current_dir(workspace_root)
        .output()
        .context("failed to run `git ls-files` for check-no-inheritable-descriptor")?;

    anyhow::ensure!(
        output.status.success(),
        "`git ls-files` failed ({}) — check-no-inheritable-descriptor cannot enumerate its \
         scan roots",
        output.status
    );

    let listing =
        String::from_utf8(output.stdout).context("`git ls-files` emitted a non-UTF-8 path")?;
    Ok(listing
        .split('\0')
        .filter(|path| path.ends_with(".rs"))
        .map(PathBuf::from)
        .collect())
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
        let Some(scan_root_count) = report
            .files_scanned_per_scan_root
            .iter_mut()
            .find(|(root, _)| relative_path.starts_with(root))
        else {
            continue;
        };
        scan_root_count.1 += 1;

        let body = fs::read_to_string(workspace_root.join(relative_path))
            .with_context(|| format!("failed to read {}", relative_path.display()))?;
        report.files_scanned += 1;

        for (line, call_text, close_on_exec_spelling) in inheritable_descriptor_calls(&body) {
            report.violations.push(InheritableDescriptorViolation {
                file: relative_path.clone(),
                line,
                call_text,
                close_on_exec_spelling,
            });
        }
    }
    Ok(report)
}

/// Every descriptor-creating call in `body` that is not close-on-exec, as
/// `(1-based line, whitespace-collapsed call text, the close-on-exec spelling)`.
fn inheritable_descriptor_calls(body: &str) -> Vec<(usize, String, &'static str)> {
    let code = blank_out_comment_lines(body);
    let mut calls = Vec::new();
    for descriptor_creating_call in DESCRIPTOR_CREATING_CALLS {
        let mut search_from = 0usize;
        while let Some(offset) = code[search_from..].find(descriptor_creating_call.call_prefix) {
            let call_start = search_from + offset;
            let open_paren = call_start + descriptor_creating_call.call_prefix.len() - 1;
            let Some(close_paren) = matching_close_paren(&code, open_paren) else {
                break;
            };
            let arguments = &code[open_paren + 1..close_paren];
            let is_close_on_exec = descriptor_creating_call
                .close_on_exec_flag
                .is_some_and(|flag| arguments.contains(flag));
            if !is_close_on_exec {
                calls.push((
                    code[..call_start].matches('\n').count() + 1,
                    code[call_start..=close_paren]
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" "),
                    descriptor_creating_call.close_on_exec_spelling,
                ));
            }
            search_from = close_paren + 1;
        }
    }
    calls.sort_by_key(|(line, _, _)| *line);
    calls
}

/// Blank whole-line comments while keeping the line count, so a reported line
/// number still points at the source.
fn blank_out_comment_lines(body: &str) -> String {
    body.lines()
        .map(|line| {
            if line.trim_start().starts_with("//") {
                ""
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn matching_close_paren(code: &str, open_paren: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (offset, byte) in code.as_bytes().iter().enumerate().skip(open_paren) {
        match byte {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(offset);
                }
            }
            _ => {}
        }
    }
    None
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
        let refusal = ensure_every_scan_root_contributed(&report).unwrap_err();
        assert!(refusal.to_string().contains(SCAN_ROOTS[1]), "got {refusal}");
    }
}
