// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// MoQ (Media over QUIC) Session
//
// Manages a QUIC/WebTransport connection to a MoQ relay for publish/subscribe.
// Uses moq-transport (cloudflare/moq-rs) for the MoQ protocol and
// web-transport-quinn for the underlying QUIC/WebTransport connection.

use crate::core::{Result, StreamError};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

/// Accepts any TLS certificate without verification (development only).
#[derive(Debug)]
struct NoTlsCertificateVerification(Arc<rustls::crypto::CryptoProvider>);

impl rustls::client::danger::ServerCertVerifier for NoTlsCertificateVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> std::result::Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

// ============================================================================
// MOQ SESSION CONFIGURATION
// ============================================================================

/// Configuration for connecting to a MoQ relay.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MoqRelayConfig {
    /// MoQ relay endpoint URL (e.g., "https://relay.quic.video").
    pub relay_endpoint_url: String,
    /// Broadcast namespace path (e.g., "my-broadcast-name"). Case-sensitive, no trailing slash.
    pub broadcast_path: String,
    /// Disable TLS certificate verification (for development only).
    pub tls_disable_verify: bool,
    /// Connection timeout in milliseconds.
    pub timeout_ms: u64,
}

impl Default for MoqRelayConfig {
    fn default() -> Self {
        Self {
            relay_endpoint_url: "https://draft-14.cloudflare.mediaoverquic.com".to_string(),
            broadcast_path: String::new(),
            tls_disable_verify: false,
            timeout_ms: 10000,
        }
    }
}

impl MoqRelayConfig {
    fn full_url(&self) -> Result<url::Url> {
        let raw = format!(
            "{}/{}",
            self.relay_endpoint_url.trim_end_matches('/'),
            self.broadcast_path.trim_start_matches('/')
        );
        url::Url::parse(&raw)
            .map_err(|e| StreamError::Configuration(format!("Invalid MoQ relay URL '{raw}': {e}")))
    }
}

/// Create a WebTransport client with QUIC keep-alive to prevent relay idle timeouts.
///
/// Bypasses `ClientBuilder` to configure [`quinn::TransportConfig`] directly,
/// setting a 4-second keep-alive interval (< Cloudflare's ~10-15s idle timeout).
fn create_webtransport_client(
    tls_disable_verify: bool,
) -> Result<web_transport::quinn::Client> {
    let provider = web_transport::quinn::crypto::default_provider();

    let crypto = if tls_disable_verify {
        rustls::ClientConfig::builder_with_provider(provider.clone())
            .with_protocol_versions(&[&rustls::version::TLS13])
            .map_err(|e| {
                StreamError::Runtime(format!("TLS config failed: {e}"))
            })?
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(
                NoTlsCertificateVerification(provider),
            ))
            .with_no_client_auth()
    } else {
        let mut roots = rustls::RootCertStore::empty();
        let native_certs = rustls_native_certs::load_native_certs();
        for cert in native_certs.certs {
            roots.add(cert).map_err(|e| {
                StreamError::Runtime(format!("Failed to add root cert: {e}"))
            })?;
        }
        rustls::ClientConfig::builder_with_provider(provider)
            .with_protocol_versions(&[&rustls::version::TLS13])
            .map_err(|e| {
                StreamError::Runtime(format!("TLS config failed: {e}"))
            })?
            .with_root_certificates(roots)
            .with_no_client_auth()
    };

    let mut crypto = crypto;
    crypto.alpn_protocols = vec![b"h3".to_vec()];

    let quic_client_config =
        quinn::crypto::rustls::QuicClientConfig::try_from(crypto).map_err(|e| {
            StreamError::Runtime(format!("QUIC client config failed: {e}"))
        })?;

    let mut client_config = quinn::ClientConfig::new(Arc::new(quic_client_config));

    let mut transport = quinn::TransportConfig::default();
    transport.keep_alive_interval(Some(Duration::from_secs(4)));
    client_config.transport_config(Arc::new(transport));

    let endpoint = quinn::Endpoint::client("[::]:0".parse().unwrap()).map_err(|e| {
        StreamError::Runtime(format!("QUIC endpoint creation failed: {e}"))
    })?;

    tracing::info!(keep_alive_secs = 4, "QUIC transport configured with keep-alive");

    Ok(web_transport::quinn::Client::new(endpoint, client_config))
}

