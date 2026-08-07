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
//!   prohibition itself, in the decision records under
//!   [`EXEMPT_PROHIBITION_LINE_ROOTS`]. `ARCHITECTURE.md`, `GLOSSARY.md` and the
//!   2026-08-02 pivot ADR are the documents the ruling names as the drift
//!   vector, so exempting them whole would blind the lint at the worst possible
//!   place. Per line, matched on exact text: a new banned line in
//!   `ARCHITECTURE.md` still fails, and editing an exempted one forces
//!   re-review.
//!
//! Markdown supersession spans (`~~…~~`) are skipped, because the docs policy
//! requires a retraction to quote what it retracts.
//!
//! Honest limits, so nobody reads a green run as more than it is:
//!
//! - This is vocabulary, not behaviour. A watchdog renamed to avoid these
//!   patterns passes. The behavioural proof that the parent never hosts a
//!   processor class is `sdk/streamlib-python-wheel/tests/test_helper_placement.py`.
//! - **The pattern set is a subset of the rule's STOP-WORK vocabulary, not the
//!   whole of it**, and the list below is not exhaustive either. Probed misses:
//!   the runtime described as "one process" or "one big process" (the same two
//!   words state the correct model — one process *per* processor), registering
//!   the class "in the parent process", a per-callback duration monitor naming
//!   another processor, a per-processor "placement decision" or "placement
//!   choice", and any paraphrase of the retracted numbers — "roughly half the
//!   subprocess latency", "0.6% of a frame budget" — which no literal list
//!   catches. A green run means these patterns are absent, never that the rule
//!   is satisfied; the reviewers are the coverage for the rest.
//! - **A banned phrase split by a line wrap is caught; a paired-term rule whose
//!   terms land on different lines is not.** [`wrapped_banned_phrase`] explains
//!   why the second half is deliberate rather than missing.
//! - The contention shape is anchored to the word `GIL`, so a non-adjacent
//!   phrasing — "reduces contention between processors", "the lock is contended
//!   by both" — does not reach it. Bare `contention` flagged a processor's own
//!   threads and bare `contended` flagged the surface adapter's lock prose;
//!   anchoring is what keeps the shape from crying wolf.
//! - `.claude/` and `.github/` are not scanned. The rules and reviewer
//!   definitions quote the banned shapes to forbid them and change through their
//!   own dedicated PRs; `.github/` is CI configuration, and this gate's own
//!   workflow is named after what it bans.
//! - `CHANGELOG.md` is not scanned at any depth: it is release-please-generated
//!   history of what shipped, not a claim about what the runtime is. `README.md`
//!   and `CLAUDE.md` are.

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

/// Release-please-generated history of what shipped, including the release that
/// shipped the model this gate now bans. History is not a claim about what the
/// runtime is, and it is not ours to rewrite. Skipped at every depth — the
/// per-crate changelogs a future release config emits are the same artifact.
const SKIP_FILE_NAMES: &[&str] = &["CHANGELOG.md"];

/// `mmd` is here because `ARCHITECTURE.md` declares the plan to be the document
/// *plus* its diagrams, moving together — half the plan being unscanned would
/// leave the ruling's named drift vector half-covered.
const SCAN_EXTENSIONS: &[&str] = &["rs", "md", "mmd", "py", "pyi", "ts", "toml", "yaml", "yml"];

/// Marker comment that exempts an entire file. See the module doc — the set is
/// pinned by [`EXPECTED_ALLOW_FILE_PATHS`] and every member is a retraction.
const ALLOW_FILE_PRAGMA: &str = "check-no-in-process-placement:allow-file";

/// Every file allowed to carry the pragma, each because it must quote the
/// banned vocabulary at length to retract and ban it. Pinned so a fifth
/// exemption is a deliberate, reviewed edit rather than a comment someone
/// pasted to get CI green.
const EXPECTED_ALLOW_FILE_PATHS: &[&str] = &[
    "docs/decisions/helper-process-placement-only.md",
    "docs/plan/changes/archive/2026-08-07-in-process-hosting-ripout.md",
    "xtask/src/check_no_in_process_placement.rs",
];

