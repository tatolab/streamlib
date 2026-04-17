// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! gRPC service implementation for broker diagnostics and telemetry ingestion.

use std::sync::Arc;

use tonic::{Request, Response, Status};

use crate::proto::broker_service_server::BrokerService;
use crate::proto::{
    ConnectionInfo, GetHealthRequest, GetHealthResponse, GetRuntimeEndpointRequest,
    GetRuntimeEndpointResponse, GetVersionRequest, GetVersionResponse, ListConnectionsRequest,
    ListConnectionsResponse, ListProcessorsRequest, ListProcessorsResponse, ListRuntimesRequest,
    ListRuntimesResponse, ListSurfacesRequest, ListSurfacesResponse, ProcessorInfo,
    PruneDeadRuntimesRequest, PruneDeadRuntimesResponse, RegisterRuntimeRequest,
    RegisterRuntimeResponse, RuntimeInfo, SnapshotSurfaceRequest, SnapshotSurfaceResponse,
    SurfaceInfo, UnregisterRuntimeRequest, UnregisterRuntimeResponse,
};
use crate::state::BrokerState;

use streamlib_telemetry::proto::telemetry_ingest_service_server::TelemetryIngestService;
use streamlib_telemetry::proto::{
    IngestTelemetryRequest, IngestTelemetryResponse, TelemetrySpanRecord,
};
use streamlib_telemetry::sqlite_telemetry_database::SqliteTelemetryDatabase;

/// Current protocol version. Bump when gRPC API changes.
pub const PROTOCOL_VERSION: u32 = 1;

/// gRPC service for broker diagnostics and telemetry ingestion.
pub struct BrokerGrpcService {
    state: BrokerState,
    telemetry_database: Arc<SqliteTelemetryDatabase>,
    otlp_span_exporter: Option<tokio::sync::Mutex<opentelemetry_otlp::SpanExporter>>,
}

impl BrokerGrpcService {
    /// Create a new gRPC service with shared state and telemetry database.
    pub fn new(
        state: BrokerState,
        telemetry_database: Arc<SqliteTelemetryDatabase>,
        otlp_span_exporter: Option<opentelemetry_otlp::SpanExporter>,
    ) -> Self {
        Self {
            state,
            telemetry_database,
            otlp_span_exporter: otlp_span_exporter.map(tokio::sync::Mutex::new),
        }
    }
}

#[tonic::async_trait]
impl BrokerService for BrokerGrpcService {
    async fn get_health(
        &self,
        _request: Request<GetHealthRequest>,
    ) -> Result<Response<GetHealthResponse>, Status> {
        let uptime = self.state.uptime_secs();

        Ok(Response::new(GetHealthResponse {
            healthy: true,
            status: "ok".to_string(),
            uptime_secs: uptime,
        }))
    }

    async fn get_version(
        &self,
        _request: Request<GetVersionRequest>,
    ) -> Result<Response<GetVersionResponse>, Status> {
        Ok(Response::new(GetVersionResponse {
            version: env!("CARGO_PKG_VERSION").to_string(),
            git_commit: option_env!("GIT_COMMIT").unwrap_or("unknown").to_string(),
            build_date: option_env!("BUILD_DATE").unwrap_or("unknown").to_string(),
            protocol_version: PROTOCOL_VERSION,
        }))
    }

    async fn list_runtimes(
        &self,
        _request: Request<ListRuntimesRequest>,
    ) -> Result<Response<ListRuntimesResponse>, Status> {
        let runtimes: Vec<RuntimeInfo> = self
            .state
            .get_runtimes()
            .into_iter()
            .map(|r| {
                let subprocess_count = self.state.subprocess_count_for_runtime(&r.runtime_id);
                let connection_count = self.state.connection_count_for_runtime(&r.runtime_id);
                RuntimeInfo {
                    runtime_id: r.runtime_id,
                    name: r.name,
                    api_endpoint: r.api_endpoint,
                    log_path: r.log_path,
                    pid: r.pid,
                    registered_at_unix_ms: r
                        .registered_at
                        .elapsed()
                        .as_millis()
                        .try_into()
                        .unwrap_or(0),
                    processor_count: subprocess_count as i32,
                    connection_count: connection_count as i32,
                }
            })
            .collect();

        Ok(Response::new(ListRuntimesResponse { runtimes }))
    }

