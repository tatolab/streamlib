// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Keeps every apt install in CI behind the bounded-retry action.
//!
//! A workflow that runs `apt-get` itself has no wall-clock bound, and the mode
//! that costs is not the one people write guards for. A measured run fetched
//! 35.6 MB at 48 kB/s over 12m17s with every request making forward progress:
//! `Acquire::Retries` never fired because nothing failed, and
//! `Acquire::http::Timeout` never fired because the connection was never idle.
//! Two workflows blew past 800s and 1400s on the same day against medians of
//! 17s and 13s.
//!
//! So the gate is a placement rule, not a vocabulary one: `apt-get` belongs in
//! `install-system-dependencies-with-bounded-retry.sh` and nowhere else under
//! `.github/workflows/`. Three workflows previously carried three copies of the
//! same unbounded pair of commands, which is exactly how one of them ends up
//! with a bound and the others do not.
//!
//! It also requires `timeout-minutes` on every step that calls the action.
//! Composite-action steps cannot declare one, so the caller's step is the only
//! place the native backstop can live, and a backstop nobody can forget is the
//! whole point of gating it.
//!
//! Discovery is `git ls-files`, matching the other gates: CI walks a clean
//! checkout, so "the files in the repo" is the intended semantics.

use anyhow::{Context, Result};
use std::path::Path;
use std::process::Command;

/// The action every workflow must go through to install apt packages.
const BOUNDED_APT_INSTALL_ACTION_REFERENCE: &str =
    "./.github/actions/install-linux-engine-build-dependencies";

/// The one file allowed to invoke `apt-get` under `.github/`.
const BOUNDED_RETRY_SCRIPT_RELATIVE_PATH: &str =
    ".github/actions/install-linux-engine-build-dependencies/install-system-dependencies-with-bounded-retry.sh";

/// A workflow line that installs apt packages outside the bounded-retry script.
#[derive(Debug, PartialEq, Eq)]
pub struct UnboundedAptInvocation {
    pub workflow_path: String,
    pub line_number: usize,
    pub line: String,
}

/// A step calling the action without the native step-level ceiling.
#[derive(Debug, PartialEq, Eq)]
pub struct ActionCallWithoutTimeout {
    pub workflow_path: String,
    pub line_number: usize,
}

#[derive(Debug, Default)]
pub struct Findings {
    pub unbounded_apt_invocations: Vec<UnboundedAptInvocation>,
    pub action_calls_without_timeout: Vec<ActionCallWithoutTimeout>,
    pub workflows_scanned: usize,
}

pub fn run(workspace_root: &Path) -> Result<()> {
    let findings = scan(workspace_root)?;

    crate::ensure_source_walking_gate_read_source(
        "check-bounded-apt-install",
        ".github/workflows/",
        findings.workflows_scanned,
        "an unbounded apt install to hang a job for 13 minutes",
    )?;

    let mut failure_lines: Vec<String> = Vec::new();

    for invocation in &findings.unbounded_apt_invocations {
        failure_lines.push(format!(
            "{}:{}: `apt-get` in a workflow — install through `{}` instead, which bounds \
             each command and escapes to a second mirror. The only file allowed to invoke \
             apt is `{}`. Offending line: {}",
            invocation.workflow_path,
            invocation.line_number,
            BOUNDED_APT_INSTALL_ACTION_REFERENCE,
            BOUNDED_RETRY_SCRIPT_RELATIVE_PATH,
            invocation.line.trim(),
        ));
    }

    for call in &findings.action_calls_without_timeout {
        failure_lines.push(format!(
            "{}:{}: step calls the bounded-apt-install action without `timeout-minutes` — \
             a composite action cannot declare one, so this step is the only place the \
             native ceiling can live",
            call.workflow_path, call.line_number,
        ));
    }

    anyhow::ensure!(
        failure_lines.is_empty(),
        "check-bounded-apt-install found {} violation(s) across {} workflow(s):\n{}",
        failure_lines.len(),
        findings.workflows_scanned,
        failure_lines.join("\n"),
    );

    tracing::info!(
        "check-bounded-apt-install: {} workflows scanned, every apt install bounded",
        findings.workflows_scanned
    );
    Ok(())
}

