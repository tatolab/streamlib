// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! OpenTelemetry LogExporter that sends log records to the broker via gRPC.

use std::sync::Mutex;
use std::time::UNIX_EPOCH;

use opentelemetry_sdk::error::OTelSdkResult;
use opentelemetry_sdk::logs::{LogBatch, LogExporter};
use opentelemetry_sdk::Resource;
use tonic::transport::Channel;

use crate::format_any_value;
use crate::proto::telemetry_ingest_service_client::TelemetryIngestServiceClient;
use crate::proto::{IngestTelemetryRequest, TelemetryLogRecord};

/// Exports OpenTelemetry log records to the broker via gRPC.
pub struct BrokerTelemetryLogExporter {
    endpoint: String,
    client: Mutex<Option<TelemetryIngestServiceClient<Channel>>>,
    service_name: String,
    runtime_handle: tokio::runtime::Handle,
}

impl std::fmt::Debug for BrokerTelemetryLogExporter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BrokerTelemetryLogExporter")
            .field("endpoint", &self.endpoint)
            .field("service_name", &self.service_name)
            .finish()
    }
}

impl BrokerTelemetryLogExporter {
    pub fn new(endpoint: String, service_name: String) -> Self {
        Self {
            endpoint,
            client: Mutex::new(None),
            service_name,
            runtime_handle: tokio::runtime::Handle::current(),
        }
    }

    fn send_to_broker(&self, logs: Vec<TelemetryLogRecord>) -> OTelSdkResult {
        let mut guard = self.client.lock().unwrap();

        self.runtime_handle.block_on(async {
            if guard.is_none() {
                match TelemetryIngestServiceClient::connect(self.endpoint.clone()).await {
                    Ok(c) => *guard = Some(c),
                    Err(e) => {
                        return Err(opentelemetry_sdk::error::OTelSdkError::InternalFailure(
                            format!("Broker connection failed: {}", e),
                        ));
                    }
                }
            }

            let client = guard.as_mut().unwrap();
            let request = IngestTelemetryRequest {
                spans: vec![],
                logs,
            };

            match client.ingest_telemetry(request).await {
                Ok(_) => Ok(()),
                Err(e) => {
                    *guard = None;
                    Err(opentelemetry_sdk::error::OTelSdkError::InternalFailure(
                        format!("Broker log ingest failed: {}", e),
                    ))
                }
            }
        })
    }

    fn convert_logs(&self, batch: &LogBatch<'_>) -> Vec<TelemetryLogRecord> {
        batch
            .iter()
            .map(|(record, _scope)| {
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
                            String::new()
                        } else {
                            tid.to_string()
                        };
                        let span_str = if sid == opentelemetry::trace::SpanId::INVALID {
                            String::new()
                        } else {
                            sid.to_string()
                        };
                        (trace_str, span_str)
                    })
                    .unwrap_or_default();

                let severity_number = record.severity_number().map(|s| s as i32).unwrap_or(0);
                let severity_text = record
                    .severity_text()
                    .map(|s| s.to_string())
                    .unwrap_or_default();

                let body = record.body().map(format_any_value).unwrap_or_default();

                let attributes_json = {
                    let mut attrs = serde_json::Map::new();
                    for (key, value) in record.attributes_iter() {
                        attrs.insert(
                            key.to_string(),
                            serde_json::Value::String(format!("{:?}", value)),
                        );
                    }
                    if attrs.is_empty() {
                        String::new()
                    } else {
                        serde_json::to_string(&attrs).unwrap_or_default()
                    }
                };

                TelemetryLogRecord {
                    timestamp_unix_ns: timestamp_ns,
                    trace_id,
                    span_id,
                    severity_number,
                    severity_text,
                    body,
                    service_name: self.service_name.clone(),
                    attributes_json,
                    resource_json: String::new(),
                }
            })
            .collect()
    }
}

impl LogExporter for BrokerTelemetryLogExporter {
    fn export(
        &self,
        batch: LogBatch<'_>,
    ) -> impl std::future::Future<Output = OTelSdkResult> + Send {
        let logs = self.convert_logs(&batch);
        let result = self.send_to_broker(logs);
        async move { result }
    }

    fn set_resource(&mut self, _resource: &Resource) {
        // Resource is captured via service_name at construction time
    }
}
