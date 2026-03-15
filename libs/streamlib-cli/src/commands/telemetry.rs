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
    follow: bool,
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

    // Build base WHERE clause (reused for initial query and follow polling)
    let mut where_clauses = Vec::new();
    let mut base_params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

    if let Some(svc) = service {
        where_clauses.push("service_name LIKE ?");
        base_params.push(Box::new(format!("%{}%", svc)));
    }

    if let Some(since_str) = since {
        let duration = parse_duration(since_str)?;
        let cutoff_ns = cutoff_timestamp_ns(duration);
        where_clauses.push("timestamp_unix_ns > ?");
        base_params.push(Box::new(cutoff_ns));
    }

    if let Some(sev) = severity {
        where_clauses.push("severity_number >= ?");
        base_params.push(Box::new(sev));
    }

    let where_sql = if where_clauses.is_empty() {
        "1=1".to_string()
    } else {
        where_clauses.join(" AND ")
    };

    // Initial query — show last N lines
    let initial_query = format!(
        "SELECT timestamp_unix_ns, severity_text, service_name, body, trace_id, attributes_json
         FROM logs WHERE {} ORDER BY timestamp_unix_ns DESC LIMIT {}",
        where_sql, lines
    );

    let mut last_seen_ns: i64 = 0;

    {
        let mut stmt = conn.prepare(&initial_query)?;
        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            base_params.iter().map(|p| p.as_ref()).collect();
        let rows = stmt.query_map(param_refs.as_slice(), |row| {
            let timestamp_ns: i64 = row.get(0)?;
            let severity: Option<String> = row.get(1)?;
            let service: String = row.get(2)?;
            let body: Option<String> = row.get(3)?;
            let trace_id: Option<String> = row.get(4)?;
            Ok((timestamp_ns, severity, service, body, trace_id))
        })?;

        let mut results: Vec<_> = rows.filter_map(|r| r.ok()).collect();
        results.reverse();

        if results.is_empty() && !follow {
            println!("No log entries found.");
            return Ok(());
        }

        for (timestamp_ns, severity, service, body, trace_id) in &results {
            print_log_line(
                *timestamp_ns,
                severity.as_deref(),
                service,
                body.as_deref(),
                trace_id.as_deref(),
            );
            if *timestamp_ns > last_seen_ns {
                last_seen_ns = *timestamp_ns;
            }
        }
    }

    if !follow {
        return Ok(());
    }

    // Follow mode — poll for new entries every second
    let poll_query = format!(
        "SELECT timestamp_unix_ns, severity_text, service_name, body, trace_id, attributes_json
         FROM logs WHERE {} AND timestamp_unix_ns > ? ORDER BY timestamp_unix_ns ASC",
        where_sql
    );

    loop {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;

        let mut stmt = conn.prepare(&poll_query)?;
        let mut poll_params: Vec<&dyn rusqlite::types::ToSql> =
            base_params.iter().map(|p| p.as_ref()).collect();
        poll_params.push(&last_seen_ns);

        let rows = stmt.query_map(poll_params.as_slice(), |row| {
            let timestamp_ns: i64 = row.get(0)?;
            let severity: Option<String> = row.get(1)?;
            let service: String = row.get(2)?;
            let body: Option<String> = row.get(3)?;
            let trace_id: Option<String> = row.get(4)?;
            Ok((timestamp_ns, severity, service, body, trace_id))
        })?;

        for row in rows {
            let (timestamp_ns, severity, service, body, trace_id) = row?;
            print_log_line(
                timestamp_ns,
                severity.as_deref(),
                &service,
                body.as_deref(),
                trace_id.as_deref(),
            );
            if timestamp_ns > last_seen_ns {
                last_seen_ns = timestamp_ns;
            }
        }
    }
}