pub fn scan(workspace_root: &Path) -> Result<Findings> {
    let mut findings = Findings::default();

    for workflow_path in list_tracked_workflow_files(workspace_root)? {
        let absolute = workspace_root.join(&workflow_path);
        let contents = std::fs::read_to_string(&absolute)
            .with_context(|| format!("failed to read {}", absolute.display()))?;

        findings.workflows_scanned += 1;
        scan_one_workflow(&workflow_path, &contents, &mut findings);
    }

    Ok(findings)
}

fn list_tracked_workflow_files(workspace_root: &Path) -> Result<Vec<String>> {
    let output = Command::new("git")
        .args(["ls-files", "--cached", "--others", "--exclude-standard", "--"])
        .arg(".github/workflows")
        .current_dir(workspace_root)
        .output()
        .context("failed to run `git ls-files` for .github/workflows")?;

    anyhow::ensure!(
        output.status.success(),
        "`git ls-files .github/workflows` failed: {}",
        String::from_utf8_lossy(&output.stderr),
    );

    Ok(String::from_utf8(output.stdout)
        .context("`git ls-files` emitted non-UTF-8 paths")?
        .lines()
        .map(str::to_owned)
        .filter(|path| path.ends_with(".yml") || path.ends_with(".yaml"))
        .collect())
}

fn scan_one_workflow(workflow_path: &str, contents: &str, findings: &mut Findings) {
    for (index, line) in contents.lines().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('#') || !line.contains("apt-get") {
            continue;
        }
        findings
            .unbounded_apt_invocations
            .push(UnboundedAptInvocation {
                workflow_path: workflow_path.to_owned(),
                line_number: index + 1,
                line: line.to_owned(),
            });
    }

    for step in split_into_step_blocks(contents) {
        let calls_the_action = step
            .lines
            .iter()
            .any(|(_, line)| line.contains(BOUNDED_APT_INSTALL_ACTION_REFERENCE));
        if !calls_the_action {
            continue;
        }

        let declares_a_timeout = step
            .lines
            .iter()
            .any(|(_, line)| line.trim_start().starts_with("timeout-minutes:"));
        if !declares_a_timeout {
            findings
                .action_calls_without_timeout
                .push(ActionCallWithoutTimeout {
                    workflow_path: workflow_path.to_owned(),
                    line_number: step.first_line_number,
                });
        }
    }
}

struct StepBlock<'a> {
    first_line_number: usize,
    lines: Vec<(usize, &'a str)>,
}

