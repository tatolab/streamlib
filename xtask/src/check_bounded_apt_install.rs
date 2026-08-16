// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Keeps every apt install in CI behind the bounded-retry action.
//!
//! A workflow that installs packages itself has no wall-clock bound, and the
//! mode that costs is not the one people write guards for — the script's own
//! header records the measurement and why apt's guards miss it.
//!
//! So this is a location rule: a package install belongs in
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
//! Two stated blind spots, both narrowed as far as an indentation scan can:
//! `timeout-minutes:` counts only at a step's own key indentation, so a `with:`
//! input of that name does not satisfy the gate; and a step spelled across a
//! YAML anchor or an `!!merge` key would not be recognised as a step at all.
//! Neither appears in this repo, and [`ensure_the_gate_is_still_wired`] is what
//! stops the whole gate from passing vacuously if the shapes it keys on move.

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

/// The one file under `.github/` allowed to invoke `apt-get`.
const BOUNDED_RETRY_SCRIPT_RELATIVE_PATH: &str = ".github/actions/install-linux-engine-build-dependencies/install-system-dependencies-with-bounded-retry.sh";

/// Everything CI runs lives here — workflows and composite actions alike.
const GITHUB_CI_SCAN_ROOT: &str = ".github";

/// Only a workflow step can carry `timeout-minutes`; a composite action's cannot.
const WORKFLOW_FILE_PREFIX: &str = ".github/workflows/";

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
            "{}:{}: fetches packages under {} — install through `{}` instead, which bounds \
             each command and escapes to a second mirror. The only file allowed to fetch \
             packages is `{}`. Offending line: {}",
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
        "check-bounded-apt-install: {} files scanned, {} bounded action call(s), no raw apt-get",
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
         there — the gate's `apt-get` ban would then point callers at a file that does not \
         exist. Update BOUNDED_RETRY_SCRIPT_RELATIVE_PATH if the script moved.",
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
        if file_path == BOUNDED_RETRY_SCRIPT_RELATIVE_PATH {
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
/// Blind spot, stated rather than papered over: a front-end whose subcommand is
/// built from a shell variable (`apt-get "$verb"`) reads as neither. A trailing
/// `\` continuation does count, because a front-end left dangling at end of line
/// is always the head of a wrapped invocation.
fn line_fetches_packages(line: &str) -> bool {
    let tokens: Vec<&str> = line.split_whitespace().collect();

    tokens.iter().enumerate().any(|(position, token)| {
        let following = tokens.get(position + 1).copied();
        match *token {
            front_end if PACKAGE_FETCH_FRONT_ENDS.contains(&front_end) => match following {
                None | Some("\\") => true,
                Some(subcommand) => PACKAGE_FETCH_SUBCOMMANDS.contains(&subcommand),
            },
            "dpkg" => following.is_some_and(|flag| DPKG_INSTALL_FLAGS.contains(&flag)),
            _ => false,
        }
    })
}

fn collect_action_calls(
    workflow_path: &str,
    contents: &str,
    report: &mut BoundedAptInstallScanReport,
) {
    for step in split_into_step_blocks(contents) {
        if !step
            .lines
            .iter()
            .any(|line| line.contains(BOUNDED_APT_INSTALL_ACTION_REFERENCE))
        {
            continue;
        }

        report.workflow_steps_calling_the_action += 1;

        // Only at the step's own key indentation: a `with:` input that happens
        // to be named `timeout-minutes` is an input, not a ceiling.
        let step_key_indent = step.list_marker_indent + 2;
        let declares_a_timeout = step.lines.iter().any(|line| {
            line.trim_start().starts_with("timeout-minutes:")
                && line.len() - line.trim_start().len() == step_key_indent
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
    list_marker_indent: usize,
    lines: Vec<&'a str>,
}

/// Split a workflow into top-level YAML list-item blocks.
///
/// A block runs from its `-` line to the next non-blank line indented no
/// further, which keeps a nested `with:` mapping inside the step it belongs to.
/// Blocks never overlap: a `-` already swallowed by an open block is one of that
/// step's own list items, not a sibling step, and spawning a block for it would
/// produce a step that excludes its own `timeout-minutes`.
///
/// Indentation-based rather than parsed — the gate needs to know which lines
/// share a step, not what the YAML means, and the repo has no YAML dependency to
/// add for it.
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

        let list_marker_indent = line.len() - trimmed.len();
        let mut block_lines: Vec<&str> = vec![*line];
        let mut end = index + 1;

        for (following_index, following_line) in lines.iter().enumerate().skip(index + 1) {
            if following_line.trim().is_empty() {
                continue;
            }
            if following_line.len() - following_line.trim_start().len() <= list_marker_indent {
                break;
            }
            block_lines.push(following_line);
            end = following_index + 1;
        }

        next_unclaimed_line = end;
        blocks.push(WorkflowStepBlock {
            first_line_number: index + 1,
            list_marker_indent,
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

        fn run_with_attempt_bound(&self, attempt_timeout_seconds: u64) -> (bool, String) {
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
                .output()
                .expect("the bounded-retry script must be executable");

            (
                result.status.success(),
                fs::read_to_string(&self.invocation_log).unwrap(),
            )
        }
    }

    fn write_executable_fixture(path: PathBuf, body: &str) -> PathBuf {
        fs::write(&path, body).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    const LOG_THE_SUBCOMMAND: &str =
        "printf '%s\\n' \"$1\" >> \"$STREAMLIB_APT_FIXTURE_INVOCATION_LOG\"";

    #[test]
    fn a_healthy_mirror_installs_without_switching() {
        let harness = BoundedRetryScriptHarness::new(&format!(
            "#!/usr/bin/env bash\n{LOG_THE_SUBCOMMAND}\nexit 0\n"
        ));
        let (succeeded, log) = harness.run_with_attempt_bound(5);

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

    fn count_of(log: &str, subcommand: &str) -> usize {
        log.lines().filter(|line| *line == subcommand).count()
    }

    #[test]
    fn a_slow_install_trips_the_bound_and_escapes_to_the_fallback() {
        let harness = BoundedRetryScriptHarness::new(&format!(
            "#!/usr/bin/env bash\n{LOG_THE_SUBCOMMAND}\n{STALL_ONLY_ON_THE_INSTALL}"
        ));
        let (succeeded, log) = harness.run_with_attempt_bound(2);

        assert!(
            log.contains("switched"),
            "the install-side bound must fire and repoint the mirror; log:\n{log}"
        );
        assert!(
            succeeded,
            "the fallback mirror must carry the install home; log:\n{log}"
        );
        assert_eq!(
            count_of(&log, "install"),
            2,
            "the install must be attempted once per mirror; log:\n{log}"
        );
    }

    #[test]
    fn an_interrupted_dpkg_is_repaired_before_the_fallback_attempt() {
        // The bound can fire mid-unpack, and the SIGINT reaches dpkg too. apt
        // then refuses every later install until dpkg is reconfigured, which
        // would make the fallback attempt fail deterministically.
        let harness = BoundedRetryScriptHarness::new(&format!(
            "#!/usr/bin/env bash\n{LOG_THE_SUBCOMMAND}\n{STALL_ONLY_ON_THE_INSTALL}"
        ));
        let (_, log) = harness.run_with_attempt_bound(2);

        let repaired = log.lines().position(|line| line == "dpkg-repaired");
        let switched = log.lines().position(|line| line.starts_with("switched"));
        assert!(
            repaired.is_some() && switched.is_some() && repaired < switched,
            "dpkg must be repaired before the mirror switch; log:\n{log}"
        );
    }

    #[test]
    fn both_mirrors_failing_on_the_install_exits_non_zero_rather_than_hanging() {
        let harness = BoundedRetryScriptHarness::new(&format!(
            "#!/usr/bin/env bash\n{LOG_THE_SUBCOMMAND}\n\
             if [ \"$1\" = update ]; then exit 0; fi\nexit 100\n"
        ));
        let (succeeded, log) = harness.run_with_attempt_bound(5);

        assert!(!succeeded, "exhausting both mirrors must fail; log:\n{log}");
        assert_eq!(
            count_of(&log, "install"),
            2,
            "the install must be tried once per mirror, no more; log:\n{log}"
        );
    }

    #[test]
    fn a_failing_update_does_not_go_on_to_install() {
        let harness = BoundedRetryScriptHarness::new(&format!(
            "#!/usr/bin/env bash\n{LOG_THE_SUBCOMMAND}\nexit 100\n"
        ));
        let (succeeded, log) = harness.run_with_attempt_bound(5);

        assert!(!succeeded, "exhausting both mirrors must fail; log:\n{log}");
        assert_eq!(
            count_of(&log, "update"),
            2,
            "one update per mirror; log:\n{log}"
        );
        assert_eq!(
            count_of(&log, "install"),
            0,
            "a failed update must not be followed by an install; log:\n{log}"
        );
    }
}
