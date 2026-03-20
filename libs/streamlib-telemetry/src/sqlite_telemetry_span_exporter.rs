// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! OpenTelemetry SpanExporter that writes spans to a SQLite database.

use std::sync::Arc;
use std::time::UNIX_EPOCH;

use opentelemetry::trace::Status;
use opentelemetry_sdk::error::OTelSdkResult;
use opentelemetry_sdk::trace::{SpanData, SpanExporter};
use opentelemetry_sdk::Resource;

use crate::sqlite_telemetry_database::SqliteTelemetryDatabase;

/// Exports OpenTelemetry spans to a SQLite database.
#[derive(Debug)]
pub struct SqliteTelemetrySpanExporter {
    database: Arc<SqliteTelemetryDatabase>,
    service_name: String,
}

impl SqliteTelemetrySpanExporter {
    pub fn new(database: Arc<SqliteTelemetryDatabase>, service_name: String) -> Self {
        Self {
            database,
            service_name,
        }
    }

    fn insert_spans(&self, batch: &[SpanData]) -> Result<(), rusqlite::Error> {
        let conn = self.database.connection();
        let mut stmt = conn.prepare_cached(
            "INSERT OR REPLACE INTO spans (
                trace_id, span_id, parent_span_id, operation_name, service_name,
                span_kind, start_time_unix_ns, end_time_unix_ns, duration_ns,
                status_code, status_message, attributes_json, resource_json, events_json
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        )?;

        for span in batch {
            let trace_id = span.span_context.trace_id().to_string();
            let span_id = span.span_context.span_id().to_string();
            let parent_span_id = {
                let id = span.parent_span_id;
                if id == opentelemetry::trace::SpanId::INVALID {
                    None
                } else {
                    Some(id.to_string())
                }
            };

            let start_ns = span
                .start_time
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() as i64;
            let end_ns = span
                .end_time
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() as i64;
            let duration_ns = end_ns - start_ns;

            let span_kind = format!("{:?}", span.span_kind);

            let (status_code, status_message) = match &span.status {
                Status::Unset => ("Unset".to_string(), None),
                Status::Ok => ("Ok".to_string(), None),
                Status::Error { description } => {
                    ("Error".to_string(), Some(description.to_string()))
                }
            };

            let attributes_json = if span.attributes.is_empty() {
                None
            } else {
                let map: serde_json::Map<String, serde_json::Value> = span
                    .attributes
                    .iter()
                    .map(|kv| {
                        (
                            kv.key.to_string(),
                            serde_json::Value::String(kv.value.to_string()),
                        )
                    })
                    .collect();
                Some(serde_json::to_string(&map).unwrap_or_default())
            };

            let events_json = if span.events.is_empty() {
                None
            } else {
                let events: Vec<serde_json::Value> = span
                    .events
                    .iter()
                    .map(|event| {
                        let attrs: serde_json::Map<String, serde_json::Value> = event
                            .attributes
                            .iter()
                            .map(|kv| {
                                (
                                    kv.key.to_string(),
                                    serde_json::Value::String(kv.value.to_string()),
                                )
                            })
                            .collect();
                        serde_json::json!({
                            "name": event.name.to_string(),
                            "timestamp_ns": event.timestamp.duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos() as i64,
                            "attributes": attrs,
                        })
                    })
                    .collect();
                Some(serde_json::to_string(&events).unwrap_or_default())
            };

            stmt.execute(rusqlite::params![
                trace_id,
                span_id,
                parent_span_id,
                span.name.as_ref(),
                &self.service_name,
                span_kind,
                start_ns,
                end_ns,
                duration_ns,
                status_code,
                status_message,
                attributes_json,
                Option::<String>::None, // resource_json — resource is set at provider level
                events_json,
            ])?;
        }

        Ok(())
    }
}

impl SpanExporter for SqliteTelemetrySpanExporter {
    fn export(
        &self,
        batch: Vec<SpanData>,
    ) -> impl std::future::Future<Output = OTelSdkResult> + Send {
        let result = self.insert_spans(&batch);
        async move {
            result.map_err(|e| {
                opentelemetry_sdk::error::OTelSdkError::InternalFailure(format!(
                    "SQLite span export failed: {}",
                    e
                ))
            })
        }
    }

    fn set_resource(&mut self, _resource: &Resource) {
        // Resource is captured via service_name at construction time
    }
}
