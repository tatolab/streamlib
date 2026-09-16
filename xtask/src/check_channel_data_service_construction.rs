// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Bans building an iceoryx2 publish-subscribe service anywhere but
//! `iceoryx2/node.rs`, whose `Iceoryx2Node::open_or_create_service` is the one
//! place a channel data service is built.
//!
//! Every data service carries the sequence-number user header, and iceoryx2
//! refuses an opener presenting any other, so a service built by hand either
//! fails to open against the engine's channels or carries bags no destination
//! can count the loss of. Test modules, test files and benches are scanned too:
//! a test publishing on a service without the header proves nothing about the
//! wire the engine runs.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// The repo-relative roots holding Rust that can reach iceoryx2.
const SCAN_ROOTS: &[&str] = &["runtime", "sdk", "adapters"];

/// The one file allowed to build a publish-subscribe service.
const ALLOWED_FILE: &str = "runtime/streamlib-engine/src/iceoryx2/node.rs";

/// The builder call that opens a publish-subscribe service.
const PUBLISH_SUBSCRIBE_BUILDER_CALL: &str = "publish_subscribe::<";

#[derive(Debug)]
pub struct Violation {
    pub path: PathBuf,
    pub line_no: usize,
    pub line_text: String,
}

pub struct CheckReport {
    pub violations: Vec<Violation>,
    pub files_scanned: usize,
}

pub fn run(workspace_root: &Path) -> Result<()> {
    let report = scan(workspace_root)?;
    crate::ensure_source_walking_gate_read_source(
        "check-channel-data-service-construction",
        &format!("{SCAN_ROOTS:?}"),
        report.files_scanned,
        "a publish-subscribe service built outside iceoryx2/node.rs",
    )?;

    let failure_lines: Vec<String> = report
        .violations
        .iter()
        .map(|violation| {
            format!(
                "{}:{}: a publish-subscribe service built outside {ALLOWED_FILE} lacks the \
                 sequence-number user header every channel data service carries. Open the \
                 channel through `Iceoryx2Node::open_or_create_service` and take its ports \
                 from the returned service.\n    {}",
                violation.path.display(),
                violation.line_no,
                violation.line_text.trim(),
            )
        })
        .collect();
    anyhow::ensure!(
        failure_lines.is_empty(),
        "check-channel-data-service-construction found {} violation(s) across {} file(s):\n{}",
        failure_lines.len(),
        report.files_scanned,
        failure_lines.join("\n"),
    );

    tracing::info!(
        "check-channel-data-service-construction: {} files scanned across {SCAN_ROOTS:?}, no \
         publish-subscribe service built outside {ALLOWED_FILE}",
        report.files_scanned,
    );
    Ok(())
}

pub fn scan(workspace_root: &Path) -> Result<CheckReport> {
    let mut report = CheckReport {
        violations: Vec::new(),
        files_scanned: 0,
    };
    for scan_root in SCAN_ROOTS {
        for relative_path in crate::list_repository_files_under(workspace_root, scan_root)? {
            if !relative_path.ends_with(".rs") {
                continue;
            }
            let path = workspace_root.join(&relative_path);
            // `git ls-files --cached` lists a file deleted from the worktree but not
            // yet from the index.
            if !path.is_file() {
                continue;
            }
            let content = std::fs::read_to_string(&path)
                .with_context(|| format!("failed to read {}", path.display()))?;
            report.files_scanned += 1;
            collect_violations(Path::new(&relative_path), &content, &mut report.violations);
        }
    }
    Ok(report)
}

fn collect_violations(relative_path: &Path, content: &str, violations: &mut Vec<Violation>) {
    if relative_path == Path::new(ALLOWED_FILE) {
        return;
    }
    for (index, line) in content.lines().enumerate() {
        if crate::source_call_site_scan::is_a_whole_line_comment(line) {
            continue;
        }
        if line.contains(PUBLISH_SUBSCRIBE_BUILDER_CALL) {
            violations.push(Violation {
                path: relative_path.to_path_buf(),
                line_no: index + 1,
                line_text: line.to_string(),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn violations_in(relative_path: &str, content: &str) -> Vec<Violation> {
        let mut violations = Vec::new();
        collect_violations(Path::new(relative_path), content, &mut violations);
        violations
    }

    #[test]
    fn a_raw_data_service_builder_in_a_unit_test_module_is_refused() {
        let violations = violations_in(
            "runtime/streamlib-engine/src/iceoryx2/input.rs",
            "fn real() {}\n#[cfg(test)]\nmod tests {\n    fn open() {\n        node.service_builder(&name)\n            .publish_subscribe::<[u8]>()\n            .open_or_create();\n    }\n}\n",
        );

        assert_eq!(violations.len(), 1, "{violations:?}");
        assert_eq!(violations[0].line_no, 6);
    }

    #[test]
    fn a_service_opened_through_the_engine_wrapper_passes() {
        let violations = violations_in(
            "sdk/streamlib-python-wheel/src/python_processor_link_data_access.rs",
            "let channel = node.open_or_create_service(&name, max_subscribers, depth)?;\nlet publisher = channel.create_publisher(expected_payload_bytes)?;\n",
        );

        assert!(violations.is_empty(), "{violations:?}");
    }

    #[test]
    fn a_commented_mention_is_not_a_builder() {
        let violations = violations_in(
            "runtime/streamlib-engine/src/iceoryx2/output.rs",
            "// a raw `.publish_subscribe::<[u8]>()` carries no user header\n",
        );

        assert!(violations.is_empty(), "{violations:?}");
    }

    #[test]
    fn the_engine_node_file_is_the_one_allowed_site_and_benches_are_scanned() {
        let tmp = tempfile::TempDir::new().unwrap();
        let builder =
            "let service = node.service_builder(&name).publish_subscribe::<[u8]>().create();\n";
        let allowed = tmp.path().join(ALLOWED_FILE);
        std::fs::create_dir_all(allowed.parent().unwrap()).unwrap();
        std::fs::write(&allowed, builder).unwrap();
        let bench = tmp.path().join("runtime/streamlib-engine/benches/hop.rs");
        std::fs::create_dir_all(bench.parent().unwrap()).unwrap();
        std::fs::write(&bench, builder).unwrap();
        std::process::Command::new("git")
            .arg("init")
            .arg("-q")
            .current_dir(tmp.path())
            .status()
            .unwrap();

        let report = scan(tmp.path()).unwrap();

        assert_eq!(report.files_scanned, 2);
        assert_eq!(report.violations.len(), 1, "{:?}", report.violations);
        assert_eq!(
            report.violations[0].path,
            PathBuf::from("runtime/streamlib-engine/benches/hop.rs")
        );
    }
}
