// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

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
//! Stated blind spots. `timeout-minutes:` counts only at a step's own key
//! indentation, so a `with:` input of that name does not satisfy the gate. A
//! step spelled across a YAML anchor or an `!!merge` key is not recognised as a
//! step at all. The front-end list is the apt family only, so
//! `reusable-extension-wheel.yml`'s `dnf install` inside its manylinux container
//! passes — that job is release-time, runs in a container this action cannot
//! serve, and is deliberately out of scope; it carries its own wall-clock-bounded
//! retry instead. Two more live on [`line_fetches_packages`], which owns the
//! shell heuristic.
//!
//! The scan root is `.github/` alone, so a workflow step that shells out to a
//! script elsewhere in the repo is uncovered — `ci.yml`'s repo gates already run
//! `bash scripts/check-license-headers.sh`, and `scripts/docker/host-prereqs.sh`
//! already installs apt packages unbounded. Widening the root would put every
//! developer-facing script in the repo under a CI rule written for CI.
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
/// Global flags between the front-end and its subcommand are skipped, so
/// `apt-get -y install` and `apt-get -qq update` both read as fetches. `--`
/// stops the skip: after end-of-options a word is prose, not a subcommand.
///
/// Two blind spots, stated rather than papered over: a front-end whose
/// subcommand is built from a shell variable (`apt-get "$verb"`), and a flag
/// whose value is a separate token (`apt-get -o Foo=bar update`) — the value
/// does not start with `-`, so the skip stops on it.
fn line_fetches_packages(line: &str) -> bool {
    let mut tokens = line.split_whitespace().peekable();

    while let Some(token) = tokens.next() {
        if PACKAGE_FETCH_FRONT_ENDS.contains(&token) {
            let mut following = tokens.peek().copied();
            while following.is_some_and(|word| word != "--" && word.starts_with('-')) {
                tokens.next();
                following = tokens.peek().copied();
            }
            match following {
                Some("\\") => return true,
                Some(subcommand) if PACKAGE_FETCH_SUBCOMMANDS.contains(&subcommand) => return true,
                _ => continue,
            }
        }

        if token == "dpkg"
            && tokens
                .peek()
                .is_some_and(|flag| DPKG_INSTALL_FLAGS.contains(flag))
        {
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
                && indentation_width(line) == step.key_indentation_width
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
    key_indentation_width: usize,
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

        let list_marker_indentation_width = indentation_width(line);
        let mut block_lines: Vec<&str> = vec![*line];
        let mut end = index + 1;

        for (following_index, following_line) in lines.iter().enumerate().skip(index + 1) {
            if following_line.trim().is_empty() {
                continue;
            }
            if indentation_width(following_line) <= list_marker_indentation_width {
                break;
            }
            block_lines.push(following_line);
            end = following_index + 1;
        }

        // `- key:` puts the first key on the marker line, after however much
        // space follows the dash; a bare `-` puts it on the next line.
        let after_the_dash = &trimmed[1..];
        let key_indentation_width = if after_the_dash.trim().is_empty() {
            block_lines
                .get(1)
                .map_or(list_marker_indentation_width + 2, |line| {
                    indentation_width(line)
                })
        } else {
            list_marker_indentation_width + 1 + indentation_width(after_the_dash)
        };

        next_unclaimed_line = end;
        blocks.push(WorkflowStepBlock {
            first_line_number: index + 1,
            key_indentation_width,
            lines: block_lines,
        });
    }

    blocks
}

#[cfg(test)]
mod tests;
