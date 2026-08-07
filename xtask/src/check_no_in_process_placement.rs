// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! CI lint enforcing the helper-process-placement-only ruling (owner 2026-08-04).
//!
//! Every Python processor runs in its own child process with its own
//! interpreter and its own GIL; hosting one in the app's interpreter is banned
//! outright, and so is any diagnostic whose premise is that processors share a
//! GIL or an interpreter. See `docs/decisions/helper-process-placement-only.md`
//! and `.claude/rules/placement.md`.
//!
//! Two deliberate inversions of the `check_no_reverse_dns` shape this lint is
//! otherwise modeled on:
//!
//! - **Prose is in scope, not out of it.** This lint is line-based over `.md`
//!   files and over Rust doc comments and line comments. The shipped violation
//!   announced itself in a `//!` line ("One interpreter runs every Python
//!   processor") that three review rounds read past; an AST walk over string
//!   literals is blind to exactly that. Do not "fix" this lint by moving it to
//!   `syn`.
//! - **`docs/` is a scan root**, and `spikes/` must stay absent — the
//!   `streamlib-pyembed-spike` tree published the retracted latency numbers.
//!
//! Two escape hatches, both narrow:
//!
//! - [`ALLOW_FILE_PRAGMA`] exempts a whole file. Reserved for the documents that
//!   have to quote the banned vocabulary at length in order to retract and ban
//!   it; [`EXPECTED_ALLOW_FILE_PATHS`] pins the set.
//! - [`EXEMPT_PROHIBITION_LINES`] exempts individual lines that state the
//!   prohibition itself in `docs/plan/**` — files a source session cannot edit,
//!   since plan edits are gated to the plan skills. Exempting those files whole
//!   would blind the lint at the one place the ADR blames for the original
//!   drift, so the exemption is per line and matched on exact text: a new banned
//!   line in `ARCHITECTURE.md` still fails.
//!
//! Markdown supersession spans (`~~…~~`) are skipped, because the docs policy
//! requires a retraction to quote what it retracts.
//!
//! Honest limits, so nobody reads a green run as more than it is:
//!
//! - This is vocabulary, not behaviour. A watchdog renamed to avoid these
//!   patterns passes. The behavioural proof that the parent never hosts a
//!   processor class is `sdk/streamlib-python-wheel/tests/test_helper_placement.py`.
//! - `.claude/` is not scanned — the rules and reviewer definitions quote the
//!   banned shapes to forbid them, and they change through their own dedicated
//!   PRs.
//! - The repo-root `CHANGELOG.md` is not scanned: it is release-please-generated
//!   history of what shipped, not a claim about what the runtime is.

// check-no-in-process-placement:allow-file — this file defines the banned
// patterns and so must contain them literally.

use anyhow::{Context, Result};
use std::borrow::Cow;
use std::fs;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

const SCAN_PARENTS: &[&str] = &[
    "runtime", "sdk", "adapters", "tools", "packages", "examples", "xtask", "docs",
];

/// The retracted spike tree. Its README was the last place in the tree where a
/// grep returned "in-process beats the baseline" as a live claim.
const FORBIDDEN_TREE: &str = "spikes";

const SKIP_PATH_FRAGMENTS: &[&str] = &[
    "/target/",
    "/_generated_/",
    "/node_modules/",
    "/.git/",
    "/.venv/",
];

const SCAN_EXTENSIONS: &[&str] = &["rs", "md", "py", "pyi", "ts", "toml", "yaml", "yml"];

/// Marker comment that exempts an entire file. See the module doc — the set is
/// pinned by [`EXPECTED_ALLOW_FILE_PATHS`] and every member is a retraction.
const ALLOW_FILE_PRAGMA: &str = "check-no-in-process-placement:allow-file";

/// Every file allowed to carry the pragma, each because it must quote the
/// banned vocabulary at length to retract and ban it. Pinned so a fifth
/// exemption is a deliberate, reviewed edit rather than a comment someone
/// pasted to get CI green.
const EXPECTED_ALLOW_FILE_PATHS: &[&str] = &[
    "docs/decisions/helper-process-placement-only.md",
    "docs/decisions/importable-python-library.md",
    "docs/plan/changes/archive/2026-08-07-in-process-hosting-ripout.md",
    "xtask/src/check_no_in_process_placement.rs",
];

