// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// WHIP (WebRTC-HTTP Ingestion Protocol) Client
//
// Implements RFC 9725 WHIP signaling for WebRTC streaming.

use crate::core::{Result, StreamError};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};

// ============================================================================
// WHIP CONFIGURATION
// ============================================================================

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct WhipConfig {
    pub endpoint_url: String,
    /// Optional Bearer token for authentication.
    /// Set to None for endpoints that don't require authentication (e.g., Cloudflare Stream).
    pub auth_token: Option<String>,
    pub timeout_ms: u64,
}

// ============================================================================
// WHIP CLIENT
// ============================================================================

/// WHIP (WebRTC-HTTP Ingestion Protocol) client per RFC 9725
///
/// Handles HTTP signaling for WebRTC streaming:
/// - POST /whip: Create session (SDP offer → answer)
/// - PATCH /session: Send ICE candidates (trickle ICE)
/// - DELETE /session: Terminate session
///
/// Uses pollster::block_on() for async HTTP calls (same pattern as WebRtcSession).
pub struct WhipClient {
    config: WhipConfig,

    /// HTTP client with HTTPS support
    /// Body type: http_body_util::combinators::BoxBody for flexibility
    http_client: hyper_util::client::legacy::Client<
        hyper_rustls::HttpsConnector<hyper_util::client::legacy::connect::HttpConnector>,
        http_body_util::combinators::BoxBody<
            bytes::Bytes,
            Box<dyn std::error::Error + Send + Sync>,
        >,
    >,

    /// Session URL (from Location header after POST success)
    session_url: Option<String>,

    /// Session ETag (for ICE restart - future use)
    session_etag: Option<String>,

    /// Pending ICE candidates (buffered for batch sending)
    pending_candidates: Arc<Mutex<Vec<String>>>,

    /// Shared tokio runtime handle for async HTTP operations.
    tokio_handle: tokio::runtime::Handle,
}

impl WhipClient {
    pub fn new(config: WhipConfig, tokio_handle: tokio::runtime::Handle) -> Result<Self> {
        tracing::info!(
            "[WhipClient] Creating WHIP client for endpoint: {}",
            config.endpoint_url
        );

        // Build HTTPS connector using rustls with ring crypto provider and native CA roots
        tracing::debug!("[WhipClient] Building HTTPS connector with native roots...");
        let https = hyper_rustls::HttpsConnectorBuilder::new()
            .with_native_roots() // Use system CA store (includes ring provider via feature flag)
            .map_err(|e| {
                tracing::error!("[WhipClient] Failed to load CA roots: {}", e);
                StreamError::Configuration(format!("Failed to load CA roots: {}", e))
            })?
            .https_or_http() // Allow http:// for local testing
            .enable_http1()
            .enable_http2()
            .build();
        tracing::debug!("[WhipClient] HTTPS connector built successfully");

        // Create HTTP client
        tracing::debug!("[WhipClient] Creating HTTP client...");
        let http_client =
            hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
                .pool_idle_timeout(std::time::Duration::from_secs(30))
                .build(https);
        tracing::info!("[WhipClient] HTTP client created successfully");

        Ok(Self {
            config,
            http_client,
            session_url: None,
            session_etag: None,
            pending_candidates: Arc::new(Mutex::new(Vec::new())),
            tokio_handle,
        })
    }

