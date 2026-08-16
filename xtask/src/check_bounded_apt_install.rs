// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Keeps every apt install in CI behind the bounded-retry action.
//!
//! A workflow that installs packages itself has no wall-clock bound, and the
//! mode that costs is not the one people write guards for — the script's own
//! header records the measurement and why apt's guards miss it.
//!
//! So this is a location rule: an *apt* install belongs in
//! `install-system-dependencies-with-bounded-retry.sh` and nowhere else under
//! `.github/`. Three workflows previously carried three copies of the same
//! unbounded pair of commands, which is exactly how one of them ends up with a
//! bound and the others do not — and composite actions are scanned too, because
//! this repo's first `.github/actions/` directory arrived with that script.
//!
//! It also requires `timeout-minutes` on every *workflow* step that calls the
//! action. Composite-action steps cannot declare one, so the caller's step is
//! the only place the native backstop can live, and a backstop nobody can
//! forget is the whole point of gating it.
//!
//! Three stated blind spots. `timeout-minutes:` counts only at a step's own key
//! indentation, so a `with:` input of that name does not satisfy the gate. A
//! step spelled across a YAML anchor or an `!!merge` key is not recognised as a
//! step at all. And the front-end list is the apt family only, so
//! `release-wheel.yml`'s `dnf install` inside its manylinux container passes —
//! that job is release-time, runs in a container this action cannot serve, and
//! is deliberately out of scope; it is a real unbounded fetch all the same.
//!
//! `.github/ISSUE_TEMPLATE/` is exempt: it is prose by construction, cannot run
//! anything, and is the one place under `.github/` where "run apt install …" is
//! a sentence rather than a step.
//!
//! [`ensure_the_gate_is_still_wired`] is what stops the whole gate from passing
//! vacuously if the shapes it keys on move.

use anyhow::Result;
use std::path::Path;

/// The action every workflow must go through to install apt packages.
const BOUNDED_APT_INSTALL_ACTION_REFERENCE: &str =
    "./.github/actions/install-linux-engine-build-dependencies";

/// Front-ends that fetch packages, in the spellings a CI step actually uses.
///
/// `apt-get` alone would be one hyphen from useless: `apt install -y` is the
/// spelling most people reach for first and is just as unbounded.
const PACKAGE_FETCH_FRONT_ENDS: &[&str] = &["apt-get", "apt", "aptitude"];

/// Subcommands that make one of those front-ends reach the network.
const PACKAGE_FETCH_SUBCOMMANDS: &[&str] = &[
    "install",
    "reinstall",
    "update",
    "upgrade",
    "dist-upgrade",
    "full-upgrade",
    "build-dep",
];

/// `dpkg` is mostly read-only (`-l`, `-L`, `-S`), so only the installing flags
/// count — otherwise the gate would fire on a perfectly good package query.
const DPKG_INSTALL_FLAGS: &[&str] = &["-i", "--install", "--unpack"];

/// The one file under `.github/` allowed to fetch packages.
const BOUNDED_RETRY_SCRIPT_RELATIVE_PATH: &str = ".github/actions/install-linux-engine-build-dependencies/install-system-dependencies-with-bounded-retry.sh";

/// Everything CI runs lives here — workflows and composite actions alike.
const GITHUB_CI_SCAN_ROOT: &str = ".github";

/// Only a workflow step can carry `timeout-minutes`; a composite action's cannot.
const WORKFLOW_FILE_PREFIX: &str = ".github/workflows/";

/// Prose by construction — the one place under `.github/` where an apt command
/// is quoted rather than run. Everything else is scanned, extension or not: a
/// Docker container action's `Dockerfile` is a first-class Actions shape and a
/// natural home for an unbounded `RUN apt-get install`.
const PROSE_ONLY_EXEMPT_PREFIX: &str = ".github/ISSUE_TEMPLATE/";

/// A line that installs apt packages outside the bounded-retry script.
#[derive(Debug, PartialEq, Eq)]
pub struct UnboundedAptInvocation {
    pub file_path: String,
    pub line_number: usize,
    pub line: String,
}

/// A workflow step calling the action without the native step-level ceiling.
#[derive(Debug, PartialEq, Eq)]
pub struct ActionCallWithoutTimeout {
    pub workflow_path: String,
    pub line_number: usize,
}