/// Lines that state the prohibition, in plan documents a source session cannot
/// edit. Matched on exact trimmed text, so editing one of these lines fails the
/// lint and forces the edit through review, and a *new* banned line in the same
/// file still fails.
///
/// This list only shrinks. A line that leaves its document is a stale entry and
/// [`exempt_prohibition_lines_are_all_live`] fails until it is deleted here.
const EXEMPT_PROHIBITION_LINES: &[(&str, &str)] = &[
    (
        "docs/plan/ARCHITECTURE.md",
        "In-process hosting of a Python processor does not exist — not as a default, a",
    ),
    (
        "docs/plan/GLOSSARY.md",
        "in-process hosting of a Python processor does not exist. Native built-ins running in",
    ),
    (
        "docs/plan/GLOSSARY.md",
        "\"in-process placement\", \"both placements\", \"placement policy\", \"placement heuristic\",",
    ),
    (
        "docs/plan/GLOSSARY.md",
        "`docs/decisions/helper-process-placement-only.md`): **In-process placement**,",
    ),
    (
        "docs/plan/changes/importable-python-library.md",
        "\"In-process Python authoring\" — in-process hosting of a Python processor is banned,",
    ),
    (
        "docs/plan/changes/importable-python-library.md",
        "(A \"dev-mode GIL-hold watchdog\" clause was removed here 2026-08-04: it measured",
    ),
];

/// A banned shape. `all_of` terms must every one appear on the line;
/// `any_of` and `also_requires_any_of` — when non-empty — each need one hit.
/// All matching is ASCII-case-insensitive on a lowercased copy of the line.
struct BannedShape {
    all_of: &'static [&'static str],
    any_of: &'static [&'static str],
    also_requires_any_of: &'static [&'static str],
    guidance: &'static str,
}

/// What matched, and what to tell the author about it.
struct BannedShapeMatch {
    matched: Cow<'static, str>,
    guidance: &'static str,
}

const CO_TENANCY_GUIDANCE: &str = "processors never share an interpreter or a GIL — every Python processor runs in its own helper process";
const DIAGNOSTIC_GUIDANCE: &str = "a diagnostic premised on shared-GIL contention measures something that cannot happen under helper-process placement";
const PLACEMENT_GUIDANCE: &str =
    "there is one placement; say `app-process` for the legitimate in-that-process senses";
const RETRACTED_NUMBERS_GUIDANCE: &str = "the #1702 spike's latency numbers are retracted as placement evidence — the two arms ran different CPython builds and were never re-measured";

/// The paired-term rules exist because the bare words are legitimate: the
/// engine has an EPOLLHUP watchdog, `rt.run()` documents a GIL-release
/// contract about its own interpreter's threads, and `both placements` appears
/// in the very sentences that retire it.
const BANNED_SHAPES: &[BannedShape] = &[
    BannedShape {
        all_of: &["interpreter", "processor"],
        any_of: &["shared interpreter", "one interpreter", "same interpreter"],
        also_requires_any_of: &[],
        guidance: CO_TENANCY_GUIDANCE,
    },
    BannedShape {
        all_of: &["gil", "contention"],
        any_of: &[],
        also_requires_any_of: &[],
        guidance: DIAGNOSTIC_GUIDANCE,
    },
    BannedShape {
        all_of: &["gil", "stall", "processor"],
        any_of: &[],
        also_requires_any_of: &[],
        guidance: CO_TENANCY_GUIDANCE,
    },
    BannedShape {
        all_of: &["gil", "every other"],
        any_of: &[],
        also_requires_any_of: &[],
        guidance: CO_TENANCY_GUIDANCE,
    },
    BannedShape {
        all_of: &[],
        any_of: &["gil-hold", "gil hold", "slow-callback", "slow callback", "stall-attribution", "stall attribution"],
        also_requires_any_of: WATCHDOG_NOUNS,
        guidance: DIAGNOSTIC_GUIDANCE,
    },
    BannedShape {
        all_of: &["both placements viable"],
        any_of: &[],
        also_requires_any_of: &[],
        guidance: PLACEMENT_GUIDANCE,
    },
    BannedShape {
        all_of: &[],
        any_of: &["in-process placement", "in-process hosting", "in-process authoring"],
        also_requires_any_of: &[],
        guidance: PLACEMENT_GUIDANCE,
    },
    BannedShape {
        all_of: &[],
        any_of: &["0.085ms", "0.085 ms", "0.161ms", "0.161 ms", "0.089ms", "0.089 ms", "0.180ms", "0.180 ms"],
        also_requires_any_of: &[],
        guidance: RETRACTED_NUMBERS_GUIDANCE,
    },
];

