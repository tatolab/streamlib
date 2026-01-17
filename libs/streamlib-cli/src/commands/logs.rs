// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Log streaming commands.

use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{bail, Context, Result};

/// Get the streamlib logs directory (~/.streamlib/logs).
fn get_logs_dir() -> Result<PathBuf> {
    let home =
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Could not determine home directory"))?;
    Ok(home.join(".streamlib").join("logs"))
}

/// Parse a duration string like "5m", "1h", "30s" into a Duration.
fn parse_duration(s: &str) -> Result<Duration> {
    let s = s.trim();
    if s.is_empty() {
        bail!("Empty duration string");
    }

    let (num_str, unit) = if let Some(n) = s.strip_suffix('s') {
        (n, "s")
    } else if let Some(n) = s.strip_suffix('m') {
        (n, "m")
    } else if let Some(n) = s.strip_suffix('h') {
        (n, "h")
    } else if let Some(n) = s.strip_suffix('d') {
        (n, "d")
    } else {
        // Default to seconds if no unit
        (s, "s")
    };

    let num: u64 = num_str.parse().context("Invalid duration number")?;

    let secs = match unit {
        "s" => num,
        "m" => num * 60,
        "h" => num * 3600,
        "d" => num * 86400,
        _ => bail!("Unknown duration unit: {}", unit),
    };

    Ok(Duration::from_secs(secs))
}

/// Stream logs from a runtime.
pub async fn stream(runtime: &str, follow: bool, lines: usize, since: Option<&str>) -> Result<()> {
    // Find the log file for this runtime
    let logs_dir = get_logs_dir()?;
    let log_file = logs_dir.join(format!("{}.log", runtime));

    if !log_file.exists() {
        // Try to find a file that starts with the runtime name (partial match)
        let mut found = None;
        if let Ok(entries) = std::fs::read_dir(&logs_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                if name_str.starts_with(runtime) && name_str.ends_with(".log") {
                    found = Some(entry.path());
                    break;
                }
            }
        }

        match found {
            Some(path) => {
                println!("Found log file: {}", path.display());
                return stream_file(&path, follow, lines, since).await;
            }
            None => {
                bail!(
                    "Log file not found for runtime '{}'. Expected: {}\n\
                     Available logs:\n{}",
                    runtime,
                    log_file.display(),
                    list_available_logs(&logs_dir)?
                );
            }
        }
    }

    stream_file(&log_file, follow, lines, since).await
}

/// List available log files.
fn list_available_logs(logs_dir: &PathBuf) -> Result<String> {
    let mut logs = Vec::new();

    if logs_dir.exists() {
        for entry in std::fs::read_dir(logs_dir)?.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if name_str.ends_with(".log") {
                logs.push(format!("  - {}", name_str.trim_end_matches(".log")));
            }
        }
    }

    if logs.is_empty() {
        Ok("  (no logs found)".to_string())
    } else {
        Ok(logs.join("\n"))
    }
}

/// Parse a timestamp from the beginning of a log line.
/// Expected format: "2026-01-15T02:01:51.889896Z  INFO ..."
fn parse_log_timestamp(line: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    // Timestamp is at the start, ends before the first space after the 'Z'
    let timestamp_end = line.find("Z ")? + 1;
    let timestamp_str = &line[..timestamp_end];
    chrono::DateTime::parse_from_rfc3339(timestamp_str)
        .ok()
        .map(|dt| dt.with_timezone(&chrono::Utc))
}

/// Stream a log file with tail/follow support.
async fn stream_file(
    path: &PathBuf,
    follow: bool,
    lines: usize,
    since: Option<&str>,
) -> Result<()> {
    let file = File::open(path).context("Failed to open log file")?;
    let mut reader = BufReader::new(file);

    // Parse --since duration and calculate cutoff time
    let cutoff_time = if let Some(since_str) = since {
        let duration = parse_duration(since_str)?;
        Some(chrono::Utc::now() - chrono::Duration::from_std(duration)?)
    } else {
        None
    };

    // Read the file to get total lines for tail behavior
    let all_lines: Vec<String> = reader
        .by_ref()
        .lines()
        .collect::<std::result::Result<Vec<_>, _>>()?;

    // Filter by --since if specified
    let filtered_lines: Vec<&String> = if let Some(cutoff) = cutoff_time {
        all_lines
            .iter()
            .filter(|line| {
                parse_log_timestamp(line)
                    .map(|ts| ts >= cutoff)
                    .unwrap_or(true) // Keep lines without parseable timestamps
            })
            .collect()
    } else {
        all_lines.iter().collect()
    };

    // Print the last N lines
    let start = if filtered_lines.len() > lines {
        filtered_lines.len() - lines
    } else {
        0
    };

    for line in &filtered_lines[start..] {
        println!("{}", line);
    }

    // If follow mode, keep tailing the file
    if follow {
        // Seek to end of file
        let file = reader.into_inner();
        let mut reader = BufReader::new(file);
        reader.seek(SeekFrom::End(0))?;

        let mut pos = reader.stream_position()?;

        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) => {
                    // No new data - seek to current position to refresh file state
                    // This clears BufReader's EOF cache so it can see new appended content
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    reader.seek(SeekFrom::Start(pos))?;
                }
                Ok(n) => {
                    pos += n as u64;
                    print!("{}", line);
                    let _ = std::io::stdout().flush();
                }
                Err(e) => {
                    eprintln!("Error reading log: {}", e);
                    break;
                }
            }
        }
    }

    Ok(())
}