    async fn list_processors(
        &self,
        request: Request<ListProcessorsRequest>,
    ) -> Result<Response<ListProcessorsResponse>, Status> {
        let runtime_id = &request.get_ref().runtime_id;

        let subprocesses = if runtime_id.is_empty() {
            self.state.get_subprocesses()
        } else {
            self.state.get_subprocesses_for_runtime(runtime_id)
        };

        let processors: Vec<ProcessorInfo> = subprocesses
            .into_iter()
            .map(|s| ProcessorInfo {
                runtime_id: s.runtime_id,
                processor_id: s.processor_id,
                processor_type: "Processor".to_string(),
                registered_at_unix_ms: s
                    .registered_at
                    .elapsed()
                    .as_millis()
                    .try_into()
                    .unwrap_or(0),
                state: "running".to_string(),
            })
            .collect();

        Ok(Response::new(ListProcessorsResponse { processors }))
    }

    async fn list_connections(
        &self,
        request: Request<ListConnectionsRequest>,
    ) -> Result<Response<ListConnectionsResponse>, Status> {
        let runtime_id = &request.get_ref().runtime_id;

        let connection_metadata = if runtime_id.is_empty() {
            self.state.get_connections()
        } else {
            self.state.get_connections_for_runtime(runtime_id)
        };

        let connections: Vec<ConnectionInfo> = connection_metadata
            .into_iter()
            .map(|c| ConnectionInfo {
                connection_id: c.connection_id,
                runtime_id: c.runtime_id,
                processor_id: c.processor_id,
                role: c.role,
                established_at_unix_ms: c
                    .established_at
                    .elapsed()
                    .as_millis()
                    .try_into()
                    .unwrap_or(0),
                frames_transferred: 0,
                bytes_transferred: 0,
            })
            .collect();

        Ok(Response::new(ListConnectionsResponse { connections }))
    }

    async fn get_runtime_endpoint(
        &self,
        request: Request<GetRuntimeEndpointRequest>,
    ) -> Result<Response<GetRuntimeEndpointResponse>, Status> {
        use crate::proto::get_runtime_endpoint_request::Query;

        let query = request.into_inner().query;

        let runtime = match query {
            Some(Query::Name(name)) => self.state.get_runtime_by_name(&name),
            Some(Query::RuntimeId(id)) => self.state.get_runtime_by_id(&id),
            None => {
                return Err(Status::invalid_argument(
                    "Query must specify name or runtime_id",
                ))
            }
        };

        match runtime {
            Some(r) => Ok(Response::new(GetRuntimeEndpointResponse {
                found: true,
                runtime_id: r.runtime_id,
                name: r.name,
                api_endpoint: r.api_endpoint,
                log_path: r.log_path,
            })),
            None => Ok(Response::new(GetRuntimeEndpointResponse {
                found: false,
                runtime_id: String::new(),
                name: String::new(),
                api_endpoint: String::new(),
                log_path: String::new(),
            })),
        }
    }

    async fn register_runtime(
        &self,
        request: Request<RegisterRuntimeRequest>,
    ) -> Result<Response<RegisterRuntimeResponse>, Status> {
        let req = request.into_inner();

        tracing::info!(
            "Registering runtime: {} (name: {}, pid: {}, api: {}, log: {})",
            req.runtime_id,
            req.name,
            req.pid,
            req.api_endpoint,
            req.log_path
        );

        self.state.register_runtime_with_metadata(
            &req.runtime_id,
            &req.name,
            &req.api_endpoint,
            &req.log_path,
            req.pid,
        );

        Ok(Response::new(RegisterRuntimeResponse {
            success: true,
            error: String::new(),
        }))
    }

    async fn unregister_runtime(
        &self,
        request: Request<UnregisterRuntimeRequest>,
    ) -> Result<Response<UnregisterRuntimeResponse>, Status> {
        let req = request.into_inner();

        tracing::info!("Unregistering runtime: {}", req.runtime_id);

        self.state.unregister_runtime(&req.runtime_id);

        Ok(Response::new(UnregisterRuntimeResponse { success: true }))
    }

    async fn prune_dead_runtimes(
        &self,
        _request: Request<PruneDeadRuntimesRequest>,
    ) -> Result<Response<PruneDeadRuntimesResponse>, Status> {
        let pruned_names = self.state.prune_dead_runtimes();
        let pruned_count = pruned_names.len() as i32;

        if pruned_count > 0 {
            tracing::info!(
                "Pruned {} dead runtime(s): {:?}",
                pruned_count,
                pruned_names
            );
        }

        Ok(Response::new(PruneDeadRuntimesResponse {
            pruned_count,
            pruned_names,
        }))
    }