/// The watchdog family needs its diagnostic name *and* a monitor noun, or
/// every `GIL-holding thread` doc comment trips it.
const WATCHDOG_NOUNS: &[&str] = &["watchdog", "monitor", "detector"];

#[derive(Debug, PartialEq, Eq)]
pub struct LintViolation {
    pub file: PathBuf,
    pub line: usize,
    pub matched: String,
    pub guidance: &'static str,
}

#[derive(Debug, Default)]
pub struct InProcessPlacementScanReport {
    pub violations: Vec<LintViolation>,
    pub files_scanned: usize,
    pub allow_filed: Vec<PathBuf>,
    pub exempt_lines_hit: usize,
}

pub fn run(workspace_root: &Path) -> Result<()> {
    ensure_forbidden_tree_absent(workspace_root)?;
    let report = lint_workspace(workspace_root)?;
    ensure_allow_file_set_is_pinned(workspace_root, &report)?;
    crate::ensure_source_walking_gate_read_source(
        "check-no-in-process-placement",
        &format!("{SCAN_PARENTS:?}"),
        report.files_scanned,
        "in-process placement vocabulary re-enter the tree",
    )?;

    if report.violations.is_empty() {
        println!(
            "✓ check-no-in-process-placement: {} files scanned, {} allow-file'd, {} exempt prohibition line(s) matched",
            report.files_scanned,
            report.allow_filed.len(),
            report.exempt_lines_hit,
        );
        return Ok(());
    }

    eprintln!(
        "✗ check-no-in-process-placement: {} violation(s)",
        report.violations.len()
    );
    for v in &report.violations {
        eprintln!(
            "  {}:{}: banned placement vocabulary `{}` — {}. See docs/decisions/helper-process-placement-only.md",
            v.file.display(),
            v.line,
            v.matched,
            v.guidance,
        );
    }
    anyhow::bail!(
        "in-process placement lint failed: {} violation(s)",
        report.violations.len()
    );
}