// ============================================================================
// MOQ PUBLISH SESSION
// ============================================================================

/// Publishes data to a MoQ relay via moq-transport.
///
/// Wraps a WebTransport connection + MoQ session with track management.
/// Tracks are created on-demand and served to subscribers via the relay.
pub struct MoqPublishSession {
    _config: MoqRelayConfig,
    tracks_writer: moq_transport::serve::TracksWriter,
    track_subgroup_writers: HashMap<String, moq_transport::serve::SubgroupsWriter>,
    /// Active SubgroupWriter per track — reused across frames within a GOP.
    /// A new subgroup is created on keyframe; P-frames reuse the existing one.
    active_subgroup_writers: HashMap<String, moq_transport::serve::SubgroupWriter>,
    /// Keeps the TracksRequest alive so announce can fulfill dynamic subscriptions.
    _tracks_request: moq_transport::serve::TracksRequest,
    /// Keeps the MoQ session event loop alive.
    _session_task: tokio::task::JoinHandle<()>,
    /// Keeps the announce (serve subscriptions) loop alive.
    _announce_task: tokio::task::JoinHandle<()>,
}

impl MoqPublishSession {
    /// Connect to a MoQ relay and prepare to publish a broadcast.
    pub async fn connect(config: MoqRelayConfig) -> Result<Self> {
        let url = config.full_url()?;
        let client = create_webtransport_client(config.tls_disable_verify)?;

        let wt_session = client.connect(url).await.map_err(|e| {
            StreamError::Runtime(format!("MoQ WebTransport connect failed: {e}"))
        })?;

        // Convert web_transport_quinn::Session → web_transport::Session for moq-transport
        let wt_session: web_transport::Session = wt_session.into();

        let (session, mut publisher, _subscriber) =
            moq_transport::session::Session::connect(
                wt_session,
                None,
                moq_transport::session::Transport::WebTransport,
            )
            .await
            .map_err(|e| {
                StreamError::Runtime(format!("MoQ session connect failed: {e}"))
            })?;

        // Run session event loop in background
        let session_task = tokio::spawn(async move {
            if let Err(e) = session.run().await {
                if !e.is_graceful_close() {
                    tracing::warn!(%e, "MoQ publish session ended");
                }
            }
        });

        // Namespace must be a non-empty broadcast name. The URL path is the
        // scope (routing bucket) and the namespace lives inside it.
        // Publisher and subscriber must use the exact same namespace.
        let namespace = moq_transport::coding::TrackNamespace::from_utf8_path(
            &config.broadcast_path,
        );
        let (tracks_writer, tracks_request, tracks_reader) =
            moq_transport::serve::Tracks::new(namespace).produce();

        // Spawn announce loop — serves incoming subscriptions from the relay
        let announce_task = tokio::spawn(async move {
            if let Err(e) = publisher.announce(tracks_reader).await {
                if !matches!(
                    e,
                    moq_transport::session::SessionError::Serve(
                        moq_transport::serve::ServeError::Done
                    )
                ) {
                    tracing::warn!(%e, "MoQ announce ended");
                }
            }
        });

        tracing::info!(
            broadcast = %config.broadcast_path,
            "MoQ publish session connected"
        );

        Ok(Self {
            _config: config,
            tracks_writer,
            track_subgroup_writers: HashMap::new(),
            active_subgroup_writers: HashMap::new(),
            _tracks_request: tracks_request,
            _session_task: session_task,
            _announce_task: announce_task,
        })
    }

