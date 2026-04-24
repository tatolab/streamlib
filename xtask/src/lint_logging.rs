// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Ripgrep-style string lint that bans ad-hoc logging patterns in the Python
//! and TypeScript polyglot SDKs. The only sanctioned pathway is `streamlib.log.*`
//! — see `docs/logging.md`. Rust violations are caught by clippy's
//! `disallowed-macros` rule (see `clippy.toml`).

use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

pub struct LintTarget {
    pub name: &'static str,
    pub root_relative: &'static str,
    pub extension: &'static str,
    pub exclude_path_segments: &'static [&'static str],
    pub exclude_file_suffixes: &'static [&'static str],
    pub comment_prefix: &'static str,
    pub banned_substrings: &'static [&'static str],
    pub allow_substring: &'static str,
}

pub const TARGETS: &[LintTarget] = &[
    LintTarget {
        name: "python",
        root_relative: "libs/streamlib-python",
        extension: "py",
        exclude_path_segments: &["tests", "_generated_", "__pycache__", ".venv", "build", "dist"],
        exclude_file_suffixes: &["_test.py"],
        comment_prefix: "#",
        banned_substrings: &["print(", "sys.stdout", "sys.stderr", "logging.basicConfig"],
        allow_substring: "streamlib.log.",
    },
    LintTarget {
        name: "typescript",
        root_relative: "libs/streamlib-deno",
        extension: "ts",
        exclude_path_segments: &["_generated_", "tests", "node_modules"],
        exclude_file_suffixes: &["_test.ts", ".test.ts"],
        comment_prefix: "//",
        banned_substrings: &[
            "console.log",
            "console.warn",
            "console.error",
            "console.info",
            "console.debug",
            "Deno.stdout.write",
            "Deno.stderr.write",
        ],
        allow_substring: "streamlib.log.",
    },
];

#[derive(Debug)]
pub struct Violation {
    pub path: PathBuf,
    pub line_no: usize,
    pub line_text: String,
    pub matched_pattern: &'static str,
    pub target: &'static str,
}

pub struct LintReport {
    pub violations: Vec<Violation>,
    pub files_scanned: usize,
}

pub fn run(project_root: &Path) -> Result<()> {
    let report = scan_all(project_root)?;
    for v in &report.violations {
        eprintln!(
            "{}:{}: [{}] banned `{}` — use streamlib.log.* / see docs/logging.md\n    {}",
            v.path.display(),
            v.line_no,
            v.target,
            v.matched_pattern,
            v.line_text.trim_end(),
        );
    }
    if report.violations.is_empty() {
        println!(
            "lint-logging: {} file(s) scanned across polyglot SDKs, no violations",
            report.files_scanned,
        );
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "lint-logging: {} violation(s) across {} file(s) scanned",
            report.violations.len(),
            report.files_scanned,
        ))
    }
}

pub fn scan_all(project_root: &Path) -> Result<LintReport> {
    let mut violations = Vec::new();
    let mut files_scanned = 0usize;
    for target in TARGETS {
        let root = project_root.join(target.root_relative);
        if !root.exists() {
            continue;
        }
        scan_target(&root, target, &mut violations, &mut files_scanned)?;
    }
    Ok(LintReport { violations, files_scanned })
}

fn scan_target(
    root: &Path,
    target: &'static LintTarget,
    violations: &mut Vec<Violation>,
    files_scanned: &mut usize,
) -> Result<()> {
    for entry in WalkDir::new(root).into_iter().filter_map(|e| e.ok()) {
        let path = entry.path();
        if !entry.file_type().is_file() {
            continue;
        }
        if path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e != target.extension)
            .unwrap_or(true)
        {
            continue;
        }
        if is_excluded(path, root, target) {
            continue;
        }
        *files_scanned += 1;
        scan_file(path, target, violations)?;
    }
    Ok(())
}