    async fn list_surfaces(
        &self,
        request: Request<ListSurfacesRequest>,
    ) -> Result<Response<ListSurfacesResponse>, Status> {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            let runtime_id = &request.get_ref().runtime_id;

            let surface_metadata = self.state.get_surfaces();
            let surfaces: Vec<SurfaceInfo> = surface_metadata
                .into_iter()
                .filter(|s| runtime_id.is_empty() || s.runtime_id == *runtime_id)
                .map(|s| SurfaceInfo {
                    surface_id: s.surface_id,
                    runtime_id: s.runtime_id,
                    width: s.width,
                    height: s.height,
                    format: s.format,
                    resource_type: s.resource_type,
                    registered_at_unix_ms: s
                        .registered_at
                        .elapsed()
                        .as_millis()
                        .try_into()
                        .unwrap_or(0),
                    checkout_count: s.checkout_count,
                })
                .collect();

            Ok(Response::new(ListSurfacesResponse { surfaces }))
        }

        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            let _ = request;
            Ok(Response::new(ListSurfacesResponse {
                surfaces: Vec::new(),
            }))
        }
    }

    async fn snapshot_surface(
        &self,
        request: Request<SnapshotSurfaceRequest>,
    ) -> Result<Response<SnapshotSurfaceResponse>, Status> {
        #[cfg(target_os = "macos")]
        {
            use crate::xpc_ffi::{
                fourcc_to_string, kIOSurfaceLockReadOnly, CFRelease, IOSurfaceGetBaseAddress,
                IOSurfaceGetBytesPerRow, IOSurfaceGetHeight, IOSurfaceGetPixelFormat,
                IOSurfaceGetWidth, IOSurfaceLock, IOSurfaceLookupFromMachPort, IOSurfaceUnlock,
            };

            let surface_id = &request.get_ref().surface_id;

            // Get the mach port for this surface
            let mach_port = match self.state.get_surface_mach_port(surface_id) {
                Some(port) => port,
                None => {
                    return Ok(Response::new(SnapshotSurfaceResponse {
                        success: false,
                        error: format!("Surface '{}' not found", surface_id),
                        png_data: Vec::new(),
                        width: 0,
                        height: 0,
                    }));
                }
            };

            // Look up the IOSurface
            let iosurface = unsafe { IOSurfaceLookupFromMachPort(mach_port) };
            if iosurface.is_null() {
                return Ok(Response::new(SnapshotSurfaceResponse {
                    success: false,
                    error: "Failed to lookup IOSurface from mach port".to_string(),
                    png_data: Vec::new(),
                    width: 0,
                    height: 0,
                }));
            }

            // Get dimensions and format
            let width = unsafe { IOSurfaceGetWidth(iosurface) } as u32;
            let height = unsafe { IOSurfaceGetHeight(iosurface) } as u32;
            let bytes_per_row = unsafe { IOSurfaceGetBytesPerRow(iosurface) };
            let pixel_format = unsafe { IOSurfaceGetPixelFormat(iosurface) };
            let format_str = fourcc_to_string(pixel_format);

            // Only support BGRA for now
            if format_str != "BGRA" {
                unsafe { CFRelease(iosurface) };
                return Ok(Response::new(SnapshotSurfaceResponse {
                    success: false,
                    error: format!(
                        "Unsupported pixel format '{}'. Only BGRA is supported.",
                        format_str
                    ),
                    png_data: Vec::new(),
                    width,
                    height,
                }));
            }

            // Lock the surface for CPU read
            let lock_result =
                unsafe { IOSurfaceLock(iosurface, kIOSurfaceLockReadOnly, std::ptr::null_mut()) };
            if lock_result != 0 {
                unsafe { CFRelease(iosurface) };
                return Ok(Response::new(SnapshotSurfaceResponse {
                    success: false,
                    error: format!("Failed to lock IOSurface: error {}", lock_result),
                    png_data: Vec::new(),
                    width,
                    height,
                }));
            }

            // Read pixels
            let base_addr = unsafe { IOSurfaceGetBaseAddress(iosurface) };
            let mut rgba_data = Vec::with_capacity((width * height * 4) as usize);

            for y in 0..height {
                let row_ptr = unsafe { (base_addr as *const u8).add(y as usize * bytes_per_row) };
                for x in 0..width {
                    let pixel_ptr = unsafe { row_ptr.add(x as usize * 4) };
                    // BGRA -> RGBA
                    let b = unsafe { *pixel_ptr };
                    let g = unsafe { *pixel_ptr.add(1) };
                    let r = unsafe { *pixel_ptr.add(2) };
                    let a = unsafe { *pixel_ptr.add(3) };
                    rgba_data.push(r);
                    rgba_data.push(g);
                    rgba_data.push(b);
                    rgba_data.push(a);
                }
            }

            // Unlock the surface
            unsafe { IOSurfaceUnlock(iosurface, kIOSurfaceLockReadOnly, std::ptr::null_mut()) };
            unsafe { CFRelease(iosurface) };

            // Encode as PNG
            let png_data = match encode_png(&rgba_data, width, height) {
                Ok(data) => data,
                Err(e) => {
                    return Ok(Response::new(SnapshotSurfaceResponse {
                        success: false,
                        error: format!("Failed to encode PNG: {}", e),
                        png_data: Vec::new(),
                        width,
                        height,
                    }));
                }
            };

            Ok(Response::new(SnapshotSurfaceResponse {
                success: true,
                error: String::new(),
                png_data,
                width,
                height,
            }))
        }

        #[cfg(target_os = "linux")]
        {
            let _ = request;
            Ok(Response::new(SnapshotSurfaceResponse {
                success: false,
                error: "Surface snapshots are not yet supported on Linux".to_string(),
                png_data: Vec::new(),
                width: 0,
                height: 0,
            }))
        }

        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            let _ = request;
            Ok(Response::new(SnapshotSurfaceResponse {
                success: false,
                error: "Surface snapshots are not supported on this platform".to_string(),
                png_data: Vec::new(),
                width: 0,
                height: 0,
            }))
        }
    }
}