#[derive(Debug, Default)]
pub struct BoundedAptInstallScanReport {
    pub unbounded_apt_invocations: Vec<UnboundedAptInvocation>,
    pub action_calls_without_timeout: Vec<ActionCallWithoutTimeout>,
    pub workflow_steps_calling_the_action: usize,
    pub files_scanned: usize,
}

pub fn run(workspace_root: &Path) -> Result<()> {
    let report = scan(workspace_root)?;

    crate::ensure_source_walking_gate_read_source(
        "check-bounded-apt-install",
        GITHUB_CI_SCAN_ROOT,
        report.files_scanned,
        "an unbounded apt install to hang a job for 13 minutes",
    )?;
    ensure_the_gate_is_still_wired(workspace_root, &report)?;

    let mut failure_lines: Vec<String> = Vec::new();

    for invocation in &report.unbounded_apt_invocations {
        failure_lines.push(format!(
            "{}:{}: fetches apt packages under {} — install through `{}` instead, which \
             bounds each command and escapes to a second mirror. The only file allowed to \
             run apt is `{}`. Offending line: {}",
            invocation.file_path,
            invocation.line_number,
            GITHUB_CI_SCAN_ROOT,
            BOUNDED_APT_INSTALL_ACTION_REFERENCE,
            BOUNDED_RETRY_SCRIPT_RELATIVE_PATH,
            invocation.line.trim(),
        ));
    }

    for call in &report.action_calls_without_timeout {
        failure_lines.push(format!(
            "{}:{}: step calls the bounded-apt-install action without `timeout-minutes` — \
             a composite action cannot declare one, so this step is the only place the \
             native ceiling can live",
            call.workflow_path, call.line_number,
        ));
    }

    anyhow::ensure!(
        failure_lines.is_empty(),
        "check-bounded-apt-install found {} violation(s) across {} file(s):\n{}",
        failure_lines.len(),
        report.files_scanned,
        failure_lines.join("\n"),
    );

    tracing::info!(
        "check-bounded-apt-install: {} files scanned, {} bounded action call(s), no unbounded fetch",
        report.files_scanned,
        report.workflow_steps_calling_the_action,
    );
    Ok(())
}

/// Fail if the shapes this gate keys on have moved out from under it.
///
/// Every check here is a substring match against one of two constants. Rename
/// the action directory and `calls_the_action` matches nothing, every violation
/// list comes back empty, and the gate reports green while each step quietly
/// loses its ceiling — a gate that cannot fail is worse than no gate, because
/// the workflow list reads as covered.
fn ensure_the_gate_is_still_wired(
    workspace_root: &Path,
    report: &BoundedAptInstallScanReport,
) -> Result<()> {
    anyhow::ensure!(
        workspace_root
            .join(BOUNDED_RETRY_SCRIPT_RELATIVE_PATH)
            .is_file(),
        "check-bounded-apt-install expects the bounded-retry script at `{}` and it is not \
         there — the gate's package-fetch ban would then point callers at a file that does \
         not exist. Update BOUNDED_RETRY_SCRIPT_RELATIVE_PATH if the script moved.",
        BOUNDED_RETRY_SCRIPT_RELATIVE_PATH,
    );

    anyhow::ensure!(
        report.workflow_steps_calling_the_action > 0,
        "check-bounded-apt-install matched `{}` in no workflow step — either every apt \
         install was removed from CI, or the action was renamed and this gate is now \
         checking nothing. Update BOUNDED_APT_INSTALL_ACTION_REFERENCE.",
        BOUNDED_APT_INSTALL_ACTION_REFERENCE,
    );

    Ok(())
}

pub fn scan(workspace_root: &Path) -> Result<BoundedAptInstallScanReport> {
    let mut report = BoundedAptInstallScanReport::default();

    for file_path in crate::list_repository_files_under(workspace_root, GITHUB_CI_SCAN_ROOT)? {
        if file_path == BOUNDED_RETRY_SCRIPT_RELATIVE_PATH
            || file_path.starts_with(PROSE_ONLY_EXEMPT_PREFIX)
        {
            continue;
        }

        let absolute = workspace_root.join(&file_path);
        let Ok(contents) = std::fs::read_to_string(&absolute) else {
            // A non-UTF-8 file under .github/ holds no shell for apt to run.
            continue;
        };

        report.files_scanned += 1;
        collect_unbounded_apt_invocations(&file_path, &contents, &mut report);

        if file_path.starts_with(WORKFLOW_FILE_PREFIX) {
            collect_action_calls(&file_path, &contents, &mut report);
        }
    }

    Ok(report)
}