fn is_excluded(path: &Path, root: &Path, target: &LintTarget) -> bool {
    let rel = path.strip_prefix(root).unwrap_or(path);
    for component in rel.components() {
        if let std::path::Component::Normal(seg) = component {
            if let Some(seg_str) = seg.to_str() {
                if target.exclude_path_segments.contains(&seg_str) {
                    return true;
                }
            }
        }
    }
    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
        for suffix in target.exclude_file_suffixes {
            if name.ends_with(suffix) {
                return true;
            }
        }
    }
    false
}

/// Marker comment that exempts an entire file. Used by the infrastructure
/// files that *install* the unified pathway — `_log_interceptors.py`,
/// `_log_interceptors.ts`, and the subprocess bootstrap runners that must
/// emit diagnostics before the logging pipeline is wired.
const ALLOW_FILE_PRAGMA: &str = "streamlib:lint-logging:allow-file";

/// Marker comment that exempts a single line.
const ALLOW_LINE_PRAGMA: &str = "streamlib:lint-logging:allow-line";

fn scan_file(
    path: &Path,
    target: &'static LintTarget,
    violations: &mut Vec<Violation>,
) -> Result<()> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    if content.contains(ALLOW_FILE_PRAGMA) {
        return Ok(());
    }
    for (idx, line) in content.lines().enumerate() {
        if is_comment_line(line, target.comment_prefix) {
            continue;
        }
        if line.contains(target.allow_substring) {
            continue;
        }
        if line.contains(ALLOW_LINE_PRAGMA) {
            continue;
        }
        for pattern in target.banned_substrings {
            if line.contains(pattern) {
                violations.push(Violation {
                    path: path.to_path_buf(),
                    line_no: idx + 1,
                    line_text: line.to_string(),
                    matched_pattern: pattern,
                    target: target.name,
                });
                break;
            }
        }
    }
    Ok(())
}