    /// Publish a frame (opaque bytes) to a track.
    ///
    /// - `track_name`: MoQ track name (typically the schema_name from FramePayload).
    /// - `payload`: Raw bytes to publish.
    /// - `is_keyframe`: If true, starts a new subgroup (MoQ Group = GOP boundary).
    ///   P-frames reuse the active subgroup so all frames in a GOP share one
    ///   subgroup, preventing the subscriber from missing frames.
    pub fn publish_frame(
        &mut self,
        track_name: &str,
        payload: &[u8],
        is_keyframe: bool,
    ) -> Result<()> {
        // Single subgroup per track: all frames (IDR + P) go into one subgroup.
        // The subscriber reads all objects from this subgroup in order, receiving
        // IDR frames naturally as they appear in the stream.
        if !self.active_subgroup_writers.contains_key(track_name) {
            let subgroups_writer = self.ensure_track_subgroups_writer(track_name)?;
            let subgroup = subgroups_writer.append(0).map_err(|e| {
                StreamError::Runtime(format!("Failed to create MoQ subgroup: {e}"))
            })?;
            self.active_subgroup_writers
                .insert(track_name.to_string(), subgroup);
        }

        let subgroup = self.active_subgroup_writers.get_mut(track_name).unwrap();
        let write_result = subgroup.write(bytes::Bytes::copy_from_slice(payload));
        if let Err(e) = write_result {
            // Remove the stale writer so next call creates a new one
            self.active_subgroup_writers.remove(track_name);
            return Err(StreamError::Runtime(format!(
                "Failed to write MoQ frame: {e}"
            )));
        }

        Ok(())
    }

    fn ensure_track_subgroups_writer(
        &mut self,
        track_name: &str,
    ) -> Result<&mut moq_transport::serve::SubgroupsWriter> {
        if !self.track_subgroup_writers.contains_key(track_name) {
            let track_writer = self.tracks_writer.create(track_name).ok_or_else(|| {
                StreamError::Runtime(format!(
                    "Failed to create MoQ track '{track_name}' (all readers dropped)"
                ))
            })?;

            let subgroups_writer =
                track_writer.subgroups().map_err(|e| {
                    StreamError::Runtime(format!(
                        "Failed to enter subgroups mode for track '{track_name}': {e}"
                    ))
                })?;

            self.track_subgroup_writers
                .insert(track_name.to_string(), subgroups_writer);
        }

        Ok(self.track_subgroup_writers.get_mut(track_name).unwrap())
    }
}

// ============================================================================
// MOQ SUBSCRIBE SESSION
// ============================================================================

/// Subscribes to data from a MoQ relay via moq-transport.
///
/// Creates subscriptions to individual tracks and returns readers
/// that yield frames as they arrive from the relay.
pub struct MoqSubscribeSession {
    _config: MoqRelayConfig,
    subscriber: moq_transport::session::Subscriber,
    namespace: moq_transport::coding::TrackNamespace,
    /// Tokio handle captured during connect() for spawning from non-tokio threads.
    tokio_handle: tokio::runtime::Handle,
    /// Keeps the MoQ session event loop alive.
    _session_task: tokio::task::JoinHandle<()>,
}

