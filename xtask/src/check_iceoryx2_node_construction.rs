// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Bans building an iceoryx2 node, or reading iceoryx2's global configuration,
//! anywhere but the engine's one domain function in `iceoryx2/node.rs`.
//!
//! A node built any other way reads iceoryx2's ambient configuration and lands
//! in a different domain from every engine-owned node, where no data flows and
//! nothing errors — a partial migration hangs tests silently. So unlike every
//! other gate, test modules, test files and benches are scanned too.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// The repo-relative roots holding Rust that can reach iceoryx2.
const SCAN_ROOTS: &[&str] = &["runtime", "sdk", "adapters"];

/// The one file allowed to build a node: the engine's domain function.
const ALLOWED_FILE: &str = "runtime/streamlib-engine/src/iceoryx2/node.rs";

/// Calls that build a node or read iceoryx2's lookup-path configuration.
const BANNED_CALLS: &[&str] = &["NodeBuilder::new(", "Config::global_config("];

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
        "check-iceoryx2-node-construction",
        &format!("{SCAN_ROOTS:?}"),
        report.files_scanned,
        "an iceoryx2 node outside the engine-owned domain",
    )?;
    for violation in &report.violations {
        eprintln!(
            "{}:{}: an iceoryx2 node or configuration built outside {ALLOWED_FILE} lands \
             outside the engine-owned domain. Use `create_iceoryx2_node_in_engine_owned_domain` \
             or `Iceoryx2Node::new` (in a unit test, `Iceoryx2Node::for_this_test_process`).\n    {}",
            violation.path.display(),
            violation.line_no,
            violation.line_text.trim_end(),
        );
    }
    if report.violations.is_empty() {
        println!(
            "check-iceoryx2-node-construction: {} file(s) scanned across {SCAN_ROOTS:?}, no \
             iceoryx2 node built outside {ALLOWED_FILE}",
            report.files_scanned,
        );
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "check-iceoryx2-node-construction: {} iceoryx2 node construction(s) outside {ALLOWED_FILE}",
            report.violations.len(),
        ))
    }
}

pub fn scan(workspace_root: &Path) -> Result<CheckReport> {
    let mut report = CheckReport {
        violations: Vec::new(),
        files_scanned: 0,
    };
    for scan_root in SCAN_ROOTS {
        for relative_path in crate::list_repository_files_under(workspace_root, scan_root)? {
            if !relative_path.ends_with(".rs") || relative_path == ALLOWED_FILE {
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
    for (index, line) in content.lines().enumerate() {
        if line.trim_start().starts_with("//") {
            continue;
        }
        if BANNED_CALLS.iter().any(|banned| line.contains(banned)) {
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

    fn violations_in(content: &str) -> Vec<Violation> {
        let mut violations = Vec::new();
        collect_violations(
            Path::new("runtime/streamlib-engine/src/iceoryx2/input.rs"),
            content,
            &mut violations,
        );
        violations
    }

    #[test]
    fn a_raw_node_builder_in_library_code_is_refused() {
        let violations = violations_in(
            "fn open() {\n    let node = NodeBuilder::new().create::<ipc::Service>();\n}\n",
        );

        assert_eq!(violations.len(), 1, "{violations:?}");
        assert_eq!(violations[0].line_no, 2);
    }

    #[test]
    fn a_raw_node_builder_inside_a_unit_test_module_is_refused_too() {
        let violations = violations_in(
            "fn real() {}\n#[cfg(test)]\nmod tests {\n    fn t() { let node = NodeBuilder::new().create::<ipc::Service>().unwrap(); }\n}\n",
        );

        assert_eq!(violations.len(), 1, "{violations:?}");
    }

    #[test]
    fn a_read_of_the_global_iceoryx2_configuration_is_refused() {
        let violations = violations_in("let config = Config::global_config();\n");

        assert_eq!(violations.len(), 1, "{violations:?}");
    }

    #[test]
    fn a_node_built_through_the_engine_domain_function_passes() {
        let violations = violations_in(
            "let node = Iceoryx2Node::new(&root, \"streamlib-runtime/R\")?;\nlet raw = create_iceoryx2_node_in_engine_owned_domain(&root, \"x\")?;\n",
        );

        assert!(violations.is_empty(), "{violations:?}");
    }

    #[test]
    fn a_commented_mention_is_not_a_call() {
        let violations = violations_in("// iceoryx2's NodeBuilder::new() reads the lookup path\n");

        assert!(violations.is_empty(), "{violations:?}");
    }

    #[test]
    fn the_engine_domain_function_file_is_the_one_allowed_site() {
        let tmp = tempfile::TempDir::new().unwrap();
        let allowed = tmp.path().join(ALLOWED_FILE);
        std::fs::create_dir_all(allowed.parent().unwrap()).unwrap();
        std::fs::write(&allowed, "NodeBuilder::new()\n").unwrap();
        let bench = tmp.path().join("runtime/streamlib-engine/benches/hop.rs");
        std::fs::create_dir_all(bench.parent().unwrap()).unwrap();
        std::fs::write(
            &bench,
            "let node = NodeBuilder::new().create::<ipc::Service>();\n",
        )
        .unwrap();
        std::process::Command::new("git")
            .arg("init")
            .arg("-q")
            .current_dir(tmp.path())
            .status()
            .unwrap();

        let report = scan(tmp.path()).unwrap();

        assert_eq!(report.files_scanned, 1, "the allowed file is not scanned");
        assert_eq!(report.violations.len(), 1, "{:?}", report.violations);
        assert_eq!(
            report.violations[0].path,
            PathBuf::from("runtime/streamlib-engine/benches/hop.rs")
        );
    }
}