fn collect_unbounded_apt_invocations(
    file_path: &str,
    contents: &str,
    report: &mut BoundedAptInstallScanReport,
) {
    for (index, line) in contents.lines().enumerate() {
        if line.trim_start().starts_with('#') || !line_fetches_packages(line) {
            continue;
        }
        report
            .unbounded_apt_invocations
            .push(UnboundedAptInvocation {
                file_path: file_path.to_owned(),
                line_number: index + 1,
                line: line.to_owned(),
            });
    }
}

/// Does this shell line reach the network for packages?
///
/// Whitespace tokenisation, then exact token equality on the command — which is
/// what keeps a path like `/var/cache/apt/archives` from reading as an `apt`
/// invocation, and lets `sudo`, `DEBIAN_FRONTEND=noninteractive` and any other
/// prefix fall out for free.
///
/// A trailing `\` continuation counts, because the subcommand is on the next
/// line. A front-end as the *last* token of a line does not: that is far more
/// often a step named "Install system dependencies with apt" than a command.
///
/// Blind spot, stated rather than papered over: a front-end whose subcommand is
/// built from a shell variable (`apt-get "$verb"`) reads as neither.
fn line_fetches_packages(line: &str) -> bool {
    let mut tokens = line.split_whitespace().peekable();

    while let Some(token) = tokens.next() {
        let following = tokens.peek().copied();
        let fetches = match token {
            front_end if PACKAGE_FETCH_FRONT_ENDS.contains(&front_end) => match following {
                Some("\\") => true,
                Some(subcommand) => PACKAGE_FETCH_SUBCOMMANDS.contains(&subcommand),
                None => false,
            },
            "dpkg" => following.is_some_and(|flag| DPKG_INSTALL_FLAGS.contains(&flag)),
            _ => false,
        };
        if fetches {
            return true;
        }
    }

    false
}

fn collect_action_calls(
    workflow_path: &str,
    contents: &str,
    report: &mut BoundedAptInstallScanReport,
) {
    for step in split_into_step_blocks(contents) {
        // A commented-out `uses:` is not a call. Counting one would also inflate
        // `workflow_steps_calling_the_action`, which is what
        // `ensure_the_gate_is_still_wired` reads to decide the gate is live —
        // so a repo whose every real call had been commented out would still
        // look wired.
        if !step
            .uncommented_lines()
            .any(|line| line.contains(BOUNDED_APT_INSTALL_ACTION_REFERENCE))
        {
            continue;
        }

        report.workflow_steps_calling_the_action += 1;

        // Only at the step's own key indentation: a `with:` input that happens
        // to be named `timeout-minutes` is an input GitHub ignores, not a ceiling.
        let declares_a_timeout = step.uncommented_lines().any(|line| {
            line.trim_start().starts_with("timeout-minutes:")
                && indentation_width(line) == step.key_indent
        });

        if !declares_a_timeout {
            report
                .action_calls_without_timeout
                .push(ActionCallWithoutTimeout {
                    workflow_path: workflow_path.to_owned(),
                    line_number: step.first_line_number,
                });
        }
    }
}

struct WorkflowStepBlock<'a> {
    first_line_number: usize,
    /// Indentation of the step's own keys — `name:`, `uses:`, `timeout-minutes:`
    /// — as measured, not as assumed from the `-`. `-   name:` and a bare `-`
    /// put them somewhere other than marker + 2.
    key_indent: usize,
    lines: Vec<&'a str>,
}

impl<'a> WorkflowStepBlock<'a> {
    fn uncommented_lines(&self) -> impl Iterator<Item = &'a str> {
        self.lines
            .iter()
            .copied()
            .filter(|line| !line.trim_start().starts_with('#'))
    }
}