fn is_comment_line(line: &str, prefix: &str) -> bool {
    line.trim_start().starts_with(prefix)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write_fixture(dir: &Path, rel: &str, content: &str) -> PathBuf {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, content).unwrap();
        path
    }

    fn scan_fixture_tree(target: &'static LintTarget, content: &str, rel_path: &str) -> Vec<Violation> {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().to_path_buf();
        write_fixture(&root, rel_path, content);
        let mut violations = Vec::new();
        let mut files_scanned = 0usize;
        scan_target(&root, target, &mut violations, &mut files_scanned).unwrap();
        violations
    }

    const PYTHON_TARGET: &LintTarget = &TARGETS[0];
    const TYPESCRIPT_TARGET: &LintTarget = &TARGETS[1];

    #[test]
    fn rejects_print_in_python_library() {
        let v = scan_fixture_tree(PYTHON_TARGET, "print(\"hello\")\n", "src/app.py");
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].matched_pattern, "print(");
    }

    #[test]
    fn rejects_sys_stdout_in_python() {
        let v = scan_fixture_tree(
            PYTHON_TARGET,
            "import sys\nsys.stdout.write(\"hi\")\n",
            "src/app.py",
        );
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].matched_pattern, "sys.stdout");
    }

    #[test]
    fn rejects_logging_basicconfig_in_python() {
        let v = scan_fixture_tree(
            PYTHON_TARGET,
            "import logging\nlogging.basicConfig(level=logging.INFO)\n",
            "src/app.py",
        );
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].matched_pattern, "logging.basicConfig");
    }

    #[test]
    fn rejects_console_log_in_ts_library() {
        let v = scan_fixture_tree(TYPESCRIPT_TARGET, "console.log(\"hello\");\n", "src/app.ts");
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].matched_pattern, "console.log");
    }

    #[test]
    fn rejects_deno_stdout_write_in_ts() {
        let v = scan_fixture_tree(
            TYPESCRIPT_TARGET,
            "await Deno.stdout.write(new TextEncoder().encode(\"x\"));\n",
            "src/app.ts",
        );
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].matched_pattern, "Deno.stdout.write");
    }

    #[test]
    fn rejects_every_console_method_variant() {
        for method in ["log", "warn", "error", "info", "debug"] {
            let src = format!("console.{}(\"x\");\n", method);
            let v = scan_fixture_tree(TYPESCRIPT_TARGET, &src, "src/app.ts");
            assert_eq!(v.len(), 1, "console.{} should be rejected", method);
        }
    }

    #[test]
    fn accepts_streamlib_log_python() {
        let v = scan_fixture_tree(
            PYTHON_TARGET,
            "import streamlib\nstreamlib.log.info(\"hi\")\n",
            "src/app.py",
        );
        assert!(v.is_empty(), "streamlib.log.* should pass: {:?}", v);
    }

    #[test]
    fn accepts_streamlib_log_ts() {
        let v = scan_fixture_tree(
            TYPESCRIPT_TARGET,
            "import * as streamlib from \"./mod.ts\";\nstreamlib.log.info(\"hi\");\n",
            "src/app.ts",
        );
        assert!(v.is_empty(), "streamlib.log.* should pass: {:?}", v);
    }

    #[test]
    fn skips_comment_lines_python() {
        let v = scan_fixture_tree(
            PYTHON_TARGET,
            "# don't use print(\"x\") here\nstreamlib.log.info(\"ok\")\n",
            "src/app.py",
        );
        assert!(v.is_empty(), "commented-out print should not flag: {:?}", v);
    }

    #[test]
    fn skips_comment_lines_ts() {
        let v = scan_fixture_tree(
            TYPESCRIPT_TARGET,
            "// don't use console.log here\nstreamlib.log.info(\"ok\");\n",
            "src/app.ts",
        );
        assert!(v.is_empty(), "commented-out console.log should not flag: {:?}", v);
    }

    #[test]
    fn excludes_tests_directory_python() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().to_path_buf();
        write_fixture(&root, "tests/test_app.py", "print(\"hi\")\n");
        write_fixture(&root, "src/app.py", "streamlib.log.info(\"ok\")\n");
        let mut violations = Vec::new();
        let mut files_scanned = 0usize;
        scan_target(&root, PYTHON_TARGET, &mut violations, &mut files_scanned).unwrap();
        assert!(violations.is_empty(), "tests/ should be excluded: {:?}", violations);
    }

    #[test]
    fn allow_file_pragma_skips_entire_file_python() {
        let v = scan_fixture_tree(
            PYTHON_TARGET,
            "# streamlib:lint-logging:allow-file — this is the interceptor installer itself\nimport sys\nsys.stdout = MyInterceptor()\n",
            "src/_log_interceptors.py",
        );
        assert!(v.is_empty(), "allow-file pragma should suppress entire file: {:?}", v);
    }

    #[test]
    fn allow_file_pragma_skips_entire_file_ts() {
        let v = scan_fixture_tree(
            TYPESCRIPT_TARGET,
            "// streamlib:lint-logging:allow-file — interceptor installer\nDeno.stdout.write(new Uint8Array());\n",
            "src/_log_interceptors.ts",
        );
        assert!(v.is_empty(), "allow-file pragma should suppress entire file: {:?}", v);
    }

    #[test]
    fn allow_line_pragma_skips_single_line_python() {
        let v = scan_fixture_tree(
            PYTHON_TARGET,
            "sys.stderr.write(\"fallback\")  # streamlib:lint-logging:allow-line\nprint(\"bad\")\n",
            "src/app.py",
        );
        assert_eq!(v.len(), 1, "only the non-allowed line should flag: {:?}", v);
        assert_eq!(v[0].matched_pattern, "print(");
    }

    #[test]
    fn excludes_test_suffix_ts() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().to_path_buf();
        write_fixture(&root, "src/app_test.ts", "console.log(\"hi\");\n");
        write_fixture(&root, "src/app.ts", "streamlib.log.info(\"ok\");\n");
        let mut violations = Vec::new();
        let mut files_scanned = 0usize;
        scan_target(&root, TYPESCRIPT_TARGET, &mut violations, &mut files_scanned).unwrap();
        assert!(violations.is_empty(), "*_test.ts should be excluded: {:?}", violations);
    }
}
