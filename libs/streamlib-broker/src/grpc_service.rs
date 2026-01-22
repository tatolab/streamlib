// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! gRPC service implementation for broker diagnostics.

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

/// Current protocol version. Bump when gRPC API changes.
pub const PROTOCOL_VERSION: u32 = 1;

/// gRPC service for broker diagnostics.
pub struct BrokerGrpcService {
    state: BrokerState,
}

impl BrokerGrpcService {
    /// Create a new gRPC service with shared state.
    pub fn new(state: BrokerState) -> Self {
        Self { state }
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
        #[cfg(target_os = "macos")]
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

        #[cfg(not(target_os = "macos"))]
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
                fourcc_to_string, CFRelease, IOSurfaceGetBaseAddress, IOSurfaceGetBytesPerRow,
                IOSurfaceGetHeight, IOSurfaceGetPixelFormat, IOSurfaceGetWidth,
                IOSurfaceLookupFromMachPort, IOSurfaceLock, IOSurfaceUnlock, kIOSurfaceLockReadOnly,
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

        #[cfg(not(target_os = "macos"))]
        {
            let _ = request;
            Ok(Response::new(SnapshotSurfaceResponse {
                success: false,
                error: "Surface snapshots are only supported on macOS".to_string(),
                png_data: Vec::new(),
                width: 0,
                height: 0,
            }))
        }
    }
}

/// Encode RGBA pixel data as PNG.
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
