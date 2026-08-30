// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Bans wall-clock reads outside the four observability surfaces the plan
//! permits them on (`docs/plan/ARCHITECTURE.md` §Media I/O
//! `[one-monotonic-clock]`; rationale in `docs/decisions/one-monotonic-clock.md`).
//!
//! Monotonic is the only legal clock on the data plane. A wall-clock value and a
//! media timestamp share a unit and are different quantities, so a subtraction
//! across them is always a bug — and it is an easy bug to write, because
//! `SystemTime::now()` is the reflexive spelling for "what time is it". The four
//! surfaces that keep wall clock correlate StreamLib with the outside world and
//! with other hosts' logs, a job monotonic time cannot do.
//!
//! There is no per-line pragma and no opt-out attribute. The file allowlist is
//! the only way past this gate, every entry names one of exactly four
//! [`ObservabilitySurface`] variants, and a fifth surface is a plan change — so
//! widening the list means adding a variant, which no one does by accident.
//!
//! Cheap substring scan, no `syn` and no compile. Whole-line `//` and `#`
//! comments and Python triple-quoted spans are blanked first, so a doc comment
//! or module docstring naming a banned API is not a violation. A *trailing*
//! comment naming one still is: put such a note on its own line.
//!
//! Being a substring scan, it reads one line at a time and takes each banned
//! spelling literally. A call split across lines at the `::`, or a wall clock
//! renamed at the import (`use std::time::SystemTime as Elsewhere`), walks past
//! it. That is the accepted floor: this gate exists to stop the reflexive
//! `SystemTime::now()` and the one-token slip from the engine's own
//! `clock_gettime(CLOCK_MONOTONIC)`, not to beat someone working around it.
//!
//! Discovery is `git ls-files`, not a filesystem walk. The scan roots hold
//! virtualenvs and build trees carrying tens of thousands of third-party
//! sources that are not ours to gate.

use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

/// Workspace trees whose clock usage this gate owns.
///
/// `packages/test-fixtures` is in because it is engine-side test infrastructure
/// compiled into the engine's own runs, not a consumer. The rest of `packages/`
/// and all of `examples/` are downstream consumers, dispositioned by
/// `docs/plan/ARCHITECTURE.md` §Consumers and never gated here. A root that
/// contributes no files fails [`ensure_every_arm_read_source`], so a tree
/// holding nothing this gate reads cannot be listed either.
const SCAN_ROOTS: &[&str] = &[
    "runtime",
    "sdk",
    "adapters",
    "xtask",
    "packages/test-fixtures",
];

/// Files whose *source text* spells a banned pattern without reading a clock —
/// this gate's own constants and fixtures. Not allowlist entries: the
/// permitted-surface list stays exactly the four the plan names, and nothing
/// here is licensed to read a wall clock.
const SCAN_EXEMPT_FILES: &[&str] = &["xtask/src/check_clock_usage.rs"];

/// The four surfaces the plan permits a wall-clock read on. Adding a fifth is a
/// plan change, so it is a variant here before it is a line in the allowlist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObservabilitySurface {
    LogRecordHostTimestamp,
    LogRecordSourceTimestamp,
    LogFileName,
    ControlPlaneEventTimestamp,
}

impl ObservabilitySurface {
    pub const ALL: &'static [ObservabilitySurface] = &[
        ObservabilitySurface::LogRecordHostTimestamp,
        ObservabilitySurface::LogRecordSourceTimestamp,
        ObservabilitySurface::LogFileName,
        ObservabilitySurface::ControlPlaneEventTimestamp,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            ObservabilitySurface::LogRecordHostTimestamp => "log record `host_ts`",
            ObservabilitySurface::LogRecordSourceTimestamp => "log record `source_ts`",
            ObservabilitySurface::LogFileName => "log file naming",
            ObservabilitySurface::ControlPlaneEventTimestamp => {
                "control-plane pubsub event `timestamp_ns`"
            }
        }
    }
}

pub struct PermittedWallClockSurface {
    pub path: &'static str,
    pub surface: ObservabilitySurface,
    pub reason: &'static str,
}