fn print_log_line(
    timestamp_ns: i64,
    severity: Option<&str>,
    service: &str,
    body: Option<&str>,
    trace_id: Option<&str>,
) {
    let ts = format_timestamp_ns(timestamp_ns);
    let sev = severity.unwrap_or("???");
    let msg = body.unwrap_or("");
    let trace = trace_id
        .filter(|t| !t.is_empty())
        .map(|t| format!(" [{}]", &t[..8.min(t.len())]))
        .unwrap_or_default();
    println!("{} {:>5} {} {}{}", ts, sev, service, msg, trace);
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

/// Export historical spans and logs from SQLite to an OTLP endpoint.
pub async fn export(endpoint: &str, since: Option<&str>, service: Option<&str>) -> Result<()> {
    use opentelemetry::trace::{
        SpanContext, SpanId, SpanKind, Status, TraceFlags, TraceId, TraceState,
    };
    use opentelemetry::InstrumentationScope;
    use opentelemetry_otlp::WithExportConfig;
    use opentelemetry_sdk::trace::{SpanData, SpanEvents, SpanLinks};

    let db_path =
        streamlib_telemetry::sqlite_telemetry_database::default_telemetry_database_path()?;
    if !db_path.exists() {
        bail!("Telemetry database not found at {}.", db_path.display());
    }

    let conn =
        rusqlite::Connection::open_with_flags(&db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .context("Failed to open telemetry database")?;

    // -- Export spans --
    let mut query = String::from(
        "SELECT trace_id, span_id, parent_span_id, operation_name, service_name,
                span_kind, start_time_unix_ns, end_time_unix_ns, duration_ns,
                status_code, status_message, attributes_json, events_json
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
    query.push_str(" ORDER BY start_time_unix_ns ASC");

    let mut stmt = conn.prepare(&query)?;
    let param_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();

    let mut span_batch: Vec<SpanData> = Vec::new();
    let rows = stmt.query_map(param_refs.as_slice(), |row| {
        let trace_id_str: String = row.get(0)?;
        let span_id_str: String = row.get(1)?;
        let parent_str: Option<String> = row.get(2)?;
        let operation: String = row.get(3)?;
        let service_name: String = row.get(4)?;
        let kind_str: Option<String> = row.get(5)?;
        let start_ns: i64 = row.get(6)?;
        let end_ns: i64 = row.get(7)?;
        let _duration_ns: i64 = row.get(8)?;
        let status_code: Option<String> = row.get(9)?;
        let status_message: Option<String> = row.get(10)?;
        let attrs_json: Option<String> = row.get(11)?;
        let _events_json: Option<String> = row.get(12)?;
        Ok((
            trace_id_str,
            span_id_str,
            parent_str,
            operation,
            service_name,
            kind_str,
            start_ns,
            end_ns,
            status_code,
            status_message,
            attrs_json,
        ))
    })?;

    for row_result in rows {
        let (
            trace_id_str,
            span_id_str,
            parent_str,
            operation,
            service_name,
            kind_str,
            start_ns,
            end_ns,
            status_code,
            status_message,
            attrs_json,
        ) = row_result?;

        let trace_id = TraceId::from_hex(&trace_id_str)
            .map_err(|_| anyhow::anyhow!("Invalid trace_id: {}", trace_id_str))?;
        let span_id = SpanId::from_hex(&span_id_str)
            .map_err(|_| anyhow::anyhow!("Invalid span_id: {}", span_id_str))?;
        let parent = parent_str
            .as_deref()
            .filter(|s| !s.is_empty())
            .map(SpanId::from_hex)
            .transpose()
            .map_err(|_| anyhow::anyhow!("Invalid parent_span_id"))?
            .unwrap_or(SpanId::INVALID);

        let span_context = SpanContext::new(
            trace_id,
            span_id,
            TraceFlags::SAMPLED,
            false,
            TraceState::NONE,
        );

        let start_time = std::time::UNIX_EPOCH + std::time::Duration::from_nanos(start_ns as u64);
        let end_time = std::time::UNIX_EPOCH + std::time::Duration::from_nanos(end_ns as u64);

        let span_kind = match kind_str.as_deref() {
            Some("Server") => SpanKind::Server,
            Some("Client") => SpanKind::Client,
            Some("Producer") => SpanKind::Producer,
            Some("Consumer") => SpanKind::Consumer,
            _ => SpanKind::Internal,
        };

        let status = match status_code.as_deref() {
            Some("Ok") => Status::Ok,
            Some("Error") => Status::Error {
                description: status_message.unwrap_or_default().into(),
            },
            _ => Status::Unset,
        };

        let attributes: Vec<opentelemetry::KeyValue> = if let Some(ref json) = attrs_json {
            serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(json)
                .unwrap_or_default()
                .iter()
                .map(|(k, v)| {
                    opentelemetry::KeyValue::new(k.clone(), v.as_str().unwrap_or("").to_string())
                })
                .collect()
        } else {
            vec![]
        };

        span_batch.push(SpanData {
            span_context,
            parent_span_id: parent,
            parent_span_is_remote: false,
            span_kind,
            name: operation.into(),
            start_time,
            end_time,
            attributes,
            dropped_attributes_count: 0,
            events: SpanEvents::default(),
            links: SpanLinks::default(),
            status,
            instrumentation_scope: InstrumentationScope::builder(service_name).build(),
        });
    }

    println!("Found {} span(s) to export.", span_batch.len());

    if !span_batch.is_empty() {
        let mut span_exporter = opentelemetry_otlp::SpanExporter::builder()
            .with_tonic()
            .with_endpoint(endpoint)
            .build()
            .context("Failed to create OTLP span exporter")?;

        // Set service.name resource so Jaeger/Tempo identifies the service
        use opentelemetry_sdk::trace::SpanExporter;
        let resource = opentelemetry_sdk::Resource::builder()
            .with_service_name("streamlib-runtime")
            .build();
        span_exporter.set_resource(&resource);

        span_exporter
            .export(span_batch)
            .await
            .map_err(|e| anyhow::anyhow!("OTLP span export failed: {:?}", e))?;
        println!("Spans exported successfully.");
    }

    // -- Export logs --
    let mut log_query = String::from(
        "SELECT timestamp_unix_ns, trace_id, span_id, severity_number, severity_text,
                body, service_name, attributes_json
         FROM logs WHERE 1=1",
    );
    let mut log_params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

    if let Some(svc) = service {
        log_query.push_str(" AND service_name LIKE ?");
        log_params.push(Box::new(format!("%{}%", svc)));
    }
    if let Some(since_str) = since {
        let duration = parse_duration(since_str)?;
        let cutoff_ns = cutoff_timestamp_ns(duration);
        log_query.push_str(" AND timestamp_unix_ns > ?");
        log_params.push(Box::new(cutoff_ns));
    }
    log_query.push_str(" ORDER BY timestamp_unix_ns ASC");

    let mut log_stmt = conn.prepare(&log_query)?;
    let log_param_refs: Vec<&dyn rusqlite::types::ToSql> =
        log_params.iter().map(|p| p.as_ref()).collect();

    let log_rows = log_stmt.query_map(log_param_refs.as_slice(), |row| {
        let timestamp_ns: i64 = row.get(0)?;
        let trace_id: Option<String> = row.get(1)?;
        let span_id: Option<String> = row.get(2)?;
        let severity_number: Option<i32> = row.get(3)?;
        let severity_text: Option<String> = row.get(4)?;
        let body: Option<String> = row.get(5)?;
        let service_name: String = row.get(6)?;
        let _attrs_json: Option<String> = row.get(7)?;
        Ok((
            timestamp_ns,
            trace_id,
            span_id,
            severity_number,
            severity_text,
            body,
            service_name,
        ))
    })?;

    let log_count = log_rows.filter_map(|r| r.ok()).count();
    println!(
        "Found {} log record(s) (log export via OTLP not yet implemented — spans exported).",
        log_count
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