/// YAML forbids tab indentation, so a byte count is a column count.
fn indentation_width(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

/// Split a workflow into top-level YAML list-item blocks.
///
/// A block runs from its `-` line to the next non-blank line indented no
/// further, which keeps a nested `with:` mapping inside the step it belongs to.
/// Blocks never overlap: a `-` already swallowed by an open block is one of that
/// step's own list items, not a sibling step, and spawning a block for it would
/// produce a step that excludes its own `timeout-minutes`.
///
/// Indentation-based rather than parsed. `serde_yaml` is already in the
/// workspace, so the cost is not the dependency — it is that its `Value` carries
/// no source spans, and every failure this gate emits names a `file:line` the
/// author can jump to.
fn split_into_step_blocks(contents: &str) -> Vec<WorkflowStepBlock<'_>> {
    let lines: Vec<&str> = contents.lines().collect();
    let mut blocks: Vec<WorkflowStepBlock<'_>> = Vec::new();
    let mut next_unclaimed_line = 0usize;

    for (index, line) in lines.iter().enumerate() {
        if index < next_unclaimed_line {
            continue;
        }

        let trimmed = line.trim_start();
        // `- key: value` and a bare `-` with the keys on following lines are
        // both legal spellings of one sequence item.
        if trimmed != "-" && !trimmed.starts_with("- ") {
            continue;
        }

        let list_marker_indent = indentation_width(line);
        let mut block_lines: Vec<&str> = vec![*line];
        let mut end = index + 1;

        for (following_index, following_line) in lines.iter().enumerate().skip(index + 1) {
            if following_line.trim().is_empty() {
                continue;
            }
            if indentation_width(following_line) <= list_marker_indent {
                break;
            }
            block_lines.push(following_line);
            end = following_index + 1;
        }

        // `- key:` puts the first key on the marker line, after however much
        // space follows the dash; a bare `-` puts it on the next line.
        let after_the_dash = &trimmed[1..];
        let key_indent = if after_the_dash.trim().is_empty() {
            block_lines
                .get(1)
                .map_or(list_marker_indent + 2, |line| indentation_width(line))
        } else {
            list_marker_indent + 1 + indentation_width(after_the_dash)
        };

        next_unclaimed_line = end;
        blocks.push(WorkflowStepBlock {
            first_line_number: index + 1,
            key_indent,
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
    use std::path::PathBuf;
    use std::process::Command;
    use tempfile::TempDir;

    /// The family idiom for the workspace root in a gate's tests: free, and it
    /// needs neither cargo on PATH nor the package lock.
    fn workspace_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("xtask/ always has a workspace root above it")
            .to_path_buf()
    }

    fn scan_workflow_text(contents: &str) -> BoundedAptInstallScanReport {
        let mut report = BoundedAptInstallScanReport {
            files_scanned: 1,
            ..Default::default()
        };
        collect_unbounded_apt_invocations("test.yml", contents, &mut report);
        collect_action_calls("test.yml", contents, &mut report);
        report
    }

    const A_BOUNDED_STEP: &str = "jobs:\n  t:\n    steps:\n      - name: Install system dependencies\n        timeout-minutes: 10\n        uses: ./.github/actions/install-linux-engine-build-dependencies\n        with:\n          packages: glslc libvulkan-dev\n";

    #[test]
    fn rejects_a_bare_apt_get_in_a_workflow() {
        let report = scan_workflow_text(
            "jobs:\n  t:\n    steps:\n      - name: Install system dependencies\n        \
             run: |\n          sudo apt-get update\n          sudo apt-get install -y glslc\n",
        );
        assert_eq!(
            report.unbounded_apt_invocations.len(),
            2,
            "both apt-get lines must be flagged: {report:?}"
        );
    }

    #[test]
    fn accepts_a_workflow_that_uses_the_bounded_action() {
        let report = scan_workflow_text(A_BOUNDED_STEP);
        assert!(
            report.unbounded_apt_invocations.is_empty(),
            "no raw apt-get: {report:?}"
        );
        assert!(
            report.action_calls_without_timeout.is_empty(),
            "the step declares timeout-minutes: {report:?}"
        );
        assert_eq!(report.workflow_steps_calling_the_action, 1);
    }

    #[test]
    fn rejects_an_action_call_missing_the_native_ceiling() {
        let report = scan_workflow_text(
            "jobs:\n  t:\n    steps:\n      - name: Install system dependencies\n        \
             uses: ./.github/actions/install-linux-engine-build-dependencies\n        \
             with:\n          packages: glslc\n",
        );
        assert_eq!(
            report.action_calls_without_timeout.len(),
            1,
            "a step without timeout-minutes must be flagged: {report:?}"
        );
    }

    #[test]
    fn a_timeout_on_a_neighbouring_step_does_not_count() {
        let report = scan_workflow_text(
            "jobs:\n  t:\n    steps:\n      - name: Something else\n        \
             timeout-minutes: 8\n        run: echo hi\n      - name: Install system dependencies\n        \
             uses: ./.github/actions/install-linux-engine-build-dependencies\n        \
             with:\n          packages: glslc\n",
        );
        assert_eq!(
            report.action_calls_without_timeout.len(),
            1,
            "the sibling's bound must not satisfy the action's step: {report:?}"
        );
    }

    #[test]
    fn a_with_input_named_timeout_minutes_does_not_satisfy_the_gate() {
        let report = scan_workflow_text(
            "jobs:\n  t:\n    steps:\n      - name: Install system dependencies\n        \
             uses: ./.github/actions/install-linux-engine-build-dependencies\n        \
             with:\n          timeout-minutes: 10\n          packages: glslc\n",
        );
        assert_eq!(
            report.action_calls_without_timeout.len(),
            1,
            "an input is not a step ceiling: {report:?}"
        );
    }

    #[test]
    fn a_bare_dash_step_is_still_a_step() {
        // `-` alone with the keys on following lines is a legal sequence item,
        // and a splitter that misses it would let an unbounded step through.
        let report = scan_workflow_text(
            "jobs:\n  t:\n    steps:\n      -\n        name: Install system dependencies\n        \
             uses: ./.github/actions/install-linux-engine-build-dependencies\n",
        );
        assert_eq!(
            report.workflow_steps_calling_the_action, 1,
            "the bare-dash step must be seen: {report:?}"
        );
        assert_eq!(
            report.action_calls_without_timeout.len(),
            1,
            "and it must be flagged as unbounded: {report:?}"
        );
    }

    #[test]
    fn a_nested_list_item_does_not_split_its_step_away_from_its_ceiling() {
        let report = scan_workflow_text(
            "jobs:\n  t:\n    steps:\n      - name: Install system dependencies\n        \
             timeout-minutes: 10\n        uses: ./.github/actions/install-linux-engine-build-dependencies\n        \
             with:\n          packages: glslc\n          extra:\n            - ./.github/actions/install-linux-engine-build-dependencies\n",
        );
        assert!(
            report.action_calls_without_timeout.is_empty(),
            "the nested item belongs to the bounded step, not to a step of its own: {report:?}"
        );
    }

    #[test]
    fn a_commented_out_action_call_is_not_a_call() {
        // It must not demand a ceiling, and — the one that matters — it must not
        // count toward the liveness check, or a repo whose every real call had
        // been commented out would still read as wired.
        let report = scan_workflow_text(
            "jobs:\n  t:\n    steps:\n      - name: Something else\n        \
             # uses: ./.github/actions/install-linux-engine-build-dependencies\n        \
             run: echo hi\n",
        );
        assert_eq!(
            report.workflow_steps_calling_the_action, 0,
            "a commented `uses:` is not a call: {report:?}"
        );
        assert!(
            report.action_calls_without_timeout.is_empty(),
            "and it demands no ceiling: {report:?}"
        );
    }

    #[test]
    fn the_step_key_indent_is_measured_not_assumed() {
        // `-` followed by three spaces puts the keys at marker + 4. Deriving the
        // key indent as marker + 2 would look straight past this ceiling.
        let report = scan_workflow_text(
            "jobs:\n  t:\n    steps:\n      -   name: Install system dependencies\n          \
             timeout-minutes: 10\n          \
             uses: ./.github/actions/install-linux-engine-build-dependencies\n",
        );
        assert_eq!(report.workflow_steps_calling_the_action, 1);
        assert!(
            report.action_calls_without_timeout.is_empty(),
            "the widely-indented ceiling must count: {report:?}"
        );
    }

    #[test]
    fn a_step_named_after_apt_is_not_an_apt_invocation() {
        // A dangling front-end at end of line is far more often a step title
        // than a command — and flagging a correct step told its author to use
        // the very action they were already using.
        let report = scan_workflow_text(
            "jobs:\n  t:\n    steps:\n      - name: Install system dependencies with apt\n        \
             timeout-minutes: 10\n        \
             uses: ./.github/actions/install-linux-engine-build-dependencies\n",
        );
        assert!(
            report.unbounded_apt_invocations.is_empty(),
            "a step title is not an invocation: {report:?}"
        );
        assert!(
            report.action_calls_without_timeout.is_empty(),
            "and it is correctly bounded: {report:?}"
        );
    }

    #[test]
    fn a_line_continuation_still_counts_as_an_invocation() {
        assert!(line_fetches_packages("          sudo apt-get \\"));
        assert!(!line_fetches_packages("      - name: Set up apt"));
    }

    #[test]
    fn a_read_only_dpkg_query_is_not_a_package_fetch() {
        assert!(!line_fetches_packages("        run: dpkg -L libclang1-18"));
        assert!(!line_fetches_packages(
            "        run: ls /var/cache/apt/archives"
        ));
        assert!(line_fetches_packages("        run: sudo dpkg -i local.deb"));
        assert!(line_fetches_packages(
            "          sudo DEBIAN_FRONTEND=noninteractive apt install -y cowsay"
        ));
    }

    #[test]
    fn skips_a_commented_apt_get() {
        let report = scan_workflow_text("      # was: sudo apt-get install -y glslc\n");
        assert!(
            report.unbounded_apt_invocations.is_empty(),
            "a comment naming apt-get is not an invocation: {report:?}"
        );
    }

    #[test]
    fn the_repo_itself_passes_the_gate() {
        run(&workspace_root()).expect("the repo's own CI surface must be bounded");
    }

    #[test]
    fn the_gate_refuses_to_pass_vacuously_when_nothing_calls_the_action() {
        let report = BoundedAptInstallScanReport {
            files_scanned: 8,
            ..Default::default()
        };
        let failure = ensure_the_gate_is_still_wired(&workspace_root(), &report)
            .expect_err("zero action calls means the gate is checking nothing");
        assert!(
            format!("{failure:#}").contains("this gate is now checking nothing"),
            "the failure must name the vacuous-pass hazard: {failure:#}"
        );
    }

    #[test]
    fn the_gate_refuses_to_pass_when_the_bounded_retry_script_is_gone() {
        let empty_tree = TempDir::new().unwrap();
        let report = BoundedAptInstallScanReport {
            files_scanned: 8,
            workflow_steps_calling_the_action: 3,
            ..Default::default()
        };
        let failure = ensure_the_gate_is_still_wired(empty_tree.path(), &report)
            .expect_err("a missing script must fail the gate");
        assert!(
            format!("{failure:#}").contains(BOUNDED_RETRY_SCRIPT_RELATIVE_PATH),
            "the failure must name the missing script: {failure:#}"
        );
    }

    // ---- the bounded-retry script's own behaviour ----------------------------
    //
    // The ticket asks for a synthetic check that the timeout path actually
    // fires, rather than a green run that happened to get a fast mirror. These
    // drive the real script with `STREAMLIB_APT_GET_COMMAND` pointed at a
    // fixture, so they need no root, no apt and no network.

    /// Drives the real script against a fake `apt-get` that logs every
    /// subcommand it is handed, so a test can assert what ran and in what order.
    struct BoundedRetryScriptHarness {
        _temp: TempDir,
        script: PathBuf,
        apt_get_fixture: PathBuf,
        mirror_switch_fixture: PathBuf,
        dpkg_repair_fixture: PathBuf,
        invocation_log: PathBuf,
    }

    impl BoundedRetryScriptHarness {
        fn new(apt_get_fixture_body: &str) -> Self {
            let temp = TempDir::new().unwrap();

            let invocation_log = temp.path().join("invocations.log");
            fs::write(&invocation_log, "").unwrap();

            Self {
                script: workspace_root().join(BOUNDED_RETRY_SCRIPT_RELATIVE_PATH),
                apt_get_fixture: write_executable_fixture(
                    temp.path().join("apt-get-fixture"),
                    apt_get_fixture_body,
                ),
                mirror_switch_fixture: write_executable_fixture(
                    temp.path().join("mirror-switch-fixture"),
                    "#!/usr/bin/env bash\nprintf 'switched %s\\n' \"$1\" \
                     >> \"$STREAMLIB_APT_FIXTURE_INVOCATION_LOG\"\n",
                ),
                dpkg_repair_fixture: write_executable_fixture(
                    temp.path().join("dpkg-repair-fixture"),
                    "#!/usr/bin/env bash\nprintf 'dpkg-repaired\\n' \
                     >> \"$STREAMLIB_APT_FIXTURE_INVOCATION_LOG\"\n",
                ),
                _temp: temp,
                invocation_log,
            }
        }

        fn run_with_attempt_bound(&self, attempt_timeout_seconds: u64) -> BoundedRetryScriptRun {
            let result = Command::new(&self.script)
                .arg("glslc")
                .env("STREAMLIB_APT_GET_COMMAND", &self.apt_get_fixture)
                .env(
                    "STREAMLIB_APT_MIRROR_SWITCH_COMMAND",
                    &self.mirror_switch_fixture,
                )
                .env("STREAMLIB_DPKG_REPAIR_COMMAND", &self.dpkg_repair_fixture)
                .env(
                    "STREAMLIB_APT_ATTEMPT_TIMEOUT_SECONDS",
                    attempt_timeout_seconds.to_string(),
                )
                .env("STREAMLIB_APT_FIXTURE_INVOCATION_LOG", &self.invocation_log)
                .env("STREAMLIB_APT_PRIVILEGE_PREFIX", "")
                .output()
                .expect("the bounded-retry script must be executable");

            BoundedRetryScriptRun {
                succeeded: result.status.success(),
                invocation_log: fs::read_to_string(&self.invocation_log).unwrap(),
                stderr: String::from_utf8_lossy(&result.stderr).into_owned(),
            }
        }
    }

    struct BoundedRetryScriptRun {
        succeeded: bool,
        invocation_log: String,
        stderr: String,
    }

    fn write_executable_fixture(path: PathBuf, body: &str) -> PathBuf {
        fs::write(&path, body).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    /// Logs the whole argv, not just the subcommand: the `Acquire::*` options
    /// are part of the contract, and a fixture that recorded only `$1` let them
    /// be deleted wholesale with every test still green.
    const LOG_THE_WHOLE_INVOCATION: &str =
        "printf '%s\\n' \"$*\" >> \"$STREAMLIB_APT_FIXTURE_INVOCATION_LOG\"";

    /// Succeeds on `update`, stalls on `install` until the mirror has been
    /// switched — the shape of the measured incident, where `update` finished
    /// in 65s (inside the bound) and `install` was the command that ran 12m17s.
    ///
    /// A fixture that stalls on *every* subcommand never reaches the install, so
    /// the install-side status handling goes unexercised and a regression there
    /// stays green. That mutant was confirmed to survive the earlier fixtures.
    const STALL_ONLY_ON_THE_INSTALL: &str = "\
        if [ \"$1\" = update ]; then exit 0; fi\n\
        if grep -q switched \"$STREAMLIB_APT_FIXTURE_INVOCATION_LOG\"; then exit 0; fi\n\
        sleep 600\n";

    /// Exits non-zero immediately on the install — a broken package name, not a
    /// slow mirror. The distinction is what the failure message has to get right.
    const FAIL_THE_INSTALL_OUTRIGHT: &str = "\
        if [ \"$1\" = update ]; then exit 0; fi\n\
        exit 100\n";

    fn apt_fixture(body: &str) -> String {
        format!("#!/usr/bin/env bash\n{LOG_THE_WHOLE_INVOCATION}\n{body}")
    }

    fn count_of(log: &str, subcommand: &str) -> usize {
        log.lines()
            .filter(|line| line.split_whitespace().next() == Some(subcommand))
            .count()
    }

    #[test]
    fn a_healthy_mirror_installs_without_switching() {
        let harness = BoundedRetryScriptHarness::new(&apt_fixture("exit 0\n"));
        let run = harness.run_with_attempt_bound(5);
        let log = &run.invocation_log;

        assert!(run.succeeded, "a healthy mirror must succeed; log:\n{log}");
        assert_eq!(count_of(log, "update"), 1, "log:\n{log}");
        assert_eq!(count_of(log, "install"), 1, "log:\n{log}");
        assert!(
            !log.contains("switched"),
            "a healthy run must not touch the mirror; log:\n{log}"
        );
    }

    #[test]
    fn every_apt_command_carries_the_retry_and_timeout_options() {
        // Deleting the whole `apt_acquire_options` array left the suite green
        // while the fixture logged only `$1`. These are the options that cover
        // the transient failure and the silent connection; the wall-clock bound
        // covers neither.
        let harness = BoundedRetryScriptHarness::new(&apt_fixture("exit 0\n"));
        let run = harness.run_with_attempt_bound(5);

        for invocation in run.invocation_log.lines() {
            for option in [
                "Acquire::Retries=3",
                "Acquire::http::Timeout=30",
                "Acquire::https::Timeout=30",
            ] {
                assert!(
                    invocation.contains(option),
                    "`{option}` missing from `{invocation}`"
                );
            }
        }
    }

    #[test]
    fn a_slow_install_trips_the_bound_and_escapes_to_the_fallback() {
        let harness = BoundedRetryScriptHarness::new(&apt_fixture(STALL_ONLY_ON_THE_INSTALL));
        let run = harness.run_with_attempt_bound(2);
        let log = &run.invocation_log;

        assert!(
            log.contains("switched"),
            "the install-side bound must fire and repoint the mirror; log:\n{log}"
        );
        assert!(
            run.succeeded,
            "the fallback mirror must carry the install home; log:\n{log}"
        );
        assert_eq!(
            count_of(log, "install"),
            2,
            "the install must be attempted once per mirror; log:\n{log}"
        );
        assert!(
            run.stderr.contains("did not finish inside the 2s bound"),
            "a stalled mirror must be reported as a timeout; stderr:\n{}",
            run.stderr
        );
    }

    #[test]
    fn an_interrupted_dpkg_is_repaired_before_the_fallback_attempt() {
        // The bound can fire mid-unpack, and the SIGINT reaches dpkg too. apt
        // then refuses every later install until dpkg is reconfigured, which
        // would make the fallback attempt fail deterministically.
        let harness = BoundedRetryScriptHarness::new(&apt_fixture(STALL_ONLY_ON_THE_INSTALL));
        let run = harness.run_with_attempt_bound(2);
        let log = &run.invocation_log;

        let repaired = log.lines().position(|line| line == "dpkg-repaired");
        let switched = log.lines().position(|line| line.starts_with("switched"));
        assert!(
            repaired.is_some() && switched.is_some() && repaired < switched,
            "dpkg must be repaired before the mirror switch; log:\n{log}"
        );
    }

    #[test]
    fn a_broken_package_is_not_reported_as_a_slow_mirror() {
        // The likeliest deterministic failure here is a version-pinned package
        // name a runner-image roll retired. Calling that a timeout sends the
        // reader hunting a network problem that is not there.
        let harness = BoundedRetryScriptHarness::new(&apt_fixture(FAIL_THE_INSTALL_OUTRIGHT));
        let run = harness.run_with_attempt_bound(5);

        assert!(!run.succeeded, "a broken package must fail the step");
        assert!(
            run.stderr.contains("failed with apt exit status 100"),
            "the real exit status must be reported; stderr:\n{}",
            run.stderr
        );
        assert!(
            !run.stderr.contains("did not finish inside"),
            "nothing timed out, so nothing may say so; stderr:\n{}",
            run.stderr
        );
    }

    #[test]
    fn both_mirrors_failing_on_the_install_exits_non_zero_rather_than_hanging() {
        let harness = BoundedRetryScriptHarness::new(&apt_fixture(FAIL_THE_INSTALL_OUTRIGHT));
        let run = harness.run_with_attempt_bound(5);
        let log = &run.invocation_log;

        assert!(
            !run.succeeded,
            "exhausting both mirrors must fail; log:\n{log}"
        );
        assert_eq!(
            count_of(log, "install"),
            2,
            "the install must be tried once per mirror, no more; log:\n{log}"
        );
    }

    #[test]
    fn a_failing_update_does_not_go_on_to_install() {
        let harness = BoundedRetryScriptHarness::new(&apt_fixture("exit 100\n"));
        let run = harness.run_with_attempt_bound(5);
        let log = &run.invocation_log;

        assert!(
            !run.succeeded,
            "exhausting both mirrors must fail; log:\n{log}"
        );
        assert_eq!(
            count_of(log, "update"),
            2,
            "one update per mirror; log:\n{log}"
        );
        assert_eq!(
            count_of(log, "install"),
            0,
            "a failed update must not be followed by an install; log:\n{log}"
        );
    }
}
