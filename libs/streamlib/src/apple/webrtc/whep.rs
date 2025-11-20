// WHEP (WebRTC-HTTP Egress Protocol) Client
//
// Implements IETF WHEP specification for WebRTC playback/egress.
// Based on draft-ietf-wish-whep (mirrors WHIP design for receive-only streams).

use crate::core::{StreamError, Result};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};

// ============================================================================
// WHEP CONFIGURATION
// ============================================================================

#[derive(Clone, Serialize, Deserialize)]
pub struct WhepConfig {
    pub endpoint_url: String,
    /// Optional Bearer token for authentication (per RFC 6750 Section 2.1).
    /// Required for most WHEP deployments (Cloudflare Stream, etc.).
    pub auth_token: Option<String>,
    pub timeout_ms: u64,
}

// ============================================================================
// WHEP CLIENT
// ============================================================================

/// WHEP (WebRTC-HTTP Egress Protocol) client per IETF draft-ietf-wish-whep
///
/// Handles HTTP signaling for WebRTC playback (receive-only streams):
/// - POST /whep: Create session (SDP offer → answer or counter-offer)
/// - PATCH /session: Send ICE candidates (trickle ICE)
/// - DELETE /session: Terminate session
///
/// Key differences from WHIP:
/// - Client sends SDP offer with "recvonly" media direction
/// - Server may respond with 201 (accept) or 406 (counter-offer)
/// - Unidirectional: server-to-client only
/// - No SDP renegotiation after initial exchange
pub struct WhepClient {
    config: WhepConfig,

    /// HTTP client with HTTPS support
    http_client: hyper_util::client::legacy::Client<
        hyper_rustls::HttpsConnector<hyper_util::client::legacy::connect::HttpConnector>,
        http_body_util::combinators::BoxBody<bytes::Bytes, Box<dyn std::error::Error + Send + Sync>>,
    >,

    /// Session URL (from Location header after POST success)
    session_url: Option<String>,

    /// Session ETag (for ICE restart support)
    session_etag: Option<String>,

    /// Pending ICE candidates (buffered for batch sending)
    pending_candidates: Arc<Mutex<Vec<String>>>,

    /// Tokio runtime for HTTP operations
    _runtime: tokio::runtime::Runtime,
}

impl WhepClient {
    pub fn new(config: WhepConfig) -> Result<Self> {
        // Install rustls crypto provider (ring) globally - only done once per process
        static CRYPTO_PROVIDER_INIT: std::sync::Once = std::sync::Once::new();
        CRYPTO_PROVIDER_INIT.call_once(|| {
            let _ = rustls::crypto::ring::default_provider().install_default();
            tracing::debug!("[WhepClient] Installed rustls ring crypto provider");
        });

        tracing::info!("[WhepClient] Creating WHEP client for endpoint: {}", config.endpoint_url);

        // Build HTTPS connector using rustls with ring crypto provider and native CA roots
        tracing::debug!("[WhepClient] Building HTTPS connector with native roots...");
        let https = hyper_rustls::HttpsConnectorBuilder::new()
            .with_native_roots()  // Use system CA store
            .map_err(|e| {
                tracing::error!("[WhepClient] Failed to load CA roots: {}", e);
                StreamError::Configuration(format!("Failed to load CA roots: {}", e))
            })?
            .https_or_http()      // Allow http:// for local testing
            .enable_http1()
            .enable_http2()
            .build();
        tracing::debug!("[WhepClient] HTTPS connector built successfully");

        // Create HTTP client
        tracing::debug!("[WhepClient] Creating HTTP client...");
        let http_client = hyper_util::client::legacy::Client::builder(
            hyper_util::rt::TokioExecutor::new()
        )
        .pool_idle_timeout(std::time::Duration::from_secs(30))
        .build(https);
        tracing::info!("[WhepClient] HTTP client created successfully");

        // Create Tokio runtime for HTTP operations
        tracing::debug!("[WhepClient] Creating Tokio runtime for HTTP operations...");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| StreamError::Runtime(format!("Failed to create Tokio runtime for WHEP client: {}", e)))?;
        tracing::debug!("[WhepClient] Tokio runtime created successfully");

