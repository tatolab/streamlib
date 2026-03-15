// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! SQLite database connection and schema management for telemetry storage.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result};
use rusqlite::Connection;

/// Wraps a SQLite connection configured for telemetry storage.
pub struct SqliteTelemetryDatabase {
    connection: Mutex<Connection>,
}

impl SqliteTelemetryDatabase {
    /// Open or create the telemetry database at the given path.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create directory: {}", parent.display()))?;
        }

        let connection = Connection::open(path)
            .with_context(|| format!("Failed to open SQLite database: {}", path.display()))?;

        // Configure WAL mode for concurrent readers/writers
        connection.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             PRAGMA busy_timeout=5000;",
        )?;

        let db = Self {
            connection: Mutex::new(connection),
        };
        db.create_schema()?;

        Ok(db)
    }

    fn create_schema(&self) -> Result<()> {
        let conn = self.connection.lock().unwrap();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS spans (
                trace_id TEXT NOT NULL,
                span_id TEXT NOT NULL,
                parent_span_id TEXT,
                operation_name TEXT NOT NULL,
                service_name TEXT NOT NULL,
                span_kind TEXT,
                start_time_unix_ns INTEGER NOT NULL,
                end_time_unix_ns INTEGER NOT NULL,
                duration_ns INTEGER NOT NULL,
                status_code TEXT,
                status_message TEXT,
                attributes_json TEXT,
                resource_json TEXT,
                events_json TEXT,
                PRIMARY KEY (trace_id, span_id)
            );
            CREATE INDEX IF NOT EXISTS idx_spans_start_time ON spans(start_time_unix_ns);
            CREATE INDEX IF NOT EXISTS idx_spans_service ON spans(service_name);
            CREATE INDEX IF NOT EXISTS idx_spans_status ON spans(status_code);

            CREATE TABLE IF NOT EXISTS logs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp_unix_ns INTEGER NOT NULL,
                trace_id TEXT,
                span_id TEXT,
                severity_number INTEGER,
                severity_text TEXT,
                body TEXT,
                service_name TEXT NOT NULL,
                attributes_json TEXT,
                resource_json TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_logs_timestamp ON logs(timestamp_unix_ns);
            CREATE INDEX IF NOT EXISTS idx_logs_service ON logs(service_name);
            CREATE INDEX IF NOT EXISTS idx_logs_severity ON logs(severity_number);
            CREATE INDEX IF NOT EXISTS idx_logs_trace ON logs(trace_id);",
        )
        .context("Failed to create telemetry schema")?;

        Ok(())
    }

    /// Access the underlying connection (locked).
    pub fn connection(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.connection.lock().unwrap()
    }

    /// Delete telemetry records older than `retain_days` days. Returns count of deleted rows.
    pub fn prune_older_than_days(&self, retain_days: u32) -> Result<u64> {
        let cutoff_ns = {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos() as i64;
            now - (retain_days as i64 * 86_400 * 1_000_000_000)
        };

        let conn = self.connection.lock().unwrap();

        let spans_deleted = conn.execute(
            "DELETE FROM spans WHERE start_time_unix_ns < ?1",
            rusqlite::params![cutoff_ns],
        )? as u64;

        let logs_deleted = conn.execute(
            "DELETE FROM logs WHERE timestamp_unix_ns < ?1",
            rusqlite::params![cutoff_ns],
        )? as u64;

        Ok(spans_deleted + logs_deleted)
    }
}

impl std::fmt::Debug for SqliteTelemetryDatabase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqliteTelemetryDatabase").finish()
    }
}

/// Resolve the default telemetry database path (~/.streamlib/telemetry.db).
pub fn default_telemetry_database_path() -> Result<PathBuf> {
    let home =
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Could not determine home directory"))?;
    Ok(home.join(".streamlib").join("telemetry.db"))
}