/// A pragma pasted onto a fifth file would silently exempt it, so the set is
/// pinned here rather than only in the tests — the gate refuses a tree whose
/// allow-file set is not exactly [`EXPECTED_ALLOW_FILE_PATHS`].
fn ensure_allow_file_set_is_pinned(
    workspace_root: &Path,
    report: &InProcessPlacementScanReport,
) -> Result<()> {
    let mut found: Vec<String> = report
        .allow_filed
        .iter()
        .map(|path| {
            path.strip_prefix(workspace_root)
                .unwrap_or(path)
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect();
    found.sort();
    let mut expected: Vec<String> = EXPECTED_ALLOW_FILE_PATHS.iter().map(|p| p.to_string()).collect();
    expected.sort();
    anyhow::ensure!(
        found == expected,
        "the `{ALLOW_FILE_PRAGMA}` set is {found:?}, pinned as {expected:?} — every member must be \
         a document that quotes the banned vocabulary in order to retract it"
    );
    Ok(())
}

/// The retracted spike tree must stay deleted: a green vocabulary scan over a
/// resurrected `spikes/` would still leave its published tables in the tree.
fn ensure_forbidden_tree_absent(workspace_root: &Path) -> Result<()> {
    let tree = workspace_root.join(FORBIDDEN_TREE);
    anyhow::ensure!(
        !tree.exists(),
        "{}/ is back in the tree — it published the retracted #1702 latency numbers and was \
         deleted with the in-process-hosting rip-out. See docs/decisions/helper-process-placement-only.md",
        FORBIDDEN_TREE
    );
    Ok(())
}

pub fn lint_workspace(workspace_root: &Path) -> Result<InProcessPlacementScanReport> {
    let mut report = InProcessPlacementScanReport::default();
    for parent in SCAN_PARENTS {
        let dir = workspace_root.join(parent);
        if !dir.exists() {
            continue;
        }
        scan_dir(workspace_root, &dir, &mut report)?;
    }
    Ok(report)
}

fn scan_dir(workspace_root: &Path, dir: &Path, report: &mut InProcessPlacementScanReport) -> Result<()> {
    for entry in WalkDir::new(dir).into_iter().filter_map(|e| e.ok()) {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let path_str = path.to_string_lossy();
        if SKIP_PATH_FRAGMENTS.iter().any(|frag| path_str.contains(frag)) {
            continue;
        }
        let extension = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if !SCAN_EXTENSIONS.contains(&extension) {
            continue;
        }
        scan_file(workspace_root, path, extension, report)?;
    }
    Ok(())
}

fn scan_file(
    workspace_root: &Path,
    path: &Path,
    extension: &str,
    report: &mut InProcessPlacementScanReport,
) -> Result<()> {
    let body =
        fs::read_to_string(path).with_context(|| format!("Failed to read {}", path.display()))?;
    report.files_scanned += 1;
    if body.contains(ALLOW_FILE_PRAGMA) {
        report.allow_filed.push(path.to_path_buf());
        return Ok(());
    }

    let relative = path
        .strip_prefix(workspace_root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/");

    // Supersession spans are a markdown convention; a stray `~~` in Rust or YAML
    // is not one, so only markdown gets the state machine and the unclosed-span refusal.
    let is_markdown = extension == "md";
    if is_markdown {
        ensure_supersession_spans_balanced(path, &body)?;
    }

    let mut spans = SupersessionSpanState::default();
    for (idx, line) in body.lines().enumerate() {
        let scanned = if is_markdown {
            spans.outside_spans(line)
        } else {
            Cow::Borrowed(line)
        };
        if scanned.trim().is_empty() {
            continue;
        }
        if is_exempt_prohibition_line(&relative, line) {
            report.exempt_lines_hit += 1;
            continue;
        }
        if let Some(hit) = first_banned_shape(&scanned) {
            report.violations.push(LintViolation {
                file: path.to_path_buf(),
                line: idx + 1,
                matched: hit.matched.into_owned(),
                guidance: hit.guidance,
            });
        }
    }
    Ok(())
}

fn is_exempt_prohibition_line(relative_path: &str, line: &str) -> bool {
    EXEMPT_PROHIBITION_LINES
        .iter()
        .any(|(path, text)| *path == relative_path && line.trim() == *text)
}

/// An unterminated `~~` would silently swallow the whole rest of the file, so a
/// file whose spans do not close is refused rather than scanned.
fn ensure_supersession_spans_balanced(path: &Path, body: &str) -> Result<()> {
    let mut spans = SupersessionSpanState::default();
    for line in body.lines() {
        spans.outside_spans(line);
    }
    anyhow::ensure!(
        !spans.inside_span,
        "{}: unterminated supersession span — the unclosed `~~` would hide every line after it \
         from check-no-in-process-placement. Close the span.",
        path.display(),
    );
    Ok(())
}

/// Markdown span tracking across a file's lines: a `~~…~~` supersession span
/// that opens on one line and closes on a later one, and a `~~~` fenced code
/// block, whose fence delimiters are not span markers.
#[derive(Default)]
struct SupersessionSpanState {
    inside_span: bool,
    inside_code_fence: bool,
}

impl SupersessionSpanState {
    /// The portion of `line` outside any supersession span. Fence delimiters
    /// and the lines they enclose are returned whole — fenced code is still
    /// scanned for banned vocabulary, it just cannot open or close a span.
    fn outside_spans<'a>(&mut self, line: &'a str) -> Cow<'a, str> {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            self.inside_code_fence = !self.inside_code_fence;
            return Cow::Borrowed(line);
        }
        if self.inside_code_fence || (!self.inside_span && !line.contains("~~")) {
            return Cow::Borrowed(line);
        }

        let mut outside = String::new();
        let mut rest = line;
        loop {
            match rest.find("~~") {
                Some(pos) => {
                    if !self.inside_span {
                        outside.push_str(&rest[..pos]);
                    }
                    self.inside_span = !self.inside_span;
                    rest = &rest[pos + 2..];
                }
                None => {
                    if !self.inside_span {
                        outside.push_str(rest);
                    }
                    return Cow::Owned(outside);
                }
            }
        }
    }
}