/// The two decision-record trees a prohibition may be stated in. A per-line
/// exemption anywhere else would be a hole in source a session can edit.
const EXEMPT_PROHIBITION_LINE_ROOTS: &[&str] = &["docs/plan/", "docs/decisions/"];

/// Lines that state the prohibition itself, in the decision records that must
/// name the banned model to forbid it. Matched on exact trimmed text, so editing
/// one of these lines fails the lint and forces the edit through review, and a
/// *new* banned line in the same file still fails.
///
/// Per line rather than per file on purpose: `ARCHITECTURE.md`, `GLOSSARY.md`
/// and the 2026-08-02 pivot ADR are the documents the ruling names as the drift
/// vector, so blinding them wholesale would be the worst place to do it.
///
/// This list only shrinks. A line that leaves its document is a stale entry and
/// [`exempt_prohibition_lines_are_all_live`] fails until it is deleted here.
const EXEMPT_PROHIBITION_LINES: &[(&str, &str)] = &[
    (
        "docs/plan/ARCHITECTURE.md",
        "In-process hosting of a Python processor does not exist — not as a default, a",
    ),
    (
        "docs/plan/ARCHITECTURE.md",
        "co-tenancy remedy: no two Python processors share an interpreter.",
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
    // Wrapped halves: the phrase splits across the line break, so the line the
    // violation reports on carries only the tail of a prohibition.
    (
        "docs/plan/changes/processor-class-identity.md",
        "hosting of a Python processor is banned, so a `__main__`-defined class has no legal",
    ),
    (
        "docs/decisions/importable-python-library.md",
        "interpreter — it is the performance bar, never a placement precedent). Dogfooding",
    ),
    // "…never a co-tenancy test, since no two Python / processors share an
    // interpreter" — the denial wrapped away from what it denies.
    (
        "docs/plan/changes/importable-python-library.md",
        "processors share an interpreter. Generated `.pyi` stubs ship in the wheel (IDE",
    ),
    // The 2026-08-02 pivot ADR, whose placement clauses the ruling retracts in
    // place. Its `~~…~~` spans cover the retracted claims; these six lines are
    // the retraction prose that follows each span close.
    (
        "docs/decisions/importable-python-library.md",
        "placement; in-process hosting of a Python processor is banned. The distribution decision —",
    ),
    (
        "docs/decisions/importable-python-library.md",
        "> from the app's venv. In-process hosting of a Python processor is banned outright — not a",
    ),
    (
        "docs/decisions/importable-python-library.md",
        "> latency, is the optimised axis — the relevant ratio is 0.161ms against the 16.67ms frame",
    ),
    (
        "docs/decisions/importable-python-library.md",
        "> budget (~1%, zero drops), not against 0.085ms — and the pair itself was never validly",
    ),
    (
        "docs/decisions/importable-python-library.md",
        "> shared-GIL contention cannot appear). These numbers are never again cited for any",
    ),
    (
        "docs/decisions/importable-python-library.md",
        "the other way and killed in-process hosting instead. The surviving insight is this entry's",
    ),
    (
        "docs/decisions/importable-python-library.md",
        "- **Both placements, engine-chosen** — rejected 2026-08-04",
    ),
    (
        "docs/plan/GLOSSARY.md",
        "\"transparent move\".",
    ),
    (
        "docs/plan/GLOSSARY.md",
        "**Placement policy**, **Placement heuristic**, **Transparent move** — there is one",
    ),
];

/// A banned shape. `all_of` terms must every one appear; `any_of` and
/// `also_requires_any_of` — when non-empty — each need one hit. All matching is
/// case-insensitive over a [`normalize`]d line.
///
/// There is deliberately no negative term. A "unless the line also says
/// `helper`" clause was tried and removed: it cleared the whole shape on a
/// substring hit anywhere on the line, so `When no helper is available the
/// app's interpreter hosts the processor class` — the ADR's named `not a
/// fallback` shape — passed green. A prohibition that needs quarter is a line
/// in [`EXEMPT_PROHIBITION_LINES`], which names the file and the exact text.
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
/// engine has an EPOLLHUP watchdog, `rt.run()` documents a GIL-release contract
/// about a processor's own threads, and `both placements` appears in the very
/// sentences that retire it. `unless_any_of` carries the rule's "these are NOT
/// the ban" boundary — a helper's own interpreter, and a processor's own
/// threads, are the shipped model, not a violation of it.
const BANNED_SHAPES: &[BannedShape] = &[
    BannedShape {
        all_of: &["processor"],
        any_of: &[
            "shared interpreter",
            "one interpreter",
            "same interpreter",
            "single interpreter",
            "share an interpreter",
            "shares an interpreter",
            "sharing an interpreter",
            "app's interpreter",
            "parent's interpreter",
            "parent interpreter",
        ],
        also_requires_any_of: &[],
        guidance: CO_TENANCY_GUIDANCE,
    },
    BannedShape {
        all_of: &["processor"],
        any_of: &["share a gil", "shares a gil", "sharing a gil", "share the gil", "share one gil", "shared gil"],
        also_requires_any_of: &[],
        guidance: CO_TENANCY_GUIDANCE,
    },
    BannedShape {
        all_of: &["processor"],
        any_of: &["co-host", "cohost"],
        also_requires_any_of: &[],
        guidance: PLACEMENT_GUIDANCE,
    },
    BannedShape {
        all_of: &["in-process", "lowest latency"],
        any_of: &[],
        also_requires_any_of: &[],
        guidance: PLACEMENT_GUIDANCE,
    },
    BannedShape {
        all_of: &[],
        any_of: &[
            "gil contention",
            "gil-contention",
            "contend for the gil",
            "contending for the gil",
            "gil is contended",
            "contended gil",
        ],
        also_requires_any_of: &[],
        guidance: DIAGNOSTIC_GUIDANCE,
    },
    BannedShape {
        all_of: &["gil"],
        any_of: &["stall", "block", "starve", "degrade"],
        also_requires_any_of: &["other processor", "another processor", "other python processor"],
        guidance: CO_TENANCY_GUIDANCE,
    },
    BannedShape {
        all_of: &[],
        any_of: &["gil-hold", "gil hold", "slow-callback", "slow callback", "stall-attribution", "stall attribution"],
        also_requires_any_of: WATCHDOG_NOUNS,
        guidance: DIAGNOSTIC_GUIDANCE,
    },
    BannedShape {
        all_of: &[],
        any_of: RETIRED_PLACEMENT_TERMS,
        also_requires_any_of: &[],
        guidance: PLACEMENT_GUIDANCE,
    },
    BannedShape {
        all_of: &[],
        any_of: &[
            "in-process placement",
            "in-process hosting",
            "in-process authoring",
            "in process placement",
            "in process hosting",
            "in process authoring",
        ],
        also_requires_any_of: &[],
        guidance: PLACEMENT_GUIDANCE,
    },
    BannedShape {
        all_of: &["in-process", "python processor"],
        any_of: &[],
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

/// The glossary's retired-terms list for the 2026-08-04 ruling — there is one
/// placement, so each of these names nothing. Banned bare: `both placements
/// are first-class` is the sentence the ADR blames for the #1711 incident, and
/// the narrower `both placements viable` would have read straight past it.
const RETIRED_PLACEMENT_TERMS: &[&str] = &[
    "both placements",
    "two placements",
    "either placement",
    "placement policy",
    "placement heuristic",
    "transparent move",
];

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
    pub files_scanned_per_root: Vec<(&'static str, usize)>,
    pub allow_filed: Vec<PathBuf>,
    pub exempt_lines_hit: usize,
}

pub fn run(workspace_root: &Path) -> Result<()> {
    ensure_forbidden_tree_absent(workspace_root)?;
    let report = lint_workspace(workspace_root)?;
    ensure_exempt_prohibition_lines_are_in_decision_records()?;
    ensure_allow_file_set_is_pinned(workspace_root, &report)?;
    crate::ensure_source_walking_gate_read_source(
        "check-no-in-process-placement",
        &format!("{SCAN_PARENTS:?}"),
        report.files_scanned,
        "in-process placement vocabulary re-enter the tree",
    )?;
    ensure_every_scan_root_contributed(&report)?;

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

/// The per-line hatch must never reach source a session can edit, so the roots
/// are enforced by the gate rather than only by its tests.
fn ensure_exempt_prohibition_lines_are_in_decision_records() -> Result<()> {
    for (rel, _) in EXEMPT_PROHIBITION_LINES {
        anyhow::ensure!(
            EXEMPT_PROHIBITION_LINE_ROOTS
                .iter()
                .any(|root| rel.starts_with(root)),
            "{rel} is not a decision record ({EXEMPT_PROHIBITION_LINE_ROOTS:?}) — a per-line \
             exemption there would be a hole in source a session can edit"
        );
    }
    Ok(())
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
        let before = report.files_scanned;
        scan_dir(workspace_root, &dir, &mut report)?;
        report.files_scanned_per_root.push((parent, report.files_scanned - before));
    }
    scan_workspace_root_markdown(workspace_root, &mut report)?;
    Ok(report)
}

/// `README.md` and `CLAUDE.md` sit at the workspace root, outside every scan
/// parent. `CLAUDE.md` in particular is read as instruction by every session,
/// which is the exact surface the ruling blames for the original drift.
fn scan_workspace_root_markdown(
    workspace_root: &Path,
    report: &mut InProcessPlacementScanReport,
) -> Result<()> {
    let entries = match fs::read_dir(workspace_root) {
        Ok(entries) => entries,
        Err(_) => return Ok(()),
    };
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if !path.is_file() || path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        if is_skipped_file_name(&path) {
            continue;
        }
        scan_file(workspace_root, &path, "md", report)?;
    }
    Ok(())
}

fn is_skipped_file_name(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|name| SKIP_FILE_NAMES.contains(&name))
}

/// A total `files_scanned > 0` check is satisfied by `runtime/` alone, so losing
/// `docs/` — the root this gate deliberately added — would leave it green. Every
/// declared root must exist and must have contributed at least one file.
fn ensure_every_scan_root_contributed(report: &InProcessPlacementScanReport) -> Result<()> {
    for parent in SCAN_PARENTS {
        let scanned = report
            .files_scanned_per_root
            .iter()
            .find(|(root, _)| root == parent)
            .map(|(_, count)| *count)
            .unwrap_or(0);
        anyhow::ensure!(
            scanned > 0,
            "check-no-in-process-placement scanned 0 files under {parent}/ — the root moved out \
             from under the gate, which would let banned placement vocabulary re-enter it unnoticed"
        );
    }
    Ok(())
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
        if is_skipped_file_name(path) {
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
    let mut previous = String::new();
    for (idx, line) in body.lines().enumerate() {
        let scanned = if is_markdown {
            spans.outside_spans(line)
        } else {
            Cow::Borrowed(line)
        };
        if scanned.trim().is_empty() {
            previous.clear();
            continue;
        }
        if is_exempt_prohibition_line(&relative, line) {
            report.exempt_lines_hit += 1;
            previous.clear();
            continue;
        }
        let hit = first_banned_shape(&scanned)
            .or_else(|| wrapped_banned_phrase(&previous, &scanned));
        if let Some(hit) = hit {
            report.violations.push(LintViolation {
                file: path.to_path_buf(),
                line: idx + 1,
                matched: hit.matched.into_owned(),
                guidance: hit.guidance,
            });
        }
        previous = scanned.into_owned();
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

/// Fold the prose spellings that differ from the pattern literals only by a
/// character an editor substituted: the hyphen family (a non-breaking hyphen in
/// `in‑process` is invisible in a diff) and the curly apostrophe in
/// `app's interpreter`.
fn normalize(line: &str) -> String {
    line.chars()
        .map(|c| match c {
            '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' => '-',
            '\u{2018}' | '\u{2019}' => '\'',
            other => other.to_ascii_lowercase(),
        })
        .collect()
}

fn first_banned_shape(line: &str) -> Option<BannedShapeMatch> {
    let haystack = normalize(line);
    BANNED_SHAPES
        .iter()
        .find_map(|shape| shape.match_in(&haystack))
}

/// The wrap case: a banned *phrase* split across a line break. This repo's
/// prose hard-wraps, so `in-process` / `hosting` and `one shared` /
/// `interpreter` happen with no intent to evade.
///
/// Deliberately narrower than joining the two lines and re-running everything.
/// A whole-window scan also pairs terms that merely landed on adjacent lines,
/// and adjacent prose lines routinely share a word — that trade cost four false
/// positives on the rule's protected boundary, which is exactly what
/// `.claude/rules/placement.md` says must not happen. A multi-word term absent
/// from both lines but present across the seam is the real phrase, every time.
fn wrapped_banned_phrase(previous: &str, current: &str) -> Option<BannedShapeMatch> {
    if previous.is_empty() {
        return None;
    }
    let before = normalize(previous);
    let after = normalize(current);
    let seam = format!("{before} {after}");
    BANNED_SHAPES.iter().find_map(|shape| {
        let splits_the_seam = shape
            .all_of
            .iter()
            .chain(shape.any_of.iter())
            .filter(|term| term.contains(' '))
            .any(|term| seam.contains(term) && !before.contains(term) && !after.contains(term));
        if !splits_the_seam {
            return None;
        }
        shape.match_in(&seam)
    })
}

impl BannedShape {
    /// `haystack` must already be [`normalize`]d.
    fn match_in(&self, haystack: &str) -> Option<BannedShapeMatch> {
        if !self.all_of.iter().all(|term| haystack.contains(term)) {
            return None;
        }
        if !self.also_requires_any_of.is_empty()
            && !self
                .also_requires_any_of
                .iter()
                .any(|term| haystack.contains(term))
        {
            return None;
        }
        let matched = if self.any_of.is_empty() {
            Cow::Owned(self.all_of.join(" + "))
        } else {
            Cow::Borrowed(self.any_of.iter().copied().find(|t| haystack.contains(t))?)
        };
        Some(BannedShapeMatch {
            matched,
            guidance: self.guidance,
        })
    }
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

    /// The sentence the ADR blames for the #1711 incident, which the narrower
    /// `both placements viable` pattern would have read straight past.
    #[test]
    fn flags_hosting_a_processor_in_the_apps_interpreter() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "docs/architecture/a.md",
            "Hosting a Python processor in the app's interpreter is the fast path.\n",
        );
        assert_eq!(lint(tmp.path()).len(), 1);
    }

    #[test]
    fn flags_the_interpreter_sharing_synonyms() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "docs/architecture/a.md",
            "All Python processors execute in a single interpreter.\n",
        );
        write(
            tmp.path(),
            "docs/architecture/b.md",
            "Processors share an interpreter with the app.\n",
        );
        write(
            tmp.path(),
            "docs/architecture/c.md",
            "The processor class runs in the parent interpreter.\n",
        );
        assert_eq!(lint(tmp.path()).len(), 3);
    }

    /// Each processor having *its own* interpreter is the shipped model, and it
    /// matches no pattern — the co-tenancy shape names sharing, not ownership.
    /// No negative term is needed, and none exists.
    #[test]
    fn allows_a_processor_owning_its_interpreter() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "docs/architecture/a.md",
            "Every Python processor gets its own interpreter in its own child process.\n",
        );
        assert!(lint(tmp.path()).is_empty(), "{:?}", lint(tmp.path()));
    }

    /// Naming the helper, the child, or a processor's own anything must not buy
    /// a sentence clemency. An `unless_any_of` boundary let all seven of these
    /// through — including the ADR's named "not a fallback" shape — for the sake
    /// of one plan line that `EXEMPT_PROHIBITION_LINES` now carries by name.
    #[test]
    fn boundary_words_do_not_excuse_a_co_tenancy_claim() {
        let sentences = [
            "When no helper is available the app's interpreter hosts the processor class.",
            "The helper pool runs several Python processors in one interpreter.",
            "Two Python processors share an interpreter inside the child process.",
            "The processor class is imported into the parent interpreter whenever the helper spawn fails.",
            "A processor may run in the same interpreter as the app if it declares its own low-latency mode.",
            "No two processors share an interpreter unless the helper opts them in.",
            "Several processors share one interpreter and their own GIL is shared between them.",
        ];
        for (index, sentence) in sentences.iter().enumerate() {
            let tmp = TempDir::new().unwrap();
            write(tmp.path(), "docs/architecture/a.md", &format!("{sentence}\n"));
            assert_eq!(lint(tmp.path()).len(), 1, "sentence {index} passed: {sentence}");
        }
    }

    #[test]
    fn flags_a_gil_block_attributed_to_other_processors() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "docs/architecture/a.md",
            "A callback holding the GIL blocks all other Python processors.\n",
        );
        assert_eq!(lint(tmp.path()).len(), 1);
    }

    /// The GIL-release contract names the processor's own threads. Requiring
    /// `gil` + `stall` + `processor` alone flagged it; the ban is a stall
    /// attributed to *another* processor.
    #[test]
    fn allows_a_stall_of_the_processors_own_threads() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "sdk/foo/src/a.rs",
            "//! Holding the GIL would stall the processor's own worker threads.\n",
        );
        assert!(lint(tmp.path()).is_empty());
    }

    /// `gil` + `contention` anywhere on a line flagged legitimate prose about
    /// one processor's own threads; the diagnostic is named by adjacency.
    #[test]
    fn allows_contention_that_is_not_gil_contention() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "sdk/foo/src/a.rs",
            "//! Release the GIL in native calls to reduce contention among a processor's own threads.\n",
        );
        assert!(lint(tmp.path()).is_empty());
    }

    #[test]
    fn flags_in_process_beside_a_python_processor() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "docs/architecture/a.md",
            "Python processors run in-process when latency matters.\n",
        );
        assert_eq!(lint(tmp.path()).len(), 1);
    }

    #[test]
    fn flags_unhyphenated_in_process_hosting() {
        let tmp = TempDir::new().unwrap();
        write(tmp.path(), "docs/architecture/a.md", "In process hosting is back.\n");
        assert_eq!(lint(tmp.path()).len(), 1);
    }

    /// The plan is the document plus its diagrams, moving together.
    #[test]
    fn scans_mermaid_plan_diagrams() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "docs/plan/diagrams/system.mmd",
            "  A[App] --> B[In-process hosting]\n",
        );
        assert_eq!(lint(tmp.path()).len(), 1);
    }

    #[test]
    fn skips_a_per_crate_changelog_too() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "sdk/streamlib-python-wheel/CHANGELOG.md",
            "* **python:** in-process authoring\n",
        );
        assert!(lint(tmp.path()).is_empty());
    }

    /// This repo's prose hard-wraps; a banned phrase splitting across the break
    /// is the phrase, not a coincidence.
    #[test]
    fn flags_a_banned_phrase_split_by_a_line_wrap() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "docs/architecture/a.md",
            "A change that reintroduces in-process\nhosting is a regression.\n",
        );
        write(
            tmp.path(),
            "docs/architecture/b.md",
            "Holoscan acquires the GIL in one shared\ninterpreter for every processor.\n",
        );
        let violations = lint(tmp.path());
        assert_eq!(violations.len(), 2, "got {violations:?}");
        assert!(violations.iter().all(|v| v.line == 2), "got {violations:?}");
    }

    /// The reason the wrap check is phrase-only: adjacent prose lines routinely
    /// share a word, and pairing across the break flagged the rule's protected
    /// boundary — here, a true sentence about what the app's interpreter never
    /// loaded.
    #[test]
    fn does_not_pair_terms_that_merely_landed_on_adjacent_lines() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "sdk/foo/tests/a.py",
            "# Only a parent can see that the app's interpreter never loaded a second copy\n             # of the processor's module.\n",
        );
        assert!(lint(tmp.path()).is_empty(), "{:?}", lint(tmp.path()));
    }

    /// An editor's non-breaking hyphen and curly apostrophe are invisible in a
    /// diff and would otherwise walk straight past the pattern literals.
    #[test]
    fn flags_prose_spelled_with_unicode_punctuation() {
        let tmp = TempDir::new().unwrap();
        write(tmp.path(), "docs/architecture/a.md", "In\u{2011}process hosting is back.\n");
        write(
            tmp.path(),
            "docs/architecture/b.md",
            "Hosting a Python processor in the app\u{2019}s interpreter is the fast path.\n",
        );
        assert_eq!(lint(tmp.path()).len(), 2);
    }

    #[test]
    fn flags_processors_sharing_a_gil() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "docs/architecture/a.md",
            "Two Python processors share a GIL, so one can block the other.\n",
        );
        assert_eq!(lint(tmp.path()).len(), 1);
    }

    #[test]
    fn flags_co_hosting_a_processor() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "docs/architecture/a.md",
            "The processor is co-hosted in the parent to shorten the escalate hop.\n",
        );
        assert_eq!(lint(tmp.path()).len(), 1);
    }

    #[test]
    fn flags_in_process_called_lowest_latency() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "docs/architecture/a.md",
            "Running in-process is the lowest latency option.\n",
        );
        assert_eq!(lint(tmp.path()).len(), 1);
    }

    #[test]
    fn flags_the_retired_placement_terms_bare() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "docs/architecture/a.md",
            "Both placements are first-class.\n",
        );
        write(
            tmp.path(),
            "docs/architecture/b.md",
            "Either placement is acceptable for a Python processor.\n",
        );
        write(
            tmp.path(),
            "docs/architecture/c.md",
            "The engine applies placement heuristics per processor.\n",
        );
        write(
            tmp.path(),
            "docs/architecture/d.md",
            "Placement policy is an engine concern.\n",
        );
        write(
            tmp.path(),
            "docs/architecture/e.md",
            "A transparent move between placements.\n",
        );
        assert_eq!(lint(tmp.path()).len(), 5);
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
    fn every_exempt_prohibition_line_is_in_a_decision_record() {
        ensure_exempt_prohibition_lines_are_in_decision_records().unwrap();
    }

    #[test]
    fn every_scan_root_contributes_files_in_the_real_tree() {
        let report = lint_workspace(&workspace()).unwrap();
        ensure_every_scan_root_contributed(&report).unwrap();
    }

    #[test]
    fn a_scan_root_that_moved_out_from_under_the_gate_fails_the_run() {
        let tmp = TempDir::new().unwrap();
        write(tmp.path(), "runtime/foo/src/lib.rs", "pub fn ok() {}\n");
        let report = lint_workspace(tmp.path()).unwrap();
        let err = ensure_every_scan_root_contributed(&report)
            .unwrap_err()
            .to_string();
        assert!(err.contains("scanned 0 files under"), "got {err}");
    }

    #[test]
    fn scans_workspace_root_markdown_but_not_the_changelog() {
        let tmp = TempDir::new().unwrap();
        write(tmp.path(), "CLAUDE.md", "In-process hosting is the fast path.\n");
        write(tmp.path(), "README.md", "In-process hosting is supported.\n");
        write(tmp.path(), "CHANGELOG.md", "* **python:** in-process authoring\n");
        let violations = lint(tmp.path());
        assert_eq!(violations.len(), 2, "got {violations:?}");
        assert!(
            violations
                .iter()
                .all(|v| !v.file.ends_with("CHANGELOG.md")),
            "got {violations:?}"
        );
    }
}