/// Split a workflow into YAML list-item blocks.
///
/// A step block runs from its `- ` line to the next non-blank line indented no
/// further than that `- `, which keeps nested `with:` mappings inside the step
/// they belong to. Indentation-based rather than parsed: the gate needs to know
/// which lines share a step, not what the YAML means, and the repo has no YAML
/// dependency to add for it.
fn split_into_step_blocks(contents: &str) -> Vec<StepBlock<'_>> {
    let numbered: Vec<(usize, &str)> = contents.lines().enumerate().collect();
    let mut blocks: Vec<StepBlock<'_>> = Vec::new();

    for (index, line) in &numbered {
        let indent = line.len() - line.trim_start().len();
        if !line.trim_start().starts_with("- ") {
            continue;
        }

        let mut block_lines: Vec<(usize, &str)> = vec![(index + 1, *line)];
        for (following_index, following_line) in &numbered[index + 1..] {
            if following_line.trim().is_empty() {
                continue;
            }
            let following_indent = following_line.len() - following_line.trim_start().len();
            if following_indent <= indent {
                break;
            }
            block_lines.push((following_index + 1, *following_line));
        }

        blocks.push(StepBlock {
            first_line_number: index + 1,
            lines: block_lines,
        });
    }

    blocks
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;

    fn scan_workflow_text(contents: &str) -> Findings {
        let mut findings = Findings::default();
        findings.workflows_scanned = 1;
        scan_one_workflow("test.yml", contents, &mut findings);
        findings
    }

    #[test]
    fn rejects_a_bare_apt_get_in_a_workflow() {
        let findings = scan_workflow_text(
            "jobs:\n  t:\n    steps:\n      - name: Install system dependencies\n        \
             run: |\n          sudo apt-get update\n          sudo apt-get install -y glslc\n",
        );
        assert_eq!(
            findings.unbounded_apt_invocations.len(),
            2,
            "both apt-get lines must be flagged: {findings:?}"
        );
    }

    #[test]
    fn accepts_a_workflow_that_uses_the_bounded_action() {
        let findings = scan_workflow_text(
            "jobs:\n  t:\n    steps:\n      - name: Install system dependencies\n        \
             timeout-minutes: 8\n        uses: ./.github/actions/install-linux-engine-build-dependencies\n        \
             with:\n          packages: glslc libvulkan-dev\n",
        );
        assert!(
            findings.unbounded_apt_invocations.is_empty(),
            "no raw apt-get: {findings:?}"
        );
        assert!(
            findings.action_calls_without_timeout.is_empty(),
            "the step declares timeout-minutes: {findings:?}"
        );
    }

    #[test]
    fn rejects_an_action_call_missing_the_native_ceiling() {
        let findings = scan_workflow_text(
            "jobs:\n  t:\n    steps:\n      - name: Install system dependencies\n        \
             uses: ./.github/actions/install-linux-engine-build-dependencies\n        \
             with:\n          packages: glslc\n",
        );
        assert_eq!(
            findings.action_calls_without_timeout.len(),
            1,
            "a step without timeout-minutes must be flagged: {findings:?}"
        );
    }

    #[test]
    fn a_timeout_on_a_neighbouring_step_does_not_count() {
        // The bound has to be on the step that calls the action; a sibling's
        // `timeout-minutes` bounds the sibling.
        let findings = scan_workflow_text(
            "jobs:\n  t:\n    steps:\n      - name: Something else\n        \
             timeout-minutes: 8\n        run: echo hi\n      - name: Install system dependencies\n        \
             uses: ./.github/actions/install-linux-engine-build-dependencies\n        \
             with:\n          packages: glslc\n",
        );
        assert_eq!(
            findings.action_calls_without_timeout.len(),
            1,
            "the sibling's bound must not satisfy the action's step: {findings:?}"
        );
    }

    #[test]
    fn skips_a_commented_apt_get() {
        let findings = scan_workflow_text("      # was: sudo apt-get install -y glslc\n");
        assert!(
            findings.unbounded_apt_invocations.is_empty(),
            "a comment naming apt-get is not an invocation: {findings:?}"
        );
    }

    #[test]
    fn the_repo_itself_passes_the_gate() {
        run(&crate::workspace_root().unwrap()).expect("the repo's own workflows must be bounded");
    }

    // ---- the bounded-retry script's own behaviour ----------------------------
    //
    // The ticket asks for a synthetic check that the timeout path actually
    // fires, rather than a green run that happened to get a fast mirror. These
    // drive the real script with `STREAMLIB_APT_GET_COMMAND` pointed at a
    // fixture, so they need no root, no apt and no network.

    /// Drives the real script against a fake `apt-get` that logs every
    /// subcommand it is handed, so a test can assert what ran and in what order.
    struct ScriptHarness {
        _temp: TempDir,
        script: std::path::PathBuf,
        apt_get_fixture: std::path::PathBuf,
        mirror_switch_fixture: std::path::PathBuf,
        invocation_log: std::path::PathBuf,
    }

    impl ScriptHarness {
        fn new(apt_get_fixture_body: &str) -> Self {
            let temp = TempDir::new().unwrap();

            let invocation_log = temp.path().join("invocations.log");
            fs::write(&invocation_log, "").unwrap();

            let apt_get_fixture = write_executable_fixture(
                temp.path().join("apt-get-fixture"),
                apt_get_fixture_body,
            );
            let mirror_switch_fixture = write_executable_fixture(
                temp.path().join("mirror-switch-fixture"),
                "#!/usr/bin/env bash\nprintf 'switched %s\\n' \"$1\" >> \"$INVOCATION_LOG\"\n",
            );

            Self {
                script: crate::workspace_root()
                    .unwrap()
                    .join(BOUNDED_RETRY_SCRIPT_RELATIVE_PATH),
                _temp: temp,
                apt_get_fixture,
                mirror_switch_fixture,
                invocation_log,
            }
        }

        fn run_with_attempt_bound(&self, attempt_timeout_seconds: &str) -> (bool, String) {
            let result = Command::new(&self.script)
                .arg("glslc")
                .env("STREAMLIB_APT_GET_COMMAND", &self.apt_get_fixture)
                .env(
                    "STREAMLIB_APT_MIRROR_SWITCH_COMMAND",
                    &self.mirror_switch_fixture,
                )
                .env(
                    "STREAMLIB_APT_ATTEMPT_TIMEOUT_SECONDS",
                    attempt_timeout_seconds,
                )
                .env("INVOCATION_LOG", &self.invocation_log)
                .output()
                .expect("the bounded-retry script must be executable");

            (
                result.status.success(),
                fs::read_to_string(&self.invocation_log).unwrap(),
            )
        }
    }

    fn write_executable_fixture(path: std::path::PathBuf, body: &str) -> std::path::PathBuf {
        fs::write(&path, body).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[test]
    fn a_healthy_mirror_installs_without_switching() {
        let harness = ScriptHarness::new(
            "#!/usr/bin/env bash\nprintf '%s\\n' \"$1\" >> \"$INVOCATION_LOG\"\nexit 0\n",
        );
        let (succeeded, log) = harness.run_with_attempt_bound("5");

        assert!(succeeded, "a healthy mirror must succeed; log:\n{log}");
        assert!(
            log.contains("update") && log.contains("install"),
            "both apt commands must run; log:\n{log}"
        );
        assert!(
            !log.contains("switched"),
            "a healthy run must not touch the mirror; log:\n{log}"
        );
    }

    #[test]
    fn a_slow_mirror_trips_the_bound_and_escapes_to_the_fallback() {
        // The measured failure: apt makes forward progress the whole time and
        // never errors, so only the wall-clock bound notices.
        let harness = ScriptHarness::new(
            "#!/usr/bin/env bash\nprintf '%s\\n' \"$1\" >> \"$INVOCATION_LOG\"\n\
             if grep -q switched \"$INVOCATION_LOG\"; then exit 0; fi\nsleep 600\n",
        );
        let (succeeded, log) = harness.run_with_attempt_bound("2");

        assert!(
            log.contains("switched"),
            "the bound must fire and repoint the mirror; log:\n{log}"
        );
        assert!(
            succeeded,
            "the fallback mirror must carry the install home; log:\n{log}"
        );
    }

    #[test]
    fn both_mirrors_failing_exits_non_zero_rather_than_hanging() {
        let harness = ScriptHarness::new(
            "#!/usr/bin/env bash\nprintf '%s\\n' \"$1\" >> \"$INVOCATION_LOG\"\nexit 100\n",
        );
        let (succeeded, log) = harness.run_with_attempt_bound("5");

        assert!(!succeeded, "exhausting both mirrors must fail; log:\n{log}");
        assert_eq!(
            log.lines().filter(|line| *line == "update").count(),
            2,
            "one attempt per mirror, no more; log:\n{log}"
        );
    }
}
