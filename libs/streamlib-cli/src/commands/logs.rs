// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `streamlib logs` — read and pretty-print a runtime's on-disk JSONL log
//! file. Reuses the runtime's stdout-mirror formatter so replayed output
//! matches the live tail byte-for-byte.

use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use streamlib::logging::{format_event_pretty, log_dir, LogLevel, RuntimeLogEvent, Source};

/// Arguments for the `logs` subcommand.
pub struct LogsArgs<'a> {
    pub runtime_id: Option<&'a str>,
    pub list: bool,
    pub follow: bool,
    pub processor: Option<&'a str>,
    pub pipeline: Option<&'a str>,
    pub rhi: bool,
    pub level: Option<&'a str>,
    pub source: Option<&'a str>,
    pub intercepted_only: bool,
    pub since: Option<&'a str>,
}

/// Entry point for `Commands::Logs`. Resolves the log directory and
/// dispatches to either `--list` enumeration or runtime streaming.
pub async fn run(args: LogsArgs<'_>) -> Result<()> {
    let dir = log_dir();
    let mut stdout = std::io::stdout();
    let mut stderr = std::io::stderr();
    run_into(args, &dir, &mut stdout, &mut stderr).await
}

/// Library-style entry that writes into caller-supplied sinks. Tests use
/// this to capture output without touching real stdout.
pub async fn run_into(
    args: LogsArgs<'_>,
    log_dir: &Path,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<()> {
    if args.list {
        return list_runtimes(log_dir, out);
    }

    let runtime_id = args.runtime_id.context(
        "missing runtime_id (positional). Use `streamlib logs --list` to enumerate available runtimes.",
    )?;

    if args.since.is_some() {
        let _ = writeln!(
            err,
            "note: --since is only supported by the orchestrator query path (not yet wired); ignored in offline mode."
        );
    }

    let filters = Filters::from_args(&args)?;
    stream_runtime(log_dir, runtime_id, args.follow, &filters, out, err).await
}

// ─── Filtering ───────────────────────────────────────────────────────────

struct Filters {
    processor: Option<String>,
    pipeline: Option<String>,
    rhi: bool,
    min_level: Option<LogLevel>,
    source: Option<Source>,
    intercepted_only: bool,
}

impl Filters {
    fn from_args(args: &LogsArgs<'_>) -> Result<Self> {
        Ok(Self {
            processor: args.processor.map(str::to_string),
            pipeline: args.pipeline.map(str::to_string),
            rhi: args.rhi,
            min_level: args.level.map(parse_level).transpose()?,
            source: args.source.map(parse_source).transpose()?,
            intercepted_only: args.intercepted_only,
        })
    }

    fn matches(&self, event: &RuntimeLogEvent) -> bool {
        if let Some(p) = &self.processor {
            if event.processor_id.as_deref() != Some(p.as_str()) {
                return false;
            }
        }
        if let Some(p) = &self.pipeline {
            if event.pipeline_id.as_deref() != Some(p.as_str()) {
                return false;
            }
        }
        if self.rhi && event.rhi_op.is_none() {
            return false;
        }
        if let Some(min) = self.min_level {
            if level_rank(event.level) < level_rank(min) {
                return false;
            }
        }
        if let Some(s) = self.source {
            if event.source != s {
                return false;
            }
        }
        if self.intercepted_only && !event.intercepted {
            return false;
        }
        true
    }
}

fn parse_level(s: &str) -> Result<LogLevel> {
    match s {
        "trace" => Ok(LogLevel::Trace),
        "debug" => Ok(LogLevel::Debug),
        "info" => Ok(LogLevel::Info),
        "warn" => Ok(LogLevel::Warn),
        "error" => Ok(LogLevel::Error),
        other => bail!("invalid --level '{}': expected trace|debug|info|warn|error", other),
    }
}

fn parse_source(s: &str) -> Result<Source> {
    match s {
        "rust" => Ok(Source::Rust),
        "python" => Ok(Source::Python),
        "deno" => Ok(Source::Deno),
        other => bail!("invalid --source '{}': expected rust|python|deno", other),
    }
}

fn level_rank(level: LogLevel) -> u8 {
    match level {
        LogLevel::Trace => 0,
        LogLevel::Debug => 1,
        LogLevel::Info => 2,
        LogLevel::Warn => 3,
        LogLevel::Error => 4,
    }
}

// ─── --list ──────────────────────────────────────────────────────────────

fn list_runtimes(log_dir: &Path, out: &mut dyn Write) -> Result<()> {
    if !log_dir.exists() {
        writeln!(out, "(no logs found at {})", log_dir.display())?;
        return Ok(());
    }

    let mut entries = enumerate_jsonl(log_dir)?;
    entries.sort_by_key(|e| std::cmp::Reverse(e.started_at_millis));

    if entries.is_empty() {
        writeln!(out, "(no runtime log files in {})", log_dir.display())?;
        return Ok(());
    }

    writeln!(out, "{:<24}  {:<24}  {}", "RUNTIME_ID", "STARTED_AT", "SIZE")?;
    for entry in entries {
        let started = format_millis(entry.started_at_millis);
        let size = format_size(entry.size_bytes);
        writeln!(
            out,
            "{:<24}  {:<24}  {}",
            entry.runtime_id, started, size
        )?;
    }
    Ok(())
}

struct LogFileEntry {
    runtime_id: String,
    started_at_millis: u128,
    path: PathBuf,
    size_bytes: u64,
}

fn enumerate_jsonl(log_dir: &Path) -> Result<Vec<LogFileEntry>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(log_dir)
        .with_context(|| format!("read log dir {}", log_dir.display()))?
    {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        let Some(stem) = name_str.strip_suffix(".jsonl") else {
            continue;
        };
        // `<runtime_id>-<millis>` — runtime_id may itself contain dashes,
        // so split on the LAST dash.
        let Some((rid, millis_str)) = stem.rsplit_once('-') else {
            continue;
        };
        let Ok(millis) = millis_str.parse::<u128>() else {
            continue;
        };
        let metadata = entry.metadata()?;
        out.push(LogFileEntry {
            runtime_id: rid.to_string(),
            started_at_millis: millis,
            path: entry.path(),
            size_bytes: metadata.len(),
        });
    }
    Ok(out)
}