/// Every file permitted to read a wall clock, and why. Correlating with the
/// outside world is the whole justification; nothing on the data plane qualifies.
const PERMITTED_WALL_CLOCK_SURFACES: &[PermittedWallClockSurface] = &[
    PermittedWallClockSurface {
        path: "runtime/streamlib-engine/src/core/logging/worker.rs",
        surface: ObservabilitySurface::LogRecordHostTimestamp,
        reason: "stamps host receipt, the authoritative sort key across a merged log stream",
    },
    PermittedWallClockSurface {
        path: "runtime/streamlib-engine/src/core/compiler/compiler_ops/subprocess_escalate.rs",
        surface: ObservabilitySurface::LogRecordHostTimestamp,
        reason: "stamps host receipt for records relayed from a helper process",
    },
    PermittedWallClockSurface {
        path: "sdk/streamlib-python-wheel/python/streamlib/_helper.py",
        surface: ObservabilitySurface::LogRecordSourceTimestamp,
        reason: "stamps the record at its Python origin, before the relay hop",
    },
    PermittedWallClockSurface {
        path: "runtime/streamlib-engine/src/core/logging/init.rs",
        surface: ObservabilitySurface::LogFileName,
        reason: "mints `started_at_millis`, which humans read off the JSONL file name",
    },
    PermittedWallClockSurface {
        path: "runtime/streamlib-engine/src/core/pubsub/bus.rs",
        surface: ObservabilitySurface::ControlPlaneEventTimestamp,
        reason: "stamps control-plane events, which are correlated against outside-world clocks",
    },
];

pub struct ClockUsageLanguage {
    pub name: &'static str,
    pub extension: &'static str,
    /// Blanks comments and string prose while preserving line count, so a
    /// reported line number still points at the source. Fails on prose this arm
    /// cannot delimit, rather than blanking what it could not parse.
    pub blank_out_prose: fn(&str) -> Result<String>,
    pub banned_wall_clock_reads: &'static [&'static str],
}

const LANGUAGES: &[ClockUsageLanguage] = &[
    ClockUsageLanguage {
        name: "rust",
        extension: "rs",
        blank_out_prose: blank_out_rust_prose,
        banned_wall_clock_reads: &[
            "SystemTime::now",
            "Utc::now",
            "Local::now",
            "UNIX_EPOCH.elapsed",
            "CLOCK_REALTIME",
        ],
    },
    ClockUsageLanguage {
        name: "python",
        extension: "py",
        blank_out_prose: blank_out_python_prose,
        banned_wall_clock_reads: &[
            "time.time(",
            "time.time_ns(",
            "datetime.now(",
            "datetime.utcnow(",
            "datetime.today(",
            "CLOCK_REALTIME",
        ],
    },
];

/// The monotonic spelling to reach for in each language, named in the failure so
/// the fix does not require finding this file.
const MONOTONIC_REPLACEMENTS: &str = "`MediaClock::now()` in Rust, `monotonic_now_ns()` in Python; for a unique name \
     rather than a timestamp, `mint_machine_global_unique_name_suffix()`";

#[derive(Debug, PartialEq, Eq)]
pub struct WallClockReadViolation {
    pub path: PathBuf,
    pub line: usize,
    pub matched_pattern: &'static str,
    pub line_text: String,
}