    /// POST SDP offer to WHIP endpoint, receive SDP answer.
    pub fn post_offer(&mut self, sdp_offer: &str) -> Result<String> {
        use http_body_util::Full;
        use hyper::{header, Request, StatusCode};

        // Clone what we need to avoid borrow issues
        let endpoint_url = self.config.endpoint_url.clone();
        let auth_token = self.config.auth_token.clone();
        let timeout_ms = self.config.timeout_ms;

        // Extract http_client reference before async block
        let http_client = &self.http_client;

        // MUST use Tokio runtime (tokio::time::timeout requires it)
        let result = self.tokio_handle.block_on(async {
            // Build POST request per RFC 9725 Section 4.1
            use http_body_util::BodyExt;
            let body = Full::new(bytes::Bytes::from(sdp_offer.to_owned()));
            let boxed_body = body.map_err(|never| match never {}).boxed();

            let mut req_builder = Request::builder()
                .method("POST")
                .uri(&endpoint_url)
                .header(header::CONTENT_TYPE, "application/sdp");

            // Add Authorization header only if token is provided
            if let Some(token) = &auth_token {
                req_builder =
                    req_builder.header(header::AUTHORIZATION, format!("Bearer {}", token));
            }

            let req = req_builder.body(boxed_body).map_err(|e| {
                StreamError::Runtime(format!("Failed to build WHIP POST request: {}", e))
            })?;

            tracing::debug!("WHIP POST to {}", endpoint_url);

            // Send request with timeout
            let response = tokio::time::timeout(
                std::time::Duration::from_millis(timeout_ms),
                http_client.request(req),
            )
            .await
            .map_err(|_| {
                StreamError::Runtime(format!("WHIP POST timed out after {}ms", timeout_ms))
            })?
            .map_err(|e| StreamError::Runtime(format!("WHIP POST request failed: {}", e)))?;

            let status = response.status();
            let headers = response.headers().clone();

            // Read response body
            let body_bytes = http_body_util::BodyExt::collect(response.into_body())
                .await
                .map_err(|e| {
                    StreamError::Runtime(format!("Failed to read WHIP response body: {}", e))
                })?
                .to_bytes();

            // Return status, headers, and body for processing outside async block
            Ok::<_, StreamError>((status, headers, body_bytes))
        })?;

        // Process response outside async block to avoid borrow conflicts
        let (status, headers, body_bytes) = result;

        match status {
            StatusCode::CREATED => {
                // Extract Location header (REQUIRED per RFC 9725)
                let location = headers
                    .get(header::LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .ok_or_else(|| {
                        StreamError::Runtime(
                            "WHIP server returned 201 Created without Location header".into(),
                        )
                    })?;

                // Convert relative URLs to absolute URLs
                // Cloudflare returns relative paths like "/stream-id/webRTC/publish/session-id"
                self.session_url = if location.starts_with('/') {
                    // Parse endpoint URL to get base
                    let base_url = self
                        .config
                        .endpoint_url
                        .split('/')
                        .take(3) // Take "https:", "", "hostname"
                        .collect::<Vec<_>>()
                        .join("/");
                    Some(format!("{}{}", base_url, location))
                } else {
                    Some(location.to_owned())
                };

                tracing::debug!(
                    "WHIP Location header: '{}' → session URL: '{}'",
                    location,
                    self.session_url.as_ref().unwrap()
                );

                // Extract ETag header (optional, used for ICE restart)
                self.session_etag = headers
                    .get(header::ETAG)
                    .and_then(|v| v.to_str().ok())
                    .map(|s| s.to_owned());

                // Parse SDP answer from body
                let sdp_answer = String::from_utf8(body_bytes.to_vec()).map_err(|e| {
                    StreamError::Runtime(format!("Invalid UTF-8 in SDP answer: {}", e))
                })?;

                tracing::info!(
                    "WHIP session created: {} (ETag: {})",
                    self.session_url.as_ref().unwrap(),
                    self.session_etag.as_deref().unwrap_or("none")
                );

                Ok(sdp_answer)
            }

            StatusCode::TEMPORARY_REDIRECT => {
                // Handle redirect per RFC 9725 Section 4.5
                let location = headers
                    .get(header::LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .ok_or_else(|| {
                        StreamError::Runtime("WHIP 307 redirect without Location header".into())
                    })?;

                tracing::info!("WHIP redirecting to: {}", location);

                // Update endpoint and retry (recursive, but 307 should be rare)
                self.config.endpoint_url = location.to_owned();
                self.post_offer(sdp_offer)
            }

            StatusCode::SERVICE_UNAVAILABLE => {
                // Server overloaded - caller should retry with backoff
                let retry_after = headers
                    .get(header::RETRY_AFTER)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("unknown");

                Err(StreamError::Runtime(format!(
                    "WHIP server overloaded (503), retry after: {}",
                    retry_after
                )))
            }

            _ => {
                // Other error (400, 401, 422, etc.)
                let error_body = String::from_utf8(body_bytes.to_vec())
                    .unwrap_or_else(|_| format!("HTTP {}", status));

                Err(StreamError::Runtime(format!(
                    "WHIP POST failed ({}): {}",
                    status, error_body
                )))
            }
        }
    }

    /// Queue an ICE candidate for batched transmission.
    pub fn queue_ice_candidate(&self, candidate_sdp: String) {
        self.pending_candidates.lock().unwrap().push(candidate_sdp);
    }

    /// Send pending ICE candidates to WHIP server via PATCH.
    pub fn send_ice_candidates(&self) -> Result<()> {
        use http_body_util::{BodyExt, Full};
        use hyper::{header, Request, StatusCode};

        let session_url = match &self.session_url {
            Some(url) => url,
            None => {
                return Err(StreamError::Configuration(
                    "Cannot send ICE candidates: no WHIP session URL".into(),
                ));
            }
        };

        // Drain pending candidates (atomic swap)
        let candidates = {
            let mut queue = self.pending_candidates.lock().unwrap();
            if queue.is_empty() {
                return Ok(()); // Nothing to send
            }
            queue.drain(..).collect::<Vec<_>>()
        };

        // Build SDP fragment per RFC 8840 (trickle-ice-sdpfrag)
        // Format: Multiple "a=candidate:..." lines joined by CRLF
        let sdp_fragment = candidates.join("\r\n");

        // MUST use Tokio runtime for HTTP operations
        self.tokio_handle.block_on(async {
            let body = Full::new(bytes::Bytes::from(sdp_fragment));
            let boxed_body = body.map_err(|never| match never {}).boxed();

            let mut req_builder = Request::builder()
                .method("PATCH")
                .uri(session_url)
                .header(header::CONTENT_TYPE, "application/trickle-ice-sdpfrag");

            // Add Authorization header only if token is provided
            if let Some(token) = &self.config.auth_token {
                req_builder =
                    req_builder.header(header::AUTHORIZATION, format!("Bearer {}", token));
            }

            let req = req_builder.body(boxed_body).map_err(|e| {
                StreamError::Runtime(format!("Failed to build WHIP PATCH request: {}", e))
            })?;

            tracing::debug!(
                "WHIP PATCH to {} ({} candidates)",
                session_url,
                candidates.len()
            );

            let response = tokio::time::timeout(
                std::time::Duration::from_millis(self.config.timeout_ms),
                self.http_client.request(req),
            )
            .await
            .map_err(|_| {
                StreamError::Runtime(format!(
                    "WHIP PATCH timed out after {}ms",
                    self.config.timeout_ms
                ))
            })?
            .map_err(|e| StreamError::Runtime(format!("WHIP PATCH request failed: {}", e)))?;

            let status = response.status();

            match status {
                StatusCode::NO_CONTENT => {
                    tracing::debug!("Sent {} ICE candidates to WHIP server", candidates.len());
                    Ok(())
                }
                StatusCode::OK => {
                    // 200 OK with body indicates ICE restart (server sends new candidates)
                    tracing::debug!("ICE restart response (200 OK)");
                    Ok(())
                }
                _ => {
                    // Read error body for debugging
                    let body_bytes = BodyExt::collect(response.into_body())
                        .await
                        .ok()
                        .and_then(|b| String::from_utf8(b.to_bytes().to_vec()).ok())
                        .unwrap_or_else(|| format!("HTTP {}", status));

                    Err(StreamError::Runtime(format!(
                        "WHIP PATCH failed: {}",
                        body_bytes
                    )))
                }
            }
        })
    }

    /// DELETE session to WHIP server (graceful shutdown)
    ///
    /// RFC 9725 Section 4.4: Client terminates session by DELETEing session URL.
    /// Server responds with 200 OK.
    pub fn terminate(&self) -> Result<()> {
        use http_body_util::Empty;
        use hyper::{header, Request};

        let session_url = match &self.session_url {
            Some(url) => url,
            None => {
                tracing::debug!("No WHIP session to terminate");
                return Ok(()); // No session was created
            }
        };

        // MUST use Tokio runtime for HTTP operations
        self.tokio_handle.block_on(async {
            use http_body_util::BodyExt;
            let body = Empty::<bytes::Bytes>::new();
            let boxed_body = body.map_err(|never| match never {}).boxed();

            let mut req_builder = Request::builder().method("DELETE").uri(session_url);

            // Add Authorization header only if token is provided
            if let Some(token) = &self.config.auth_token {
                req_builder =
                    req_builder.header(header::AUTHORIZATION, format!("Bearer {}", token));
            }

            let req = req_builder.body(boxed_body).map_err(|e| {
                StreamError::Runtime(format!("Failed to build WHIP DELETE request: {}", e))
            })?;

            tracing::debug!("WHIP DELETE to {}", session_url);

            let response = tokio::time::timeout(
                std::time::Duration::from_millis(self.config.timeout_ms),
                self.http_client.request(req),
            )
            .await
            .map_err(|_| {
                StreamError::Runtime(format!(
                    "WHIP DELETE timed out after {}ms",
                    self.config.timeout_ms
                ))
            })?
            .map_err(|e| StreamError::Runtime(format!("WHIP DELETE request failed: {}", e)))?;

            if response.status().is_success() {
                tracing::info!("WHIP session terminated: {}", session_url);
                Ok(())
            } else {
                // Non-fatal - session will timeout server-side
                tracing::warn!(
                    "WHIP DELETE failed (status {}), session may still exist server-side",
                    response.status()
                );
                Ok(())
            }
        })
    }
}