fn first_banned_shape(line: &str) -> Option<BannedShapeMatch> {
    let haystack = line.to_ascii_lowercase();
    for shape in BANNED_SHAPES {
        if !shape.all_of.iter().all(|term| haystack.contains(term)) {
            continue;
        }
        if !shape.also_requires_any_of.is_empty()
            && !shape
                .also_requires_any_of
                .iter()
                .any(|term| haystack.contains(term))
        {
            continue;
        }
        let matched = if shape.any_of.is_empty() {
            Cow::Owned(shape.all_of.join(" + "))
        } else {
            match shape.any_of.iter().copied().find(|t| haystack.contains(t)) {
                Some(term) => Cow::Borrowed(term),
                None => continue,
            }
        };
        return Some(BannedShapeMatch {
            matched,
            guidance: shape.guidance,
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write(dir: &Path, rel: &str, body: &str) -> PathBuf {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, body).unwrap();
        path
    }

    fn lint(dir: &Path) -> Vec<LintViolation> {
        lint_workspace(dir).unwrap().violations
    }

    /// Every banned shape, red on the violating phrasing and green on the
    /// legitimate near-miss that motivated its pairing.
    #[test]
    fn flags_shared_interpreter_claims_about_processors() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "runtime/foo/src/lib.rs",
            "//! One interpreter runs every Python processor.\npub fn ok() {}\n",
        );
        assert_eq!(lint(tmp.path()).len(), 1);
    }

    #[test]
    fn allows_one_interpreter_prose_that_names_no_processor() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "sdk/foo/pyproject.toml",
            "# one interpreter's stale `.pyc` files inside an abi3 wheel.\n",
        );
        assert!(lint(tmp.path()).is_empty());
    }

    #[test]
    fn flags_gil_contention() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "runtime/foo/src/lib.rs",
            "/// Reports GIL contention across the graph.\npub fn ok() {}\n",
        );
        assert_eq!(lint(tmp.path()).len(), 1);
    }

    #[test]
    fn flags_a_gil_stall_attributed_to_a_processor() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "runtime/foo/src/lib.rs",
            "//! Holding the GIL stalls every other Python processor.\npub fn ok() {}\n",
        );
        assert_eq!(lint(tmp.path()).len(), 1);
    }

    /// The GIL-release contract is about the child's own interpreter's threads —
    /// the explicit boundary the ruling preserves, and the live doc comment in
    /// `python_processor_link_data_access.rs`.
    #[test]
    fn allows_the_gil_release_contract_about_its_own_threads() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "sdk/foo/src/data_access.rs",
            "//! Holding the GIL across that call would stall the interpreter's other threads.\n",
        );
        assert!(lint(tmp.path()).is_empty());
    }

    #[test]
    fn flags_the_watchdog_family() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "runtime/foo/src/lib.rs",
            "/// The dev-mode GIL-hold watchdog.\npub fn ok() {}\n",
        );
        write(
            tmp.path(),
            "runtime/bar/src/lib.rs",
            "/// A slow-callback monitor for the graph.\npub fn ok() {}\n",
        );
        write(
            tmp.path(),
            "runtime/baz/src/lib.rs",
            "/// Stall-attribution detector.\npub fn ok() {}\n",
        );
        assert_eq!(lint(tmp.path()).len(), 3);
    }

    /// Bare `watchdog` is the engine's own EPOLLHUP surface-share reaper, and a
    /// `GIL-holding thread` is ordinary description. Neither is the ban.
    #[test]
    fn allows_a_watchdog_that_is_not_a_gil_diagnostic() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "runtime/foo/src/lib.rs",
            "/// EPOLLHUP watchdog: releases surfaces after a subprocess disconnect.\n\
             /// Safe for any GIL-holding thread reading the handle.\npub fn ok() {}\n",
        );
        assert!(lint(tmp.path()).is_empty());
    }

    #[test]
    fn flags_both_placements_viable_but_not_the_bare_phrase() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "docs/architecture/foo.md",
            "The measured gap makes both placements viable.\n",
        );
        write(
            tmp.path(),
            "docs/architecture/bar.md",
            "Rejected alternative: both placements, engine-chosen.\n",
        );
        let violations = lint(tmp.path());
        assert_eq!(violations.len(), 1, "got {violations:?}");
        assert_eq!(violations[0].matched, "both placements viable");
    }

    #[test]
    fn flags_in_process_placement_hosting_and_authoring() {
        let tmp = TempDir::new().unwrap();
        write(tmp.path(), "docs/architecture/a.md", "In-process placement.\n");
        write(tmp.path(), "docs/architecture/b.md", "In-process hosting.\n");
        write(tmp.path(), "docs/architecture/c.md", "In-process authoring.\n");
        assert_eq!(lint(tmp.path()).len(), 3);
    }

    /// In-process *Rust* is a different concern wearing the same two words.
    #[test]
    fn allows_in_process_rust() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "runtime/foo/src/lib.rs",
            "/// Mints an in-process `FullAccess` context for the adapter fast path.\n",
        );
        assert!(lint(tmp.path()).is_empty());
    }

    #[test]
    fn flags_the_retracted_spike_literals() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "docs/architecture/perf.md",
            "p50 0.085ms in-process vs 0.161 ms subprocess; warm 0.089ms / 0.180ms.\n",
        );
        assert_eq!(lint(tmp.path()).len(), 1);
    }

    /// The literals are ms-anchored on purpose — bare decimals collide with
    /// every tolerance constant in the tree.
    #[test]
    fn allows_bare_decimals_that_are_not_the_retracted_numbers() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "runtime/foo/src/lib.rs",
            "const PSNR_TOLERANCE: f64 = 0.085;\nconst DRIFT: f64 = 0.161;\n",
        );
        assert!(lint(tmp.path()).is_empty());
    }

    #[test]
    fn skips_a_supersession_span_that_spans_lines() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "docs/architecture/a.md",
            "> ~~the measured gap makes both placements viable, so placement is\n\
             > demoted to engine policy~~ — Superseded 2026-08-04. There is one placement.\n",
        );
        assert!(lint(tmp.path()).is_empty());
    }

    #[test]
    fn scans_the_retraction_prose_that_follows_a_closed_span() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "docs/architecture/a.md",
            "> ~~old claim~~ — Superseded: in-process hosting is banned outright.\n",
        );
        assert_eq!(lint(tmp.path()).len(), 1);
    }

    #[test]
    fn skips_two_spans_on_one_line() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "docs/architecture/a.md",
            "~~in-process hosting~~ and ~~both placements viable~~ are both retracted.\n",
        );
        assert!(lint(tmp.path()).is_empty());
    }

    /// `~~~` is a legal code fence, not half a supersession span — counting
    /// `~~` body-wide would refuse the file for an unclosed span that isn't there.
    #[test]
    fn a_tilde_code_fence_is_not_a_span_marker() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "docs/architecture/a.md",
            "~~~rust\nlet x = 1;\n~~~\nHelper-process placement is the only placement.\n",
        );
        assert!(lint(tmp.path()).is_empty());
    }

    #[test]
    fn fenced_code_is_still_scanned() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "docs/architecture/a.md",
            "```rust\n// In-process hosting is the fast path.\n```\n",
        );
        assert_eq!(lint(tmp.path()).len(), 1);
    }

    #[test]
    fn refuses_a_file_with_an_unterminated_span() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "docs/architecture/a.md",
            "~~an opened span that never closes\nin-process hosting sails through.\n",
        );
        let err = lint_workspace(tmp.path()).unwrap_err().to_string();
        assert!(err.contains("unterminated supersession span"), "got {err}");
    }

    #[test]
    fn skips_a_file_carrying_the_allow_pragma() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "docs/decisions/a.md",
            "<!-- check-no-in-process-placement:allow-file -->\nIn-process hosting is banned.\n",
        );
        let report = lint_workspace(tmp.path()).unwrap();
        assert!(report.violations.is_empty());
        assert_eq!(report.allow_filed.len(), 1);
    }

    #[test]
    fn exempts_a_prohibition_line_but_not_its_neighbours() {
        let tmp = TempDir::new().unwrap();
        let (path, text) = EXEMPT_PROHIBITION_LINES[0];
        write(
            tmp.path(),
            path,
            &format!("{text}\n  In-process hosting is the fast path.\n"),
        );
        let report = lint_workspace(tmp.path()).unwrap();
        assert_eq!(report.exempt_lines_hit, 1);
        assert_eq!(report.violations.len(), 1, "got {:?}", report.violations);
        assert_eq!(report.violations[0].line, 2);
    }

    /// Exact-text matching is the point: an edited prohibition line loses its
    /// exemption and goes back through review.
    #[test]
    fn an_edited_prohibition_line_loses_its_exemption() {
        let tmp = TempDir::new().unwrap();
        let (path, text) = EXEMPT_PROHIBITION_LINES[0];
        write(tmp.path(), path, &format!("{text} (mostly)\n"));
        assert_eq!(lint(tmp.path()).len(), 1);
    }

    #[test]
    fn skips_target_and_generated_dirs() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "runtime/foo/target/debug/build.rs",
            "//! In-process hosting.\n",
        );
        write(
            tmp.path(),
            "runtime/foo/_generated_/mod.rs",
            "//! In-process hosting.\n",
        );
        assert!(lint(tmp.path()).is_empty());
    }

    #[test]
    fn refuses_a_run_that_read_no_source() {
        let tmp = TempDir::new().unwrap();
        let report = lint_workspace(tmp.path()).unwrap();
        let err = crate::ensure_source_walking_gate_read_source(
            "check-no-in-process-placement",
            "the scan roots",
            report.files_scanned,
            "in-process placement vocabulary re-enter the tree",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("scanned 0 files"), "got {err}");
    }

    #[test]
    fn refuses_a_resurrected_spike_tree() {
        let tmp = TempDir::new().unwrap();
        write(tmp.path(), "spikes/streamlib-pyembed-spike/README.md", "\n");
        let err = ensure_forbidden_tree_absent(tmp.path())
            .unwrap_err()
            .to_string();
        assert!(err.contains("retracted"), "got {err}");
    }

    // --- assertions against the real tree ---

    fn workspace() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .to_path_buf()
    }

    #[test]
    fn the_real_workspace_has_no_banned_placement_vocabulary() {
        let report = lint_workspace(&workspace()).unwrap();
        assert!(
            report.violations.is_empty(),
            "got {:?}",
            report.violations
        );
    }

    #[test]
    fn the_allow_file_set_is_exactly_the_expected_documents() {
        let root = workspace();
        let report = lint_workspace(&root).unwrap();
        ensure_allow_file_set_is_pinned(&root, &report).unwrap();
        assert_eq!(report.allow_filed.len(), EXPECTED_ALLOW_FILE_PATHS.len());
    }

    #[test]
    fn a_fifth_allow_file_pragma_fails_the_gate() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "runtime/foo/src/lib.rs",
            "// check-no-in-process-placement:allow-file\n//! In-process hosting is the fast path.\n",
        );
        let report = lint_workspace(tmp.path()).unwrap();
        let err = ensure_allow_file_set_is_pinned(tmp.path(), &report)
            .unwrap_err()
            .to_string();
        assert!(err.contains("runtime/foo/src/lib.rs"), "got {err}");
    }

    /// The list only shrinks: a stale entry means the prohibition it exempted is
    /// gone from the document, so the entry goes too.
    #[test]
    fn exempt_prohibition_lines_are_all_live() {
        let root = workspace();
        for (rel, text) in EXEMPT_PROHIBITION_LINES {
            let body = fs::read_to_string(root.join(rel))
                .unwrap_or_else(|e| panic!("{rel}: {e}"));
            assert!(
                body.lines().any(|line| line.trim() == *text),
                "{rel} no longer contains the exempted prohibition line `{text}` — delete the \
                 stale EXEMPT_PROHIBITION_LINES entry"
            );
        }
    }

    #[test]
    fn every_exempt_prohibition_line_is_under_the_plan_directory() {
        for (rel, _) in EXEMPT_PROHIBITION_LINES {
            assert!(
                rel.starts_with("docs/plan/"),
                "{rel} is editable by a source session — give it the allow-file pragma instead"
            );
        }
    }
}