#[derive(Debug, Default)]
pub struct ClockUsageScanReport {
    pub violations: Vec<WallClockReadViolation>,
    pub files_scanned: usize,
    pub files_scanned_per_scan_root: Vec<(&'static str, usize)>,
    pub files_scanned_per_language: Vec<(&'static str, usize)>,
}

pub fn run(workspace_root: &Path) -> Result<()> {
    let report = scan(workspace_root)?;

    crate::ensure_source_walking_gate_read_source(
        "check-clock-usage",
        &format!("{SCAN_ROOTS:?}"),
        report.files_scanned,
        "a wall-clock read onto the data plane",
    )?;
    ensure_every_arm_read_source(&report)?;
    ensure_every_permitted_surface_still_reads_a_wall_clock(workspace_root)?;

    if report.violations.is_empty() {
        println!(
            "✓ check-clock-usage: {} file(s) scanned across {:?}, no wall-clock read outside \
             the {} permitted observability surface(s)",
            report.files_scanned,
            SCAN_ROOTS,
            ObservabilitySurface::ALL.len(),
        );
        return Ok(());
    }

    eprintln!(
        "✗ check-clock-usage: {} violation(s)",
        report.violations.len()
    );
    for violation in &report.violations {
        eprintln!(
            "  {}:{}: `{}`\n      {}",
            violation.path.display(),
            violation.line,
            violation.matched_pattern,
            violation.line_text.trim(),
        );
    }
    eprintln!(
        "\nWall clock is permitted on exactly {} observability surfaces — {} — and nowhere else. \
         A wall-clock value never enters the data plane and is never compared against, subtracted \
         from, or substituted for a media timestamp: the two share a unit and are different \
         quantities. Use {}. Widening the list is a plan change \
         (`docs/plan/ARCHITECTURE.md` §Media I/O), not a judgement call.",
        ObservabilitySurface::ALL.len(),
        ObservabilitySurface::ALL
            .iter()
            .map(|surface| surface.label())
            .collect::<Vec<_>>()
            .join(", "),
        MONOTONIC_REPLACEMENTS,
    );
    anyhow::bail!(
        "check-clock-usage: {} wall-clock read(s) outside the permitted observability surfaces",
        report.violations.len()
    );
}

pub fn scan(workspace_root: &Path) -> Result<ClockUsageScanReport> {
    let tracked = tracked_files_under_scan_roots(workspace_root)?;
    scan_files(workspace_root, &tracked)
}

/// Workspace-relative paths git tracks under the scan roots.
///
/// A filesystem walk would descend `sdk/streamlib-python-wheel/.venv-pyright`
/// and every other build tree, gating third-party sources the project does not
/// own. `git ls-files` sees exactly what CI checks out.
fn tracked_files_under_scan_roots(workspace_root: &Path) -> Result<Vec<PathBuf>> {
    let output = std::process::Command::new("git")
        .args(["ls-files", "-z", "--"])
        .args(SCAN_ROOTS)
        .current_dir(workspace_root)
        .output()
        .context("failed to run `git ls-files` for check-clock-usage")?;

    anyhow::ensure!(
        output.status.success(),
        "`git ls-files` failed ({}) — check-clock-usage cannot enumerate its scan roots",
        output.status
    );

    let listing =
        String::from_utf8(output.stdout).context("`git ls-files` emitted a non-UTF-8 path")?;
    Ok(listing
        .split('\0')
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .collect())
}

pub fn scan_files(
    workspace_root: &Path,
    relative_paths: &[PathBuf],
) -> Result<ClockUsageScanReport> {
    let mut report = ClockUsageScanReport {
        files_scanned_per_scan_root: SCAN_ROOTS.iter().map(|root| (*root, 0)).collect(),
        files_scanned_per_language: LANGUAGES.iter().map(|lang| (lang.name, 0)).collect(),
        ..Default::default()
    };

    for relative_path in relative_paths {
        if SCAN_EXEMPT_FILES
            .iter()
            .any(|exempt| relative_path == Path::new(exempt))
        {
            continue;
        }
        let Some(language) = language_of(relative_path) else {
            continue;
        };

        let body = fs::read_to_string(workspace_root.join(relative_path))
            .with_context(|| format!("failed to read {}", relative_path.display()))?;

        report.files_scanned += 1;
        count_file(&mut report.files_scanned_per_language, language.name);
        if let Some(root) = scan_root_of(relative_path) {
            count_file(&mut report.files_scanned_per_scan_root, root);
        }

        if is_permitted_wall_clock_surface(relative_path) {
            continue;
        }
        let reads = wall_clock_reads(&body, language)
            .with_context(|| format!("failed to scan {}", relative_path.display()))?;
        for (line, matched_pattern, line_text) in reads {
            report.violations.push(WallClockReadViolation {
                path: relative_path.clone(),
                line,
                matched_pattern,
                line_text,
            });
        }
    }

    Ok(report)
}

fn count_file(counts: &mut [(&'static str, usize)], key: &'static str) {
    if let Some(entry) = counts.iter_mut().find(|(name, _)| *name == key) {
        entry.1 += 1;
    }
}

fn language_of(relative_path: &Path) -> Option<&'static ClockUsageLanguage> {
    let extension = relative_path.extension()?.to_str()?;
    LANGUAGES.iter().find(|lang| lang.extension == extension)
}

fn scan_root_of(relative_path: &Path) -> Option<&'static str> {
    SCAN_ROOTS
        .iter()
        .find(|root| relative_path.starts_with(root))
        .copied()
}

fn is_permitted_wall_clock_surface(relative_path: &Path) -> bool {
    PERMITTED_WALL_CLOCK_SURFACES
        .iter()
        .any(|permitted| relative_path == Path::new(permitted.path))
}

/// Every banned read in `body`, as `(1-based line, pattern, line text)`.
fn wall_clock_reads(
    body: &str,
    language: &'static ClockUsageLanguage,
) -> Result<Vec<(usize, &'static str, String)>> {
    let code = (language.blank_out_prose)(body)?;
    Ok(banned_reads_in(&code, language)
        .map(|(line, pattern, text)| (line, pattern, text.to_string()))
        .collect())
}

/// Answers the allowlist-liveness question without building a violation list.
fn contains_wall_clock_read(body: &str, language: &'static ClockUsageLanguage) -> Result<bool> {
    let code = (language.blank_out_prose)(body)?;
    Ok(banned_reads_in(&code, language).next().is_some())
}

fn banned_reads_in<'a>(
    code: &'a str,
    language: &'static ClockUsageLanguage,
) -> impl Iterator<Item = (usize, &'static str, &'a str)> {
    code.lines().enumerate().flat_map(move |(index, line)| {
        language
            .banned_wall_clock_reads
            .iter()
            .filter_map(move |pattern| {
                line.contains(pattern)
                    .then_some((index + 1, *pattern, line))
            })
    })
}

fn blank_out_rust_prose(body: &str) -> Result<String> {
    Ok(body
        .lines()
        .map(|line| {
            if line.trim_start().starts_with("//") {
                ""
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

/// Blanks `#` comment lines and every triple-quoted span, which is where the
/// wheel's own `clock.py` names the banned APIs to warn readers off them.
///
/// A span that never closes is refused rather than scanned: it would blank every
/// line after it, and this gate's only failure mode is reading nothing and
/// reporting clean.
fn blank_out_python_prose(body: &str) -> Result<String> {
    const TRIPLE_QUOTES: [&str; 2] = ["\"\"\"", "'''"];
    let mut open_quote: Option<&str> = None;
    let mut code_lines: Vec<String> = Vec::new();

    for line in body.lines() {
        if open_quote.is_none() && line.trim_start().starts_with('#') {
            code_lines.push(String::new());
            continue;
        }

        let mut code = String::new();
        let mut rest = line;
        loop {
            match open_quote {
                None => {
                    let opener = TRIPLE_QUOTES
                        .iter()
                        .filter_map(|quote| rest.find(quote).map(|at| (at, *quote)))
                        .min_by_key(|(at, _)| *at);
                    match opener {
                        Some((at, quote)) => {
                            code.push_str(&rest[..at]);
                            open_quote = Some(quote);
                            rest = &rest[at + quote.len()..];
                        }
                        None => {
                            code.push_str(rest);
                            break;
                        }
                    }
                }
                Some(quote) => match rest.find(quote) {
                    Some(at) => {
                        open_quote = None;
                        rest = &rest[at + quote.len()..];
                    }
                    None => break,
                },
            }
        }
        code_lines.push(code);
    }

    anyhow::ensure!(
        open_quote.is_none(),
        "unterminated triple-quoted span — every line after it would be hidden from \
         check-clock-usage"
    );
    Ok(code_lines.join("\n"))
}

/// A scan arm that read nothing is indistinguishable from a clean one, so the
/// Python arm losing its tree would silently stop gating the SDK.
fn ensure_every_arm_read_source(report: &ClockUsageScanReport) -> Result<()> {
    for (scan_root, files_scanned) in &report.files_scanned_per_scan_root {
        anyhow::ensure!(
            *files_scanned > 0,
            "check-clock-usage scanned 0 files under {scan_root} — that scan root moved out \
             from under the gate"
        );
    }
    for (language, files_scanned) in &report.files_scanned_per_language {
        anyhow::ensure!(
            *files_scanned > 0,
            "check-clock-usage scanned 0 {language} files — that language arm moved out from \
             under the gate"
        );
    }
    Ok(())
}

/// An allowlist entry whose file stopped reading a wall clock is a licence
/// nobody is using, sitting on a path a future change will reuse.
fn ensure_every_permitted_surface_still_reads_a_wall_clock(workspace_root: &Path) -> Result<()> {
    for permitted in PERMITTED_WALL_CLOCK_SURFACES {
        let relative_path = Path::new(permitted.path);
        let body = fs::read_to_string(workspace_root.join(relative_path)).with_context(|| {
            format!(
                "check-clock-usage permits {} for {}, but the file is unreadable — a surface \
                 that moves leaves the allowlist in the same change",
                permitted.path,
                permitted.surface.label(),
            )
        })?;

        let language = language_of(relative_path).with_context(|| {
            format!(
                "check-clock-usage permits {}, whose extension no language arm scans",
                permitted.path
            )
        })?;

        anyhow::ensure!(
            contains_wall_clock_read(&body, language)
                .with_context(|| format!("failed to scan {}", permitted.path))?,
            "check-clock-usage permits {} for {} ({}), but it reads no wall clock — drop the \
             entry so the allowlist stays exactly the permitted set",
            permitted.path,
            permitted.surface.label(),
            permitted.reason,
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// The family idiom for the workspace root in a gate's tests: free, and it
    /// needs neither cargo on PATH nor the package lock.
    fn workspace_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .to_path_buf()
    }

    /// Both language arms and every scan root get a file, so a fixture tree
    /// satisfies the same read-source contract the real run asserts.
    fn scan_fixture(files: &[(&str, &str)]) -> (TempDir, ClockUsageScanReport) {
        let tmp = TempDir::new().unwrap();
        let mut tree: Vec<(&str, &str)> = vec![
            ("runtime/streamlib-engine/src/lib.rs", "pub fn ok() {}\n"),
            (
                "sdk/streamlib-python-wheel/python/streamlib/ok.py",
                "OK = 1\n",
            ),
            (
                "adapters/streamlib-adapter-cuda/src/lib.rs",
                "pub fn ok() {}\n",
            ),
            ("xtask/src/ok.rs", "pub fn ok() {}\n"),
            (
                "packages/test-fixtures/processors/ok.rs",
                "pub fn ok() {}\n",
            ),
        ];
        tree.extend_from_slice(files);

        let mut relative_paths = Vec::new();
        for (relative_path, body) in &tree {
            let path = tmp.path().join(relative_path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, body).unwrap();
            relative_paths.push(PathBuf::from(relative_path));
        }

        let report = scan_files(tmp.path(), &relative_paths).unwrap();
        (tmp, report)
    }

    #[test]
    fn flags_a_wall_clock_read_in_a_data_plane_file() {
        let (_tmp, report) = scan_fixture(&[(
            "runtime/streamlib-engine/src/iceoryx2/output.rs",
            "fn stamp() -> u64 {\n    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos() as u64\n}\n",
        )]);
        assert_eq!(report.violations.len(), 1, "got {:?}", report.violations);
        assert_eq!(report.violations[0].line, 2);
        assert_eq!(report.violations[0].matched_pattern, "SystemTime::now");
    }

    /// The engine's own canonical clock read is
    /// `libc::clock_gettime(libc::CLOCK_MONOTONIC, ..)`; flipping one token
    /// yields a wall-clock read, and the gate's failure message points readers
    /// straight at that file.
    #[test]
    fn flags_the_raw_syscall_a_session_gets_by_copying_media_clock() {
        let (_tmp, report) = scan_fixture(&[(
            "runtime/streamlib-engine/src/iceoryx2/output.rs",
            "unsafe { libc::clock_gettime(libc::CLOCK_REALTIME, &mut timespec) };\n",
        )]);
        assert_eq!(report.violations.len(), 1, "got {:?}", report.violations);
        assert_eq!(report.violations[0].matched_pattern, "CLOCK_REALTIME");
    }

    #[test]
    fn flags_the_python_realtime_clock_id() {
        let (_tmp, report) = scan_fixture(&[(
            "sdk/streamlib-python-wheel/python/streamlib/stamp.py",
            "stamp = time.clock_gettime_ns(time.CLOCK_REALTIME)\n",
        )]);
        assert_eq!(report.violations.len(), 1, "got {:?}", report.violations);
    }

    #[test]
    fn flags_reading_the_wall_clock_through_the_epoch() {
        let (_tmp, report) = scan_fixture(&[(
            "runtime/streamlib-engine/src/iceoryx2/output.rs",
            "let since_epoch = std::time::UNIX_EPOCH.elapsed().unwrap();\n",
        )]);
        assert_eq!(report.violations.len(), 1, "got {:?}", report.violations);
    }

    #[test]
    fn flags_the_chrono_spellings() {
        let (_tmp, report) = scan_fixture(&[(
            "runtime/streamlib-engine/src/core/runtime/node.rs",
            "let a = chrono::Utc::now();\nlet b = Local::now();\n",
        )]);
        assert_eq!(report.violations.len(), 2, "got {:?}", report.violations);
    }

    #[test]
    fn flags_a_python_wall_clock_read() {
        let (_tmp, report) = scan_fixture(&[(
            "sdk/streamlib-python-wheel/python/streamlib/stamp.py",
            "import time\n\n\ndef stamp() -> int:\n    return time.time_ns()\n",
        )]);
        assert_eq!(report.violations.len(), 1, "got {:?}", report.violations);
        assert_eq!(report.violations[0].line, 5);
        assert_eq!(report.violations[0].matched_pattern, "time.time_ns(");
    }

    #[test]
    fn accepts_every_permitted_observability_surface() {
        for permitted in PERMITTED_WALL_CLOCK_SURFACES {
            let read = match language_of(Path::new(permitted.path)).unwrap().name {
                "python" => "stamp = datetime.now(timezone.utc)\n",
                _ => "let stamp = SystemTime::now();\n",
            };
            let (_tmp, report) = scan_fixture(&[(permitted.path, read)]);
            assert!(
                report.violations.is_empty(),
                "{} is permitted for {}: {:?}",
                permitted.path,
                permitted.surface.label(),
                report.violations,
            );
        }
    }

    #[test]
    fn skips_a_rust_doc_comment_naming_the_banned_call() {
        let (_tmp, report) = scan_fixture(&[(
            "runtime/streamlib-engine/src/core/media_clock.rs",
            "//! Never `SystemTime::now()` — that is the wall clock.\n\
             /// Superseded by `MediaClock::now()`, not `Utc::now()`.\n\
             pub fn ok() {}\n",
        )]);
        assert!(report.violations.is_empty(), "got {:?}", report.violations);
    }

    #[test]
    fn skips_a_python_module_docstring_naming_the_banned_apis() {
        let (_tmp, report) = scan_fixture(&[(
            "sdk/streamlib-python-wheel/python/streamlib/clock.py",
            "\"\"\"Canonical monotonic-clock timestamp source.\n\n\
             Wall-clock APIs (`time.time`, `datetime.now`, `time.time_ns`) are NOT\n\
             comparable across processes.\n\"\"\"\n\n\
             from ._engine import monotonic_now_ns as monotonic_now_ns\n",
        )]);
        assert!(
            report.violations.is_empty(),
            "the wheel's own clock.py warns readers off these APIs by naming them: {:?}",
            report.violations,
        );
    }

    #[test]
    fn skips_a_python_comment_and_a_single_quoted_docstring() {
        let (_tmp, report) = scan_fixture(&[(
            "sdk/streamlib-python-wheel/tests/test_clock.py",
            "# time.time() is banned here\n'''Also banned: datetime.now()'''\nOK = 1\n",
        )]);
        assert!(report.violations.is_empty(), "got {:?}", report.violations);
    }

    #[test]
    fn flags_code_that_follows_a_closed_docstring_on_one_line() {
        let (_tmp, report) = scan_fixture(&[(
            "sdk/streamlib-python-wheel/python/streamlib/stamp.py",
            "\"\"\"doc\"\"\"\nstamp = time.time()\n",
        )]);
        assert_eq!(report.violations.len(), 1, "got {:?}", report.violations);
        assert_eq!(report.violations[0].line, 2);
    }

    #[test]
    fn refuses_a_python_file_whose_triple_quoted_span_never_closes() {
        let unterminated =
            blank_out_python_prose("\"\"\"doc that never closes\nstamp = time.time()\n");
        let err = unterminated.unwrap_err();
        assert!(err.to_string().contains("unterminated"), "got {err}");
    }

    #[test]
    fn an_unterminated_span_fails_the_scan_rather_than_hiding_the_file() {
        let tmp = TempDir::new().unwrap();
        let relative_path = "sdk/streamlib-python-wheel/python/streamlib/broken.py";
        let path = tmp.path().join(relative_path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "'''never closed\nstamp = time.time_ns()\n").unwrap();

        let err = scan_files(tmp.path(), &[PathBuf::from(relative_path)]).unwrap_err();
        assert!(err.to_string().contains("broken.py"), "got {err:#}");
    }

    #[test]
    fn accepts_the_monotonic_spellings() {
        let (_tmp, report) = scan_fixture(&[
            (
                "runtime/streamlib-engine/src/iceoryx2/output.rs",
                "let stamp = MediaClock::now().as_nanos() as u64;\nlet t = Instant::now();\n",
            ),
            (
                "sdk/streamlib-python-wheel/python/streamlib/stamp.py",
                "stamp = monotonic_now_ns()\nalso = time.clock_gettime_ns(time.CLOCK_MONOTONIC)\n",
            ),
        ]);
        assert!(report.violations.is_empty(), "got {:?}", report.violations);
    }

    /// The gate reads whole-line comments only, so a note sharing a line with
    /// code reads as code. Stated as a test so the limit is discoverable.
    #[test]
    fn flags_a_trailing_comment_naming_a_banned_call() {
        let (_tmp, report) = scan_fixture(&[(
            "runtime/streamlib-engine/src/iceoryx2/output.rs",
            "pub fn ok() {} // never SystemTime::now()\n",
        )]);
        assert_eq!(report.violations.len(), 1, "got {:?}", report.violations);
    }

    #[test]
    fn never_scans_its_own_source() {
        let (_tmp, report) =
            scan_fixture(&[(SCAN_EXEMPT_FILES[0], "let a = SystemTime::now();\n")]);
        assert!(report.violations.is_empty(), "got {:?}", report.violations);
    }

    #[test]
    fn every_permitted_entry_names_one_of_the_four_surfaces() {
        for permitted in PERMITTED_WALL_CLOCK_SURFACES {
            assert!(
                ObservabilitySurface::ALL.contains(&permitted.surface),
                "{} names a surface outside the permitted four",
                permitted.path,
            );
        }
        for surface in ObservabilitySurface::ALL {
            assert!(
                PERMITTED_WALL_CLOCK_SURFACES
                    .iter()
                    .any(|permitted| permitted.surface == *surface),
                "{} has no file — a permitted surface with no reader is not a surface",
                surface.label(),
            );
        }
    }

    #[test]
    fn refuses_a_tree_where_a_scan_root_read_nothing() {
        let report = ClockUsageScanReport {
            files_scanned_per_scan_root: vec![("runtime", 3), ("sdk", 0)],
            files_scanned_per_language: vec![("rust", 3), ("python", 1)],
            ..Default::default()
        };
        let err = ensure_every_arm_read_source(&report).unwrap_err();
        assert!(err.to_string().contains("sdk"), "got {err}");
    }

    #[test]
    fn refuses_a_tree_where_the_python_arm_read_nothing() {
        let report = ClockUsageScanReport {
            files_scanned_per_scan_root: vec![("runtime", 3)],
            files_scanned_per_language: vec![("rust", 3), ("python", 0)],
            ..Default::default()
        };
        let err = ensure_every_arm_read_source(&report).unwrap_err();
        assert!(err.to_string().contains("python"), "got {err}");
    }

    #[test]
    fn refuses_an_allowlist_entry_that_reads_no_wall_clock() {
        let tmp = TempDir::new().unwrap();
        for permitted in PERMITTED_WALL_CLOCK_SURFACES {
            let path = tmp.path().join(permitted.path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, "nothing here reads a clock\n").unwrap();
        }
        let err = ensure_every_permitted_surface_still_reads_a_wall_clock(tmp.path()).unwrap_err();
        assert!(err.to_string().contains("reads no wall clock"), "got {err}");
    }

    #[test]
    fn the_real_tree_permits_only_files_that_still_read_a_wall_clock() {
        ensure_every_permitted_surface_still_reads_a_wall_clock(&workspace_root()).unwrap();
    }

    #[test]
    fn discovery_skips_virtualenv_and_build_trees() {
        let tracked = tracked_files_under_scan_roots(&workspace_root()).unwrap();

        assert!(
            tracked
                .iter()
                .any(|path| path
                    == Path::new("sdk/streamlib-python-wheel/python/streamlib/clock.py")),
            "the wheel's Python package is inside the scan roots",
        );
        for path in &tracked {
            let text = path.to_string_lossy();
            assert!(
                !text.contains(".venv") && !text.contains("site-packages"),
                "{text} is third-party source this gate does not own",
            );
        }
    }
}