impl MoqSubscribeSession {
    /// Connect to a MoQ relay and prepare to subscribe.
    pub async fn connect(config: MoqRelayConfig) -> Result<Self> {
        let url = config.full_url()?;
        let client = create_webtransport_client(config.tls_disable_verify)?;

        let wt_session = client.connect(url).await.map_err(|e| {
            StreamError::Runtime(format!("MoQ WebTransport connect failed: {e}"))
        })?;

        let wt_session: web_transport::Session = wt_session.into();

        let (session, _publisher, subscriber) =
            moq_transport::session::Session::connect(
                wt_session,
                None,
                moq_transport::session::Transport::WebTransport,
            )
            .await
            .map_err(|e| {
                StreamError::Runtime(format!("MoQ session connect failed: {e}"))
            })?;

        // Run session event loop in background
        let session_task = tokio::spawn(async move {
            if let Err(e) = session.run().await {
                if !e.is_graceful_close() {
                    tracing::warn!(%e, "MoQ subscribe session ended");
                }
            }
        });

        // Namespace must match exactly what the publisher announced.
        let namespace = moq_transport::coding::TrackNamespace::from_utf8_path(
            &config.broadcast_path,
        );

        tracing::info!(
            broadcast = %config.broadcast_path,
            "MoQ subscribe session connected"
        );

        Ok(Self {
            _config: config,
            subscriber,
            namespace,
            tokio_handle: tokio::runtime::Handle::current(),
            _session_task: session_task,
        })
    }

    /// Subscribe to a specific track within the broadcast.
    ///
    /// Returns a [`MoqTrackReader`] that yields frames from the track.
    /// The subscription runs in the background — data flows as long as
    /// the session and reader are alive.
    pub fn subscribe_track(
        &self,
        track_name: &str,
    ) -> Result<MoqTrackReader> {
        let (writer, reader) = moq_transport::serve::Track::new(
            self.namespace.clone(),
            track_name.to_string(),
        )
        .produce();

        // Spawn the subscribe task — sends SUBSCRIBE to the relay and blocks
        // until the subscription ends. Data is routed to the TrackWriter.
        // Uses tokio::Handle::current() so this works from both tokio tasks
        // and dedicated processor threads (which have a tokio runtime available
        // via RuntimeContext).
        let mut subscriber = self.subscriber.clone();
        let track_name_owned = track_name.to_string();
        let handle = tokio::runtime::Handle::try_current()
            .unwrap_or_else(|_| self.tokio_handle.clone());
        let _subscribe_task = handle.spawn(async move {
            if let Err(e) = subscriber.subscribe(writer).await {
                tracing::debug!(
                    track = %track_name_owned,
                    %e,
                    "MoQ track subscription ended"
                );
            }
        });

        tracing::info!(track = track_name, "Subscribed to MoQ track");

        Ok(MoqTrackReader {
            track_reader: reader,
            subgroups_reader: None,
        })
    }
}

// ============================================================================
// MOQ TRACK READER (subscribe-side)
// ============================================================================

/// Reads frames from a subscribed MoQ track.
///
/// Wraps the moq-transport subgroup reading pattern into a simple
/// subgroup → frame iteration API similar to moq-lite's TrackConsumer.
pub struct MoqTrackReader {
    track_reader: moq_transport::serve::TrackReader,
    subgroups_reader: Option<moq_transport::serve::SubgroupsReader>,
}

impl MoqTrackReader {
    /// Wait for the next subgroup (analogous to moq-lite's `next_group`).
    ///
    /// On the first call, waits for the track mode to be set by the publisher.
    /// Returns `None` when the track ends.
    pub async fn next_subgroup(&mut self) -> Result<Option<MoqSubgroupReader>> {
        // Lazily initialize the subgroups reader on first call
        if self.subgroups_reader.is_none() {
            let mode = self.track_reader.mode().await.map_err(|e| {
                StreamError::Runtime(format!("MoQ track mode error: {e}"))
            })?;

            match mode {
                moq_transport::serve::TrackReaderMode::Subgroups(reader) => {
                    self.subgroups_reader = Some(reader);
                }
                _ => {
                    return Err(StreamError::Runtime(
                        "Unexpected MoQ track mode (expected subgroups)".into(),
                    ));
                }
            }
        }

        let reader = self.subgroups_reader.as_mut().unwrap();
        match reader.next().await {
            Ok(Some(subgroup)) => Ok(Some(MoqSubgroupReader { inner: subgroup })),
            Ok(None) => Ok(None),
            Err(e) => Err(StreamError::Runtime(format!(
                "MoQ subgroup read error: {e}"
            ))),
        }
    }
}