// -- TelemetryIngestService implementation --

#[tonic::async_trait]
impl TelemetryIngestService for BrokerGrpcService {
    async fn ingest_telemetry(
        &self,
        request: Request<IngestTelemetryRequest>,
    ) -> Result<Response<IngestTelemetryResponse>, Status> {
        let req = request.into_inner();
        let mut accepted_spans = 0i32;
        let mut accepted_logs = 0i32;

        // Insert spans
        if !req.spans.is_empty() {
            let conn = self.telemetry_database.connection();
            let mut stmt = conn
                .prepare_cached(
                    "INSERT OR REPLACE INTO spans (
                    trace_id, span_id, parent_span_id, operation_name, service_name,
                    span_kind, start_time_unix_ns, end_time_unix_ns, duration_ns,
                    status_code, status_message, attributes_json, resource_json, events_json
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
                )
                .map_err(|e| Status::internal(format!("SQLite prepare error: {}", e)))?;

            for span in &req.spans {
                let parent = if span.parent_span_id.is_empty() {
                    None
                } else {
                    Some(&span.parent_span_id)
                };
                let attrs = if span.attributes_json.is_empty() {
                    None
                } else {
                    Some(&span.attributes_json)
                };
                let resource = if span.resource_json.is_empty() {
                    None
                } else {
                    Some(&span.resource_json)
                };
                let events = if span.events_json.is_empty() {
                    None
                } else {
                    Some(&span.events_json)
                };
                let status_msg = if span.status_message.is_empty() {
                    None
                } else {
                    Some(&span.status_message)
                };

                match stmt.execute(rusqlite::params![
                    span.trace_id,
                    span.span_id,
                    parent,
                    span.operation_name,
                    span.service_name,
                    span.span_kind,
                    span.start_time_unix_ns,
                    span.end_time_unix_ns,
                    span.duration_ns,
                    span.status_code,
                    status_msg,
                    attrs,
                    resource,
                    events,
                ]) {
                    Ok(_) => accepted_spans += 1,
                    Err(e) => {
                        tracing::warn!("Failed to insert span: {}", e);
                    }
                }
            }
        }

        // Insert logs
        if !req.logs.is_empty() {
            let conn = self.telemetry_database.connection();
            let mut stmt = conn
                .prepare_cached(
                    "INSERT INTO logs (
                    timestamp_unix_ns, trace_id, span_id,
                    severity_number, severity_text, body,
                    service_name, attributes_json, resource_json
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                )
                .map_err(|e| Status::internal(format!("SQLite prepare error: {}", e)))?;

            for log in &req.logs {
                let trace_id = if log.trace_id.is_empty() {
                    None
                } else {
                    Some(&log.trace_id)
                };
                let span_id = if log.span_id.is_empty() {
                    None
                } else {
                    Some(&log.span_id)
                };
                let severity_num = if log.severity_number == 0 {
                    None
                } else {
                    Some(log.severity_number)
                };
                let severity_text = if log.severity_text.is_empty() {
                    None
                } else {
                    Some(&log.severity_text)
                };
                let body = if log.body.is_empty() {
                    None
                } else {
                    Some(&log.body)
                };
                let attrs = if log.attributes_json.is_empty() {
                    None
                } else {
                    Some(&log.attributes_json)
                };
                let resource = if log.resource_json.is_empty() {
                    None
                } else {
                    Some(&log.resource_json)
                };

                match stmt.execute(rusqlite::params![
                    log.timestamp_unix_ns,
                    trace_id,
                    span_id,
                    severity_num,
                    severity_text,
                    body,
                    log.service_name,
                    attrs,
                    resource,
                ]) {
                    Ok(_) => accepted_logs += 1,
                    Err(e) => {
                        tracing::warn!("Failed to insert log: {}", e);
                    }
                }
            }
        }

        // Forward spans to OTLP if configured
        if accepted_spans > 0 {
            if let Some(ref exporter_mutex) = self.otlp_span_exporter {
                let span_data = convert_proto_spans_to_span_data(&req.spans);
                if !span_data.is_empty() {
                    let exporter = exporter_mutex.lock().await;
                    if let Err(e) =
                        opentelemetry_sdk::trace::SpanExporter::export(&*exporter, span_data).await
                    {
                        tracing::warn!("OTLP span forward failed: {:?}", e);
                    }
                }
            }
        }

        Ok(Response::new(IngestTelemetryResponse {
            accepted_spans,
            accepted_logs,
        }))
    }
}

