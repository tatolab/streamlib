// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! OpenTelemetry LogExporter that writes log records to a SQLite database.

use std::sync::Arc;
use std::time::UNIX_EPOCH;

use opentelemetry_sdk::error::OTelSdkResult;
use opentelemetry_sdk::logs::{LogBatch, LogExporter};
use opentelemetry_sdk::Resource;

use crate::sqlite_telemetry_database::SqliteTelemetryDatabase;

/// Exports OpenTelemetry log records to a SQLite database.
#[derive(Debug)]
pub struct SqliteTelemetryLogExporter {
    database: Arc<SqliteTelemetryDatabase>,
    service_name: String,
}

impl SqliteTelemetryLogExporter {
    pub fn new(database: Arc<SqliteTelemetryDatabase>, service_name: String) -> Self {
        Self {
            database,
            service_name,
        }
    }

    fn insert_logs(&self, batch: &LogBatch<'_>) -> Result<(), rusqlite::Error> {
        let conn = self.database.connection();
        let mut stmt = conn.prepare_cached(
            "INSERT INTO logs (
                timestamp_unix_ns, trace_id, span_id,
                severity_number, severity_text, body,
                service_name, attributes_json, resource_json
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        )?;

        for (record, _scope) in batch.iter() {
            let timestamp_ns = record
                .timestamp()
                .or_else(|| record.observed_timestamp())
                .map(|ts| ts.duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos() as i64)
                .unwrap_or_else(|| {
                    std::time::SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap()
                        .as_nanos() as i64
                });

            let (trace_id, span_id) = record
                .trace_context()
                .map(|ctx| {
                    let tid = ctx.trace_id;
                    let sid = ctx.span_id;
                    let trace_str = if tid == opentelemetry::trace::TraceId::INVALID {
                        None
                    } else {
                        Some(tid.to_string())
                    };
                    let span_str = if sid == opentelemetry::trace::SpanId::INVALID {
                        None
                    } else {
                        Some(sid.to_string())
                    };
                    (trace_str, span_str)
                })
                .unwrap_or((None, None));

            let severity_number = record.severity_number().map(|s| s as i32);
            let severity_text = record.severity_text().map(|s| s.to_string());

            let body = record.body().map(crate::format_any_value);

            let attributes_json = {
                let mut attrs = serde_json::Map::new();
                for (key, value) in record.attributes_iter() {
                    attrs.insert(
                        key.to_string(),
                        serde_json::Value::String(format!("{:?}", value)),
                    );
                }
                if attrs.is_empty() {
                    None
                } else {
                    Some(serde_json::to_string(&attrs).unwrap_or_default())
                }
            };

            stmt.execute(rusqlite::params![
                timestamp_ns,
                trace_id,
                span_id,
                severity_number,
                severity_text,
                body,
                &self.service_name,
                attributes_json,
                Option::<String>::None, // resource_json
            ])?;
        }

        Ok(())
    }
}

impl LogExporter for SqliteTelemetryLogExporter {
    fn export(
        &self,
        batch: LogBatch<'_>,
    ) -> impl std::future::Future<Output = OTelSdkResult> + Send {
        let result = self.insert_logs(&batch);
        async move {
            result.map_err(|e| {
                opentelemetry_sdk::error::OTelSdkError::InternalFailure(format!(
                    "SQLite log export failed: {}",
                    e
                ))
            })
        }
    }

    fn set_resource(&mut self, _resource: &Resource) {
        // Resource is captured via service_name at construction time
    }
}
