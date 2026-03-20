// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! OpenTelemetry SpanExporter that sends spans to the broker via gRPC.

use std::sync::Mutex;
use std::time::UNIX_EPOCH;

use opentelemetry::trace::Status;
use opentelemetry_sdk::error::OTelSdkResult;
use opentelemetry_sdk::trace::{SpanData, SpanExporter};
use opentelemetry_sdk::Resource;
use tonic::transport::Channel;

use crate::proto::telemetry_ingest_service_client::TelemetryIngestServiceClient;
use crate::proto::{IngestTelemetryRequest, TelemetrySpanRecord};

/// Exports OpenTelemetry spans to the broker via gRPC.
pub struct BrokerTelemetrySpanExporter {
    endpoint: String,
    client: Mutex<Option<TelemetryIngestServiceClient<Channel>>>,
    service_name: String,
    runtime_handle: tokio::runtime::Handle,
}

impl std::fmt::Debug for BrokerTelemetrySpanExporter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BrokerTelemetrySpanExporter")
            .field("endpoint", &self.endpoint)
            .field("service_name", &self.service_name)
            .finish()
    }
}

impl BrokerTelemetrySpanExporter {
    pub fn new(endpoint: String, service_name: String) -> Self {
        Self {
            endpoint,
            client: Mutex::new(None),
            service_name,
            runtime_handle: tokio::runtime::Handle::current(),
        }
    }

    fn send_to_broker(&self, spans: Vec<TelemetrySpanRecord>) -> OTelSdkResult {
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
                spans,
                logs: vec![],
            };

            match client.ingest_telemetry(request).await {
                Ok(_) => Ok(()),
                Err(e) => {
                    *guard = None;
                    Err(opentelemetry_sdk::error::OTelSdkError::InternalFailure(
                        format!("Broker span ingest failed: {}", e),
                    ))
                }
            }
        })
    }

    fn convert_spans(&self, batch: &[SpanData]) -> Vec<TelemetrySpanRecord> {
        batch
            .iter()
            .map(|span| {
                let trace_id = span.span_context.trace_id().to_string();
                let span_id = span.span_context.span_id().to_string();
                let parent_span_id = {
                    let id = span.parent_span_id;
                    if id == opentelemetry::trace::SpanId::INVALID {
                        String::new()
                    } else {
                        id.to_string()
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
                    Status::Unset => ("Unset".to_string(), String::new()),
                    Status::Ok => ("Ok".to_string(), String::new()),
                    Status::Error { description } => {
                        ("Error".to_string(), description.to_string())
                    }
                };

                let attributes_json = if span.attributes.is_empty() {
                    String::new()
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
                    serde_json::to_string(&map).unwrap_or_default()
                };

                let events_json = if span.events.is_empty() {
                    String::new()
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
                    serde_json::to_string(&events).unwrap_or_default()
                };

                TelemetrySpanRecord {
                    trace_id,
                    span_id,
                    parent_span_id,
                    operation_name: span.name.to_string(),
                    service_name: self.service_name.clone(),
                    span_kind,
                    start_time_unix_ns: start_ns,
                    end_time_unix_ns: end_ns,
                    duration_ns,
                    status_code,
                    status_message,
                    attributes_json,
                    resource_json: String::new(),
                    events_json,
                }
            })
            .collect()
    }
}

impl SpanExporter for BrokerTelemetrySpanExporter {
    fn export(
        &self,
        batch: Vec<SpanData>,
    ) -> impl std::future::Future<Output = OTelSdkResult> + Send {
        let spans = self.convert_spans(&batch);
        let result = self.send_to_broker(spans);
        async move { result }
    }

    fn set_resource(&mut self, _resource: &Resource) {
        // Resource is captured via service_name at construction time
    }
}