/// Convert proto span records to OTel SDK SpanData for OTLP export.
fn convert_proto_spans_to_span_data(
    spans: &[TelemetrySpanRecord],
) -> Vec<opentelemetry_sdk::trace::SpanData> {
    use opentelemetry::trace::{
        SpanContext, SpanId, SpanKind, Status, TraceFlags, TraceId, TraceState,
    };
    use opentelemetry::InstrumentationScope;
    use opentelemetry_sdk::trace::{SpanData, SpanEvents, SpanLinks};

    spans
        .iter()
        .filter_map(|span| {
            let trace_id = TraceId::from_hex(&span.trace_id).ok()?;
            let span_id = SpanId::from_hex(&span.span_id).ok()?;
            let parent = if span.parent_span_id.is_empty() {
                SpanId::INVALID
            } else {
                SpanId::from_hex(&span.parent_span_id).ok()?
            };

            let span_context = SpanContext::new(
                trace_id,
                span_id,
                TraceFlags::SAMPLED,
                false,
                TraceState::NONE,
            );

            let start_time = std::time::UNIX_EPOCH
                + std::time::Duration::from_nanos(span.start_time_unix_ns as u64);
            let end_time = std::time::UNIX_EPOCH
                + std::time::Duration::from_nanos(span.end_time_unix_ns as u64);

            let span_kind = match span.span_kind.as_str() {
                "Server" => SpanKind::Server,
                "Client" => SpanKind::Client,
                "Producer" => SpanKind::Producer,
                "Consumer" => SpanKind::Consumer,
                _ => SpanKind::Internal,
            };

            let status = match span.status_code.as_str() {
                "Ok" => Status::Ok,
                "Error" => Status::Error {
                    description: span.status_message.clone().into(),
                },
                _ => Status::Unset,
            };

            let attributes: Vec<opentelemetry::KeyValue> = if span.attributes_json.is_empty() {
                vec![]
            } else {
                serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(
                    &span.attributes_json,
                )
                .unwrap_or_default()
                .iter()
                .map(|(k, v)| {
                    opentelemetry::KeyValue::new(k.clone(), v.as_str().unwrap_or("").to_string())
                })
                .collect()
            };

            Some(SpanData {
                span_context,
                parent_span_id: parent,
                parent_span_is_remote: false,
                span_kind,
                name: span.operation_name.clone().into(),
                start_time,
                end_time,
                attributes,
                dropped_attributes_count: 0,
                events: SpanEvents::default(),
                links: SpanLinks::default(),
                status,
                instrumentation_scope: InstrumentationScope::builder(span.service_name.clone())
                    .build(),
            })
        })
        .collect()
}

/// Encode RGBA pixel data as PNG.
#[cfg(target_os = "macos")]
fn encode_png(rgba_data: &[u8], width: u32, height: u32) -> Result<Vec<u8>, String> {
    use image::codecs::png::PngEncoder;
    use image::ImageEncoder;

    let mut png_data = Vec::new();
    let encoder = PngEncoder::new(&mut png_data);
    encoder
        .write_image(rgba_data, width, height, image::ExtendedColorType::Rgba8)
        .map_err(|e| format!("PNG encoding error: {}", e))?;

    Ok(png_data)
}