/// Reads frames from a single MoQ subgroup.
pub struct MoqSubgroupReader {
    inner: moq_transport::serve::SubgroupReader,
}

impl MoqSubgroupReader {
    /// Read the next frame from this subgroup. Returns `None` when the subgroup ends.
    pub async fn read_frame(&mut self) -> Result<Option<bytes::Bytes>> {
        self.inner.read_next().await.map_err(|e| {
            StreamError::Runtime(format!("MoQ frame read error: {e}"))
        })
    }
}

// ============================================================================
// SHARED MOQ SESSIONS (runtime-managed)
// ============================================================================

/// Default MoQ relay (Cloudflare draft-14).
pub const DEFAULT_MOQ_RELAY_URL: &str = "https://draft-14.cloudflare.mediaoverquic.com";

/// Runtime-managed MoQ sessions shared by all MoQ processors.
///
/// One QUIC connection for publishing (multiple tracks), one for subscribing.
/// Sessions are created lazily on first use and shared via Arc.
#[derive(Clone)]
pub struct SharedMoqSessions {
    relay_url: String,
    broadcast_path: String,
    publish_session: Arc<tokio::sync::OnceCell<Arc<Mutex<MoqPublishSession>>>>,
    subscribe_session: Arc<tokio::sync::OnceCell<Arc<MoqSubscribeSession>>>,
    /// Track names currently being published (for catalog).
    published_tracks: Arc<Mutex<Vec<String>>>,
}

impl SharedMoqSessions {
    /// Create a new shared session holder for a runtime.
    pub fn new(runtime_id: &str) -> Self {
        let broadcast_path = format!("streamlib/{}", runtime_id);
        Self {
            relay_url: DEFAULT_MOQ_RELAY_URL.to_string(),
            broadcast_path,
            publish_session: Arc::new(tokio::sync::OnceCell::new()),
            subscribe_session: Arc::new(tokio::sync::OnceCell::new()),
            published_tracks: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Get or create the shared publish session.
    pub async fn get_publish_session(&self) -> Result<Arc<Mutex<MoqPublishSession>>> {
        let session = self.publish_session.get_or_try_init(|| async {
            let config = MoqRelayConfig {
                relay_endpoint_url: self.relay_url.clone(),
                broadcast_path: self.broadcast_path.clone(),
                tls_disable_verify: false,
                timeout_ms: 10000,
            };
            let session = MoqPublishSession::connect(config).await?;
            Ok::<_, StreamError>(Arc::new(Mutex::new(session)))
        }).await?;
        Ok(Arc::clone(session))
    }

    /// Get or create the shared subscribe session.
    pub async fn get_subscribe_session(&self) -> Result<Arc<MoqSubscribeSession>> {
        let session = self.subscribe_session.get_or_try_init(|| async {
            let config = MoqRelayConfig {
                relay_endpoint_url: self.relay_url.clone(),
                broadcast_path: self.broadcast_path.clone(),
                tls_disable_verify: false,
                timeout_ms: 10000,
            };
            let session = MoqSubscribeSession::connect(config).await?;
            Ok::<_, StreamError>(Arc::new(session))
        }).await?;
        Ok(Arc::clone(session))
    }

    /// Register a track name as published (for catalog).
    pub fn register_published_track(&self, track_name: &str) {
        let mut tracks = self.published_tracks.lock();
        if !tracks.contains(&track_name.to_string()) {
            tracks.push(track_name.to_string());
        }
    }

    /// Get all published track names.
    pub fn published_track_names(&self) -> Vec<String> {
        self.published_tracks.lock().clone()
    }

    /// Get the broadcast path (for logging/discovery).
    pub fn broadcast_path(&self) -> &str {
        &self.broadcast_path
    }
}