        Ok(Self {
            config,
            http_client,
            session_url: None,
            session_etag: None,
            pending_candidates: Arc::new(Mutex::new(Vec::new())),
            _runtime: runtime,
        })
    }

    /// POST SDP offer to WHEP endpoint, receive SDP answer (or counter-offer)
    ///
    /// WHEP Spec: Client creates session by POSTing SDP offer with "recvonly" media direction.
    /// Server responds with:
    /// - 201 Created + SDP answer: Server accepts client's offer
    /// - 406 Not Acceptable + SDP counter-offer: Server proposes alternative configuration
    ///
    /// Both responses include Location header (session URL) and optional ETag.
    ///
    /// # Arguments
    /// * `sdp_offer` - SDP offer string (application/sdp) with "recvonly" attributes
    ///
    /// # Returns
    /// Tuple of (SDP answer/counter-offer, is_counter_offer flag)
    ///
    /// # Errors
    /// - 400 Bad Request: Malformed SDP
    /// - 401 Unauthorized: Invalid auth token
    /// - 503 Service Unavailable: Server overloaded (retry with backoff)
    pub fn post_offer(&mut self, sdp_offer: &str) -> Result<(String, bool)> {
        use hyper::{Request, StatusCode, header};
        use http_body_util::Full;

        let endpoint_url = self.config.endpoint_url.clone();
        let auth_token = self.config.auth_token.clone();
        let timeout_ms = self.config.timeout_ms;
        let http_client = &self.http_client;

        // Execute HTTP request in Tokio runtime
        let result = self._runtime.block_on(async {
            use http_body_util::BodyExt;
            let body = Full::new(bytes::Bytes::from(sdp_offer.to_owned()));
            let boxed_body = body.map_err(|never| match never {}).boxed();

            let mut req_builder = Request::builder()
                .method("POST")
                .uri(&endpoint_url)
                .header(header::CONTENT_TYPE, "application/sdp");

            // Add Authorization header if token provided
            if let Some(token) = &auth_token {
                req_builder = req_builder.header(header::AUTHORIZATION, format!("Bearer {}", token));
            }

            let req = req_builder.body(boxed_body)
                .map_err(|e| StreamError::Runtime(format!("Failed to build WHEP POST request: {}", e)))?;

            tracing::debug!("[WhepClient] POST to {}", endpoint_url);

            // Send request with timeout
            let response = tokio::time::timeout(
                std::time::Duration::from_millis(timeout_ms),
                http_client.request(req),
            )
            .await
            .map_err(|_| StreamError::Runtime(format!("WHEP POST timed out after {}ms", timeout_ms)))?
            .map_err(|e| StreamError::Runtime(format!("WHEP POST request failed: {}", e)))?;

            let status = response.status();
            let headers = response.headers().clone();

            // Read response body
            let body_bytes = http_body_util::BodyExt::collect(response.into_body())
                .await
                .map_err(|e| StreamError::Runtime(format!("Failed to read WHEP response body: {}", e)))?
                .to_bytes();

            Ok::<_, StreamError>((status, headers, body_bytes))
        })?;

        let (status, headers, body_bytes) = result;

        match status {
            StatusCode::CREATED | StatusCode::NOT_ACCEPTABLE => {
                let is_counter_offer = status == StatusCode::NOT_ACCEPTABLE;

                // Extract Location header (REQUIRED)
                let location = headers
                    .get(header::LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .ok_or_else(|| StreamError::Runtime(
                        format!("WHEP server returned {} without Location header", status)
                    ))?;

                // Convert relative URLs to absolute URLs
                self.session_url = if location.starts_with('/') {
                    let base_url = self.config.endpoint_url
                        .split('/')
                        .take(3)  // "https:", "", "hostname"
                        .collect::<Vec<_>>()
                        .join("/");
                    Some(format!("{}{}", base_url, location))
                } else {
                    Some(location.to_owned())
                };

                tracing::debug!(
                    "[WhepClient] Location header: '{}' → session URL: '{}'",
                    location,
                    self.session_url.as_ref().unwrap()
                );

                // Extract ETag header (optional, used for ICE restart)
                self.session_etag = headers
                    .get(header::ETAG)
                    .and_then(|v| v.to_str().ok())
                    .map(|s| s.to_owned());

                // Parse SDP answer/counter-offer from body
                let sdp_response = String::from_utf8(body_bytes.to_vec())
                    .map_err(|e| StreamError::Runtime(format!("Invalid UTF-8 in SDP response: {}", e)))?;

                if is_counter_offer {
                    tracing::info!(
                        "[WhepClient] Received counter-offer (406): {} (ETag: {})",
                        self.session_url.as_ref().unwrap(),
                        self.session_etag.as_deref().unwrap_or("none")
                    );
                } else {
                    tracing::info!(
                        "[WhepClient] Session created (201): {} (ETag: {})",
                        self.session_url.as_ref().unwrap(),
                        self.session_etag.as_deref().unwrap_or("none")
                    );
                }

                Ok((sdp_response, is_counter_offer))
            }

            StatusCode::SERVICE_UNAVAILABLE => {
                let retry_after = headers
                    .get(header::RETRY_AFTER)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("unknown");

                Err(StreamError::Runtime(format!(
                    "WHEP server overloaded (503), retry after: {}",
                    retry_after
                )))
            }

            _ => {
                let error_body = String::from_utf8(body_bytes.to_vec())
                    .unwrap_or_else(|_| format!("HTTP {}", status));

                Err(StreamError::Runtime(format!(
                    "WHEP POST failed ({}): {}",
                    status,
                    error_body
                )))
            }
        }
    }

    /// Queue an ICE candidate for batched transmission
    ///
    /// Candidates are buffered and sent in batches via PATCH to reduce HTTP overhead.
    ///
    /// # Arguments
    /// * `candidate_sdp` - ICE candidate in SDP fragment format (e.g., "a=candidate:...")
    pub fn queue_ice_candidate(&self, candidate_sdp: String) {
        self.pending_candidates.lock().unwrap().push(candidate_sdp);
    }

    /// Send pending ICE candidates to WHEP server via PATCH
    ///
    /// WHEP Spec: Trickle ICE candidates sent via PATCH with
    /// Content-Type: application/trickle-ice-sdpfrag
    ///
    /// Sends all buffered candidates in a single PATCH request, then clears the queue.
    pub fn send_ice_candidates(&self) -> Result<()> {
        use hyper::{Request, StatusCode, header};
        use http_body_util::{BodyExt, Full};

        let session_url = match &self.session_url {
            Some(url) => url,
            None => {
                return Err(StreamError::Configuration(
                    "Cannot send ICE candidates: no WHEP session URL".into()
                ));
            }
        };

        // Drain pending candidates
        let candidates = {
            let mut pending = self.pending_candidates.lock().unwrap();
            std::mem::take(&mut *pending)
        };

        if candidates.is_empty() {
            tracing::debug!("[WhepClient] No ICE candidates to send");
            return Ok(());
        }

        // Build SDP fragment: media-level attributes for bundled media
        // Format: "a=candidate:..." lines separated by CRLF
        let sdp_fragment = candidates.join("\r\n");

        tracing::debug!(
            "[WhepClient] Sending {} ICE candidates ({} bytes)",
            candidates.len(),
            sdp_fragment.len()
        );

        self._runtime.block_on(async {
            let body = Full::new(bytes::Bytes::from(sdp_fragment));
            let boxed_body = body.map_err(|never| match never {}).boxed();

            let mut req_builder = Request::builder()
                .method("PATCH")
                .uri(session_url)
                .header(header::CONTENT_TYPE, "application/trickle-ice-sdpfrag");

            if let Some(token) = &self.config.auth_token {
                req_builder = req_builder.header(header::AUTHORIZATION, format!("Bearer {}", token));
            }

            let req = req_builder.body(boxed_body)
                .map_err(|e| StreamError::Runtime(format!("Failed to build WHEP PATCH request: {}", e)))?;

            tracing::debug!("[WhepClient] PATCH to {}", session_url);

            let response = tokio::time::timeout(
                std::time::Duration::from_millis(self.config.timeout_ms),
                self.http_client.request(req),
            )
            .await
            .map_err(|_| StreamError::Runtime(format!("WHEP PATCH timed out after {}ms", self.config.timeout_ms)))?
            .map_err(|e| StreamError::Runtime(format!("WHEP PATCH request failed: {}", e)))?;

            let status = response.status();

            match status {
                StatusCode::NO_CONTENT => {
                    tracing::debug!("[WhepClient] ICE candidates sent successfully (204)");
                    Ok(())
                }
                StatusCode::OK => {
                    // ICE restart response (200) - server returns new credentials
                    tracing::info!("[WhepClient] ICE restart accepted (200)");
                    // TODO: Parse response body for new ICE credentials if needed
                    Ok(())
                }
                _ => {
                    let body_bytes = BodyExt::collect(response.into_body())
                        .await
                        .ok()
                        .and_then(|b| String::from_utf8(b.to_bytes().to_vec()).ok())
                        .unwrap_or_else(|| format!("HTTP {}", status));

                    Err(StreamError::Runtime(format!(
                        "WHEP PATCH failed: {}",
                        body_bytes
                    )))
                }
            }
        })
    }

    /// DELETE session to WHEP server (graceful shutdown)
    ///
    /// WHEP Spec: Client terminates session by DELETEing session URL.
    /// Server responds with 200 OK and closes ICE/DTLS sessions.
    pub fn terminate(&self) -> Result<()> {
        use hyper::{Request, header};
        use http_body_util::Empty;

        let session_url = match &self.session_url {
            Some(url) => url,
            None => {
                tracing::debug!("[WhepClient] No WHEP session to terminate");
                return Ok(());
            }
        };

        self._runtime.block_on(async {
            use http_body_util::BodyExt;
            let body = Empty::<bytes::Bytes>::new();
            let boxed_body = body.map_err(|never| match never {}).boxed();

            let mut req_builder = Request::builder()
                .method("DELETE")
                .uri(session_url);

            if let Some(token) = &self.config.auth_token {
                req_builder = req_builder.header(header::AUTHORIZATION, format!("Bearer {}", token));
            }

            let req = req_builder.body(boxed_body)
                .map_err(|e| StreamError::Runtime(format!("Failed to build WHEP DELETE request: {}", e)))?;

            tracing::debug!("[WhepClient] DELETE to {}", session_url);

            let response = tokio::time::timeout(
                std::time::Duration::from_millis(self.config.timeout_ms),
                self.http_client.request(req),
            )
            .await
            .map_err(|_| StreamError::Runtime(format!("WHEP DELETE timed out after {}ms", self.config.timeout_ms)))?
            .map_err(|e| StreamError::Runtime(format!("WHEP DELETE request failed: {}", e)))?;

            if response.status().is_success() {
                tracing::info!("[WhepClient] Session terminated: {}", session_url);
                Ok(())
            } else {
                // Non-fatal - session will timeout server-side
                tracing::warn!(
                    "[WhepClient] DELETE failed (status {}), session may still exist server-side",
                    response.status()
                );
                Ok(())
            }
        })
    }

    /// Get the session URL (if session has been created)
    pub fn session_url(&self) -> Option<&str> {
        self.session_url.as_deref()
    }

    /// Get the session ETag (if present)
    pub fn session_etag(&self) -> Option<&str> {
        self.session_etag.as_deref()
    }
}
