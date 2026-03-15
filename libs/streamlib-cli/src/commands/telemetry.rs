// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Telemetry query commands for structured logs and spans.

use std::time::Duration;

use anyhow::{bail, Context, Result};

/// Query structured logs from the telemetry SQLite database.
pub async fn logs(
    service: Option<&str>,
    since: Option<&str>,
    lines: usize,
    severity: Option<i32>,
) -> Result<()> {
    let db_path =
        streamlib_telemetry::sqlite_telemetry_database::default_telemetry_database_path()?;
    if !db_path.exists() {
        bail!(
            "Telemetry database not found at {}. Start a runtime to generate telemetry data.",
            db_path.display()
        );
    }

    let conn =
        rusqlite::Connection::open_with_flags(&db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .context("Failed to open telemetry database")?;

    let mut query = String::from(
        "SELECT timestamp_unix_ns, severity_text, service_name, body, trace_id, attributes_json
         FROM logs WHERE 1=1",
    );
    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

    if let Some(svc) = service {
        query.push_str(" AND service_name LIKE ?");
        params.push(Box::new(format!("%{}%", svc)));
    }

    if let Some(since_str) = since {
        let duration = parse_duration(since_str)?;
        let cutoff_ns = cutoff_timestamp_ns(duration);
        query.push_str(" AND timestamp_unix_ns > ?");
        params.push(Box::new(cutoff_ns));
    }

    if let Some(sev) = severity {
        query.push_str(" AND severity_number >= ?");
        params.push(Box::new(sev));
    }

    query.push_str(&format!(" ORDER BY timestamp_unix_ns DESC LIMIT {}", lines));

    let mut stmt = conn.prepare(&query)?;
    let param_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let rows = stmt.query_map(param_refs.as_slice(), |row| {
        let timestamp_ns: i64 = row.get(0)?;
        let severity: Option<String> = row.get(1)?;
        let service: String = row.get(2)?;
        let body: Option<String> = row.get(3)?;
        let trace_id: Option<String> = row.get(4)?;
        let attrs: Option<String> = row.get(5)?;
        Ok((timestamp_ns, severity, service, body, trace_id, attrs))
    })?;

    let mut results: Vec<_> = rows.filter_map(|r| r.ok()).collect();
    results.reverse(); // Show oldest first (natural reading order)

    if results.is_empty() {
        println!("No log entries found.");
        return Ok(());
    }

    for (timestamp_ns, severity, service, body, trace_id, _attrs) in &results {
        let ts = format_timestamp_ns(*timestamp_ns);
        let sev = severity.as_deref().unwrap_or("???");
        let msg = body.as_deref().unwrap_or("");
        let trace = trace_id
            .as_ref()
            .map(|t| format!(" [{}]", &t[..8.min(t.len())]))
            .unwrap_or_default();
        println!("{} {:>5} {} {}{}", ts, sev, service, msg, trace);
    }

    Ok(())
}

/// Query spans/traces from the telemetry SQLite database.
pub async fn spans(
    service: Option<&str>,
    since: Option<&str>,
    lines: usize,
    status: Option<&str>,
) -> Result<()> {
    let db_path =
        streamlib_telemetry::sqlite_telemetry_database::default_telemetry_database_path()?;
    if !db_path.exists() {
        bail!(
            "Telemetry database not found at {}. Start a runtime to generate telemetry data.",
            db_path.display()
        );
    }

    let conn =
        rusqlite::Connection::open_with_flags(&db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .context("Failed to open telemetry database")?;

    let mut query = String::from(
        "SELECT trace_id, span_id, operation_name, service_name, span_kind,
                start_time_unix_ns, duration_ns, status_code, status_message
         FROM spans WHERE 1=1",
    );
    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

    if let Some(svc) = service {
        query.push_str(" AND service_name LIKE ?");
        params.push(Box::new(format!("%{}%", svc)));
    }

    if let Some(since_str) = since {
        let duration = parse_duration(since_str)?;
        let cutoff_ns = cutoff_timestamp_ns(duration);
        query.push_str(" AND start_time_unix_ns > ?");
        params.push(Box::new(cutoff_ns));
    }

    if let Some(st) = status {
        query.push_str(" AND status_code = ?");
        params.push(Box::new(st.to_string()));
    }

    query.push_str(&format!(
        " ORDER BY start_time_unix_ns DESC LIMIT {}",
        lines
    ));

    let mut stmt = conn.prepare(&query)?;
    let param_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let rows = stmt.query_map(param_refs.as_slice(), |row| {
        let trace_id: String = row.get(0)?;
        let span_id: String = row.get(1)?;
        let operation: String = row.get(2)?;
        let service: String = row.get(3)?;
        let kind: Option<String> = row.get(4)?;
        let start_ns: i64 = row.get(5)?;
        let duration_ns: i64 = row.get(6)?;
        let status_code: Option<String> = row.get(7)?;
        let status_msg: Option<String> = row.get(8)?;
        Ok((
            trace_id,
            span_id,
            operation,
            service,
            kind,
            start_ns,
            duration_ns,
            status_code,
            status_msg,
        ))
    })?;

    let mut results: Vec<_> = rows.filter_map(|r| r.ok()).collect();
    results.reverse();

    if results.is_empty() {
        println!("No spans found.");
        return Ok(());
    }

    for (
        trace_id,
        _span_id,
        operation,
        service,
        kind,
        start_ns,
        duration_ns,
        status_code,
        status_msg,
    ) in &results
    {
        let ts = format_timestamp_ns(*start_ns);
        let dur = format_duration_ns(*duration_ns);
        let k = kind.as_deref().unwrap_or("?");
        let st = status_code.as_deref().unwrap_or("?");
        let msg = status_msg
            .as_ref()
            .map(|m| format!(" ({})", m))
            .unwrap_or_default();
        println!(
            "{} {} {:>8} {} {} [{}] {}{}",
            ts,
            &trace_id[..8.min(trace_id.len())],
            dur,
            service,
            operation,
            k,
            st,
            msg
        );
    }

    Ok(())
}

/// Delete old telemetry data.
pub async fn prune(older_than: &str) -> Result<()> {
    let duration = parse_duration(older_than)?;
    let days = (duration.as_secs() / 86400).max(1) as u32;
    let deleted = streamlib_telemetry::prune_old_telemetry(days)?;
    println!(
        "Deleted {} telemetry record(s) older than {}.",
        deleted, older_than
    );
    Ok(())
}

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

fn cutoff_timestamp_ns(duration: Duration) -> i64 {
    let now_ns = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos() as i64;
    now_ns - (duration.as_nanos() as i64)
}

fn format_timestamp_ns(ns: i64) -> String {
    let secs = ns / 1_000_000_000;
    let subsec_ms = (ns % 1_000_000_000) / 1_000_000;
    let dt =
        chrono::DateTime::from_timestamp(secs, (subsec_ms * 1_000_000) as u32).unwrap_or_default();
    dt.format("%Y-%m-%dT%H:%M:%S.%3fZ").to_string()
}

fn format_duration_ns(ns: i64) -> String {
    if ns < 1_000 {
        format!("{}ns", ns)
    } else if ns < 1_000_000 {
        format!("{:.1}µs", ns as f64 / 1_000.0)
    } else if ns < 1_000_000_000 {
        format!("{:.1}ms", ns as f64 / 1_000_000.0)
    } else {
        format!("{:.2}s", ns as f64 / 1_000_000_000.0)
    }
}