fn format_millis(millis: u128) -> String {
    let secs = (millis / 1000) as i64;
    let nanos = ((millis % 1000) * 1_000_000) as u32;
    chrono::DateTime::<chrono::Utc>::from_timestamp(secs, nanos)
        .map(|dt| dt.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or_else(|| millis.to_string())
}

fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    if bytes >= GB {
        format!("{:.1} GiB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MiB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KiB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
}

// ─── Streaming ───────────────────────────────────────────────────────────

async fn stream_runtime(
    log_dir: &Path,
    runtime_id: &str,
    follow: bool,
    filters: &Filters,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<()> {
    let mut current = match newest_for_runtime(log_dir, runtime_id)? {
        Some(entry) => entry,
        None if follow => {
            // File may not exist yet — wait for it to appear.
            writeln!(
                err,
                "note: no log file yet for runtime '{}', waiting in --follow mode...",
                runtime_id
            )?;
            wait_for_runtime_file(log_dir, runtime_id).await?
        }
        None => {
            bail!(
                "no log file found for runtime '{}' in {}.\n\
                 Use `streamlib logs --list` to see available runtimes.",
                runtime_id,
                log_dir.display()
            );
        }
    };

    let other_count = enumerate_jsonl(log_dir)?
        .into_iter()
        .filter(|e| e.runtime_id == runtime_id && e.path != current.path)
        .count();
    if other_count > 0 {
        writeln!(
            err,
            "note: {} older log file(s) exist for runtime '{}'; reading newest only.",
            other_count, runtime_id
        )?;
    }

    let mut file = File::open(&current.path)
        .with_context(|| format!("open {}", current.path.display()))?;
    let mut reader = BufReader::new(&mut file);
    let mut pos: u64 = 0;
    let mut line_buf = String::new();
    let mut pretty_buf = String::new();

    // Drain once.
    loop {
        line_buf.clear();
        let n = reader.read_line(&mut line_buf)?;
        if n == 0 {
            break;
        }
        pos += n as u64;
        emit_line(&line_buf, filters, &mut pretty_buf, out, err)?;
    }

    if !follow {
        return Ok(());
    }

    // Tail loop: poll for appended bytes; switch to a newer file if the
    // runtime restarts (new `<runtime_id>-<higher-millis>.jsonl`).
    drop(reader);
    let mut file = File::open(&current.path)
        .with_context(|| format!("reopen {}", current.path.display()))?;
    file.seek(SeekFrom::Start(pos))?;
    let mut reader = BufReader::new(file);

    loop {
        line_buf.clear();
        match reader.read_line(&mut line_buf) {
            Ok(0) => {
                tokio::time::sleep(Duration::from_millis(100)).await;
                if let Some(newer) = newest_for_runtime(log_dir, runtime_id)? {
                    if newer.path != current.path
                        && newer.started_at_millis > current.started_at_millis
                    {
                        writeln!(
                            err,
                            "note: runtime '{}' rotated to a newer log file; switching.",
                            runtime_id
                        )?;
                        current = newer;
                        let mut new_file = File::open(&current.path)
                            .with_context(|| format!("open {}", current.path.display()))?;
                        new_file.seek(SeekFrom::Start(0))?;
                        reader = BufReader::new(new_file);
                        continue;
                    }
                }
                // Refresh BufReader's EOF cache.
                let pos_now = reader.stream_position()?;
                let inner = reader.into_inner();
                let mut refreshed = inner;
                refreshed.seek(SeekFrom::Start(pos_now))?;
                reader = BufReader::new(refreshed);
            }
            Ok(_) => {
                emit_line(&line_buf, filters, &mut pretty_buf, out, err)?;
            }
            Err(e) => return Err(e).context("reading log file"),
        }
    }
}

fn newest_for_runtime(log_dir: &Path, runtime_id: &str) -> Result<Option<LogFileEntry>> {
    if !log_dir.exists() {
        return Ok(None);
    }
    let mut matches: Vec<_> = enumerate_jsonl(log_dir)?
        .into_iter()
        .filter(|e| e.runtime_id == runtime_id)
        .collect();
    matches.sort_by_key(|e| e.started_at_millis);
    Ok(matches.pop())
}

async fn wait_for_runtime_file(log_dir: &Path, runtime_id: &str) -> Result<LogFileEntry> {
    loop {
        if let Some(entry) = newest_for_runtime(log_dir, runtime_id)? {
            return Ok(entry);
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

fn emit_line(
    line: &str,
    filters: &Filters,
    pretty_buf: &mut String,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<()> {
    let trimmed = line.trim_end_matches('\n');
    if trimmed.is_empty() {
        return Ok(());
    }
    let event: RuntimeLogEvent = match serde_json::from_str(trimmed) {
        Ok(ev) => ev,
        Err(e) => {
            writeln!(err, "warning: skipping malformed JSONL line: {}", e)?;
            return Ok(());
        }
    };
    if !filters.matches(&event) {
        return Ok(());
    }
    pretty_buf.clear();
    format_event_pretty(&event, pretty_buf);
    out.write_all(pretty_buf.as_bytes())?;
    out.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::fs;
    use std::time::Duration;
    use streamlib::logging::SCHEMA_VERSION;

    fn empty_args() -> LogsArgs<'static> {
        LogsArgs {
            runtime_id: None,
            list: false,
            follow: false,
            processor: None,
            pipeline: None,
            rhi: false,
            level: None,
            source: None,
            intercepted_only: false,
            since: None,
        }
    }

    fn make_event(
        runtime_id: &str,
        host_ts: u64,
        level: LogLevel,
        source: Source,
        message: &str,
    ) -> RuntimeLogEvent {
        RuntimeLogEvent {
            schema_version: SCHEMA_VERSION,
            host_ts,
            runtime_id: runtime_id.to_string(),
            source,
            level,
            message: message.to_string(),
            target: "test::module".to_string(),
            pipeline_id: None,
            processor_id: None,
            rhi_op: None,
            source_ts: None,
            source_seq: None,
            intercepted: false,
            channel: None,
            attrs: BTreeMap::new(),
        }
    }

    fn write_jsonl(dir: &Path, runtime_id: &str, started: u128, events: &[RuntimeLogEvent]) -> PathBuf {
        fs::create_dir_all(dir).unwrap();
        let path = dir.join(format!("{}-{}.jsonl", runtime_id, started));
        let mut f = fs::File::create(&path).unwrap();
        for ev in events {
            let line = serde_json::to_string(ev).unwrap();
            writeln!(f, "{}", line).unwrap();
        }
        path
    }

    async fn run_capture(args: LogsArgs<'_>, log_dir: &Path) -> (String, String, Result<()>) {
        let mut out = Vec::<u8>::new();
        let mut err = Vec::<u8>::new();
        let res = run_into(args, log_dir, &mut out, &mut err).await;
        (
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
            res,
        )
    }

    #[tokio::test]
    async fn reads_jsonl_and_pretty_prints() {
        let dir = tempfile::tempdir().unwrap();
        let ev = make_event("Rabc", 1_700_000_000_000_000_000, LogLevel::Info, Source::Rust, "hello");
        write_jsonl(dir.path(), "Rabc", 1_700_000_000_000, std::slice::from_ref(&ev));

        let mut args = empty_args();
        args.runtime_id = Some("Rabc");
        let (out, _err, res) = run_capture(args, dir.path()).await;
        res.unwrap();

        let mut expected = String::new();
        format_event_pretty(&ev, &mut expected);
        assert_eq!(out, expected, "pretty output must match runtime stdout-mirror byte-for-byte");
    }

    #[tokio::test]
    async fn list_enumerates_runtime_files() {
        let dir = tempfile::tempdir().unwrap();
        let ev_a = make_event("Rabc", 1, LogLevel::Info, Source::Rust, "a");
        let ev_b = make_event("Rxyz", 2, LogLevel::Info, Source::Rust, "b");
        write_jsonl(dir.path(), "Rabc", 1_700_000_000_000, std::slice::from_ref(&ev_a));
        write_jsonl(dir.path(), "Rxyz", 1_700_000_001_000, std::slice::from_ref(&ev_b));

        let mut args = empty_args();
        args.list = true;
        let (out, _err, res) = run_capture(args, dir.path()).await;
        res.unwrap();

        assert!(out.contains("Rabc"), "list output missing Rabc: {}", out);
        assert!(out.contains("Rxyz"), "list output missing Rxyz: {}", out);
        assert!(out.contains("RUNTIME_ID"), "list output missing header: {}", out);
        assert!(out.contains("STARTED_AT"), "list output missing STARTED_AT col");
        assert!(out.contains("SIZE"), "list output missing SIZE col");
    }

    #[tokio::test]
    async fn filter_by_processor() {
        let dir = tempfile::tempdir().unwrap();
        let mut ev_p1 = make_event("Rabc", 100, LogLevel::Info, Source::Rust, "from p1");
        ev_p1.processor_id = Some("p1".into());
        let mut ev_p2 = make_event("Rabc", 200, LogLevel::Info, Source::Rust, "from p2");
        ev_p2.processor_id = Some("p2".into());
        write_jsonl(dir.path(), "Rabc", 1, &[ev_p1.clone(), ev_p2.clone()]);

        let mut args = empty_args();
        args.runtime_id = Some("Rabc");
        args.processor = Some("p1");
        let (out, _err, res) = run_capture(args, dir.path()).await;
        res.unwrap();
        assert!(out.contains("from p1"));
        assert!(!out.contains("from p2"));
    }

    #[tokio::test]
    async fn filter_by_pipeline() {
        let dir = tempfile::tempdir().unwrap();
        let mut ev_a = make_event("Rabc", 100, LogLevel::Info, Source::Rust, "from pl1");
        ev_a.pipeline_id = Some("pl1".into());
        let mut ev_b = make_event("Rabc", 200, LogLevel::Info, Source::Rust, "from pl2");
        ev_b.pipeline_id = Some("pl2".into());
        write_jsonl(dir.path(), "Rabc", 1, &[ev_a, ev_b]);

        let mut args = empty_args();
        args.runtime_id = Some("Rabc");
        args.pipeline = Some("pl1");
        let (out, _err, res) = run_capture(args, dir.path()).await;
        res.unwrap();
        assert!(out.contains("from pl1"));
        assert!(!out.contains("from pl2"));
    }

    #[tokio::test]
    async fn filter_rhi_only() {
        let dir = tempfile::tempdir().unwrap();
        let plain = make_event("Rabc", 100, LogLevel::Info, Source::Rust, "plain");
        let mut rhi_ev = make_event("Rabc", 200, LogLevel::Info, Source::Rust, "rhi-call");
        rhi_ev.rhi_op = Some("acquire_texture".into());
        write_jsonl(dir.path(), "Rabc", 1, &[plain, rhi_ev]);

        let mut args = empty_args();
        args.runtime_id = Some("Rabc");
        args.rhi = true;
        let (out, _err, res) = run_capture(args, dir.path()).await;
        res.unwrap();
        assert!(!out.contains("plain"));
        assert!(out.contains("rhi-call"));
    }

    #[tokio::test]
    async fn filter_by_level() {
        let dir = tempfile::tempdir().unwrap();
        let trace = make_event("Rabc", 100, LogLevel::Trace, Source::Rust, "t-msg");
        let debug = make_event("Rabc", 200, LogLevel::Debug, Source::Rust, "d-msg");
        let info = make_event("Rabc", 300, LogLevel::Info, Source::Rust, "i-msg");
        let warn = make_event("Rabc", 400, LogLevel::Warn, Source::Rust, "w-msg");
        let error = make_event("Rabc", 500, LogLevel::Error, Source::Rust, "e-msg");
        write_jsonl(dir.path(), "Rabc", 1, &[trace, debug, info, warn, error]);

        let mut args = empty_args();
        args.runtime_id = Some("Rabc");
        args.level = Some("warn");
        let (out, _err, res) = run_capture(args, dir.path()).await;
        res.unwrap();
        assert!(!out.contains("t-msg"));
        assert!(!out.contains("d-msg"));
        assert!(!out.contains("i-msg"));
        assert!(out.contains("w-msg"));
        assert!(out.contains("e-msg"));
    }

    #[tokio::test]
    async fn filter_by_source() {
        let dir = tempfile::tempdir().unwrap();
        let r = make_event("Rabc", 100, LogLevel::Info, Source::Rust, "rust-msg");
        let p = make_event("Rabc", 200, LogLevel::Info, Source::Python, "python-msg");
        let d = make_event("Rabc", 300, LogLevel::Info, Source::Deno, "deno-msg");
        write_jsonl(dir.path(), "Rabc", 1, &[r, p, d]);

        let mut args = empty_args();
        args.runtime_id = Some("Rabc");
        args.source = Some("python");
        let (out, _err, res) = run_capture(args, dir.path()).await;
        res.unwrap();
        assert!(!out.contains("rust-msg"));
        assert!(out.contains("python-msg"));
        assert!(!out.contains("deno-msg"));
    }

    #[tokio::test]
    async fn filter_intercepted_only() {
        let dir = tempfile::tempdir().unwrap();
        let direct = make_event("Rabc", 100, LogLevel::Info, Source::Rust, "direct");
        let mut captured = make_event("Rabc", 200, LogLevel::Info, Source::Python, "captured");
        captured.intercepted = true;
        captured.channel = Some("stdout".into());
        write_jsonl(dir.path(), "Rabc", 1, &[direct, captured]);

        let mut args = empty_args();
        args.runtime_id = Some("Rabc");
        args.intercepted_only = true;
        let (out, _err, res) = run_capture(args, dir.path()).await;
        res.unwrap();
        assert!(!out.contains("direct"));
        assert!(out.contains("captured"));
    }

    #[tokio::test]
    async fn filters_compose_as_and() {
        let dir = tempfile::tempdir().unwrap();
        let mut a = make_event("Rabc", 100, LogLevel::Warn, Source::Rust, "a");
        a.processor_id = Some("p1".into());
        let mut b = make_event("Rabc", 200, LogLevel::Info, Source::Rust, "b"); // wrong level
        b.processor_id = Some("p1".into());
        let mut c = make_event("Rabc", 300, LogLevel::Warn, Source::Rust, "c"); // wrong processor
        c.processor_id = Some("p2".into());
        write_jsonl(dir.path(), "Rabc", 1, &[a, b, c]);

        let mut args = empty_args();
        args.runtime_id = Some("Rabc");
        args.processor = Some("p1");
        args.level = Some("warn");
        let (out, _err, res) = run_capture(args, dir.path()).await;
        res.unwrap();
        assert!(out.contains(" a"));
        assert!(!out.contains(" b"));
        assert!(!out.contains(" c"));
    }

    #[tokio::test]
    async fn follow_tails_new_lines() {
        let dir = tempfile::tempdir().unwrap();
        let initial = make_event("Rabc", 100, LogLevel::Info, Source::Rust, "first");
        let path = write_jsonl(dir.path(), "Rabc", 1, std::slice::from_ref(&initial));

        // Appender runs in its own task; the consumer future stays on the
        // current task so its non-Send `&mut dyn Write` sinks are fine.
        let appender_path = path.clone();
        let appender = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(400)).await;
            let appended =
                make_event("Rabc", 200, LogLevel::Info, Source::Rust, "second");
            let mut f = fs::OpenOptions::new()
                .append(true)
                .open(&appender_path)
                .unwrap();
            writeln!(f, "{}", serde_json::to_string(&appended).unwrap()).unwrap();
        });

        let mut args = empty_args();
        args.runtime_id = Some("Rabc");
        args.follow = true;
        let mut out = Vec::<u8>::new();
        let mut err = Vec::<u8>::new();
        let _ = tokio::time::timeout(
            Duration::from_millis(2_000),
            run_into(args, dir.path(), &mut out, &mut err),
        )
        .await;
        appender.await.unwrap();

        let out = String::from_utf8(out).unwrap();
        assert!(out.contains("first"), "follow output missing first record: {}", out);
        assert!(out.contains("second"), "follow output missing tailed record: {}", out);
    }

    #[tokio::test]
    async fn missing_runtime_id_error_is_clear() {
        let dir = tempfile::tempdir().unwrap();
        // Create some unrelated file so list isn't empty.
        let other = make_event("Rxyz", 100, LogLevel::Info, Source::Rust, "x");
        write_jsonl(dir.path(), "Rxyz", 1, std::slice::from_ref(&other));

        let mut args = empty_args();
        args.runtime_id = Some("Runknown");
        let (_out, _err, res) = run_capture(args, dir.path()).await;
        let err_msg = res.unwrap_err().to_string();
        assert!(err_msg.contains("Runknown"), "error should mention the unknown id: {}", err_msg);
        assert!(err_msg.contains("--list"), "error should suggest --list: {}", err_msg);
    }
}
