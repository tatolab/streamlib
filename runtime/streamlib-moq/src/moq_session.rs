// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! MoQ (Media over QUIC) session — QUIC/WebTransport client + publish/subscribe state.

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, LazyLock};
use std::time::Duration;
use streamlib_error::{Error, Result};

/// Default MoQ relay (Cloudflare draft-14).
pub const DEFAULT_MOQ_RELAY_URL: &str = "https://draft-14.cloudflare.mediaoverquic.com";

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

/// Configuration for connecting to a MoQ relay.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MoqRelayConfig {
    /// MoQ relay endpoint URL.
    pub relay_endpoint_url: String,
    /// Broadcast namespace path. Case-sensitive, no trailing slash.
    pub broadcast_path: String,
    /// Disable TLS certificate verification (for development only).
    pub tls_disable_verify: bool,
    /// Connection timeout in milliseconds.
    pub timeout_ms: u64,
}

impl Default for MoqRelayConfig {
    fn default() -> Self {
        Self {
            relay_endpoint_url: DEFAULT_MOQ_RELAY_URL.to_string(),
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
            .map_err(|e| Error::Configuration(format!("Invalid MoQ relay URL '{raw}': {e}")))
    }
}

/// Create a WebTransport client with QUIC keep-alive to prevent relay idle timeouts.
///
/// Bypasses `ClientBuilder` to configure [`quinn::TransportConfig`] directly,
/// setting a 4-second keep-alive interval (< Cloudflare's ~10-15s idle timeout).
fn create_webtransport_client(tls_disable_verify: bool) -> Result<web_transport::quinn::Client> {
    let provider = web_transport::quinn::crypto::default_provider();

    let crypto = if tls_disable_verify {
        rustls::ClientConfig::builder_with_provider(provider.clone())
            .with_protocol_versions(&[&rustls::version::TLS13])
            .map_err(|e| Error::Runtime(format!("TLS config failed: {e}")))?
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoTlsCertificateVerification(provider)))
            .with_no_client_auth()
    } else {
        let mut roots = rustls::RootCertStore::empty();
        let native_certs = rustls_native_certs::load_native_certs();
        for cert in native_certs.certs {
            roots
                .add(cert)
                .map_err(|e| Error::Runtime(format!("Failed to add root cert: {e}")))?;
        }
        rustls::ClientConfig::builder_with_provider(provider)
            .with_protocol_versions(&[&rustls::version::TLS13])
            .map_err(|e| Error::Runtime(format!("TLS config failed: {e}")))?
            .with_root_certificates(roots)
            .with_no_client_auth()
    };

    let mut crypto = crypto;
    crypto.alpn_protocols = vec![b"h3".to_vec()];

    let quic_client_config = quinn::crypto::rustls::QuicClientConfig::try_from(crypto)
        .map_err(|e| Error::Runtime(format!("QUIC client config failed: {e}")))?;

    let mut client_config = quinn::ClientConfig::new(Arc::new(quic_client_config));

    let mut transport = quinn::TransportConfig::default();
    transport.keep_alive_interval(Some(Duration::from_secs(4)));
    client_config.transport_config(Arc::new(transport));

    let endpoint = quinn::Endpoint::client("[::]:0".parse().unwrap())
        .map_err(|e| Error::Runtime(format!("QUIC endpoint creation failed: {e}")))?;

    tracing::info!(
        keep_alive_secs = 4,
        "QUIC transport configured with keep-alive"
    );

    Ok(web_transport::quinn::Client::new(endpoint, client_config))
}

/// Publishes data to a MoQ relay via moq-transport.
pub struct MoqPublishSession {
    _config: MoqRelayConfig,
    tracks_writer: moq_transport::serve::TracksWriter,
    track_subgroup_writers: HashMap<String, moq_transport::serve::SubgroupsWriter>,
    /// Active SubgroupWriter per track — reused across frames within a GOP.
    /// A new subgroup is created on keyframe; P-frames reuse the existing one.
    active_subgroup_writers: HashMap<String, moq_transport::serve::SubgroupWriter>,
    _tracks_request: moq_transport::serve::TracksRequest,
    _session_task: tokio::task::JoinHandle<()>,
    _announce_task: tokio::task::JoinHandle<()>,
}

impl MoqPublishSession {
    pub async fn connect(config: MoqRelayConfig) -> Result<Self> {
        let url = config.full_url()?;
        let client = create_webtransport_client(config.tls_disable_verify)?;

        let wt_session = client
            .connect(url)
            .await
            .map_err(|e| Error::Runtime(format!("MoQ WebTransport connect failed: {e}")))?;

        let wt_session: web_transport::Session = wt_session.into();

        let (session, mut publisher, _subscriber) = moq_transport::session::Session::connect(
            wt_session,
            None,
            moq_transport::session::Transport::WebTransport,
        )
        .await
        .map_err(|e| Error::Runtime(format!("MoQ session connect failed: {e}")))?;

        let session_task = tokio::spawn(async move {
            if let Err(e) = session.run().await {
                if !e.is_graceful_close() {
                    tracing::warn!(%e, "MoQ publish session ended");
                }
            }
        });

        // Namespace must be a non-empty broadcast name. Publisher and
        // subscriber must use the exact same namespace.
        let namespace =
            moq_transport::coding::TrackNamespace::from_utf8_path(&config.broadcast_path);
        let (tracks_writer, tracks_request, tracks_reader) =
            moq_transport::serve::Tracks::new(namespace).produce();

        // Spawn announce loop — serves incoming subscriptions from the relay.
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

    /// Publish a frame to a track.
    ///
    /// A new subgroup is started on keyframe (or first frame); P-frames
    /// reuse the existing one so a GOP shares one subgroup. Subscribers
    /// stream objects in real-time without waiting for the GOP to end.
    pub fn publish_frame(
        &mut self,
        track_name: &str,
        payload: &[u8],
        is_keyframe: bool,
    ) -> Result<()> {
        let needs_new_subgroup =
            is_keyframe || !self.active_subgroup_writers.contains_key(track_name);

        if needs_new_subgroup {
            let subgroups_writer = self.ensure_track_subgroups_writer(track_name)?;
            let subgroup = subgroups_writer
                .append(0)
                .map_err(|e| Error::Runtime(format!("Failed to create MoQ subgroup: {e}")))?;
            self.active_subgroup_writers
                .insert(track_name.to_string(), subgroup);
        }

        let subgroup = self.active_subgroup_writers.get_mut(track_name).unwrap();
        let write_result = subgroup.write(bytes::Bytes::copy_from_slice(payload));
        if let Err(e) = write_result {
            self.active_subgroup_writers.remove(track_name);
            return Err(Error::Runtime(format!("Failed to write MoQ frame: {e}")));
        }

        Ok(())
    }

    fn ensure_track_subgroups_writer(
        &mut self,
        track_name: &str,
    ) -> Result<&mut moq_transport::serve::SubgroupsWriter> {
        if !self.track_subgroup_writers.contains_key(track_name) {
            let track_writer = self.tracks_writer.create(track_name).ok_or_else(|| {
                Error::Runtime(format!(
                    "Failed to create MoQ track '{track_name}' (all readers dropped)"
                ))
            })?;

            let subgroups_writer = track_writer.subgroups().map_err(|e| {
                Error::Runtime(format!(
                    "Failed to enter subgroups mode for track '{track_name}': {e}"
                ))
            })?;

            self.track_subgroup_writers
                .insert(track_name.to_string(), subgroups_writer);
        }

        Ok(self.track_subgroup_writers.get_mut(track_name).unwrap())
    }
}

/// Subscribes to data from a MoQ relay via moq-transport.
pub struct MoqSubscribeSession {
    _config: MoqRelayConfig,
    subscriber: moq_transport::session::Subscriber,
    namespace: moq_transport::coding::TrackNamespace,
    /// Tokio handle captured during connect() for spawning from non-tokio threads.
    tokio_handle: tokio::runtime::Handle,
    _session_task: tokio::task::JoinHandle<()>,
}

impl MoqSubscribeSession {
    pub async fn connect(config: MoqRelayConfig) -> Result<Self> {
        let url = config.full_url()?;
        let client = create_webtransport_client(config.tls_disable_verify)?;

        let wt_session = client
            .connect(url)
            .await
            .map_err(|e| Error::Runtime(format!("MoQ WebTransport connect failed: {e}")))?;

        let wt_session: web_transport::Session = wt_session.into();

        let (session, _publisher, subscriber) = moq_transport::session::Session::connect(
            wt_session,
            None,
            moq_transport::session::Transport::WebTransport,
        )
        .await
        .map_err(|e| Error::Runtime(format!("MoQ session connect failed: {e}")))?;

        let session_task = tokio::spawn(async move {
            if let Err(e) = session.run().await {
                if !e.is_graceful_close() {
                    tracing::warn!(%e, "MoQ subscribe session ended");
                }
            }
        });

        let namespace =
            moq_transport::coding::TrackNamespace::from_utf8_path(&config.broadcast_path);

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
    pub fn subscribe_track(&self, track_name: &str) -> Result<MoqTrackReader> {
        let (writer, reader) =
            moq_transport::serve::Track::new(self.namespace.clone(), track_name.to_string())
                .produce();

        let mut subscriber = self.subscriber.clone();
        let track_name_owned = track_name.to_string();
        let handle =
            tokio::runtime::Handle::try_current().unwrap_or_else(|_| self.tokio_handle.clone());
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

/// Reads frames from a subscribed MoQ track.
pub struct MoqTrackReader {
    track_reader: moq_transport::serve::TrackReader,
    subgroups_reader: Option<moq_transport::serve::SubgroupsReader>,
}

impl MoqTrackReader {
    /// Wait for the next subgroup. Returns `None` when the track ends.
    pub async fn next_subgroup(&mut self) -> Result<Option<MoqSubgroupReader>> {
        if self.subgroups_reader.is_none() {
            let mode = self
                .track_reader
                .mode()
                .await
                .map_err(|e| Error::Runtime(format!("MoQ track mode error: {e}")))?;

            match mode {
                moq_transport::serve::TrackReaderMode::Subgroups(reader) => {
                    self.subgroups_reader = Some(reader);
                }
                _ => {
                    return Err(Error::Runtime(
                        "Unexpected MoQ track mode (expected subgroups)".into(),
                    ));
                }
            }
        }

        let reader = self.subgroups_reader.as_mut().unwrap();
        match reader.next().await {
            Ok(Some(subgroup)) => Ok(Some(MoqSubgroupReader { inner: subgroup })),
            Ok(None) => Ok(None),
            Err(e) => Err(Error::Runtime(format!("MoQ subgroup read error: {e}"))),
        }
    }
}

/// Reads frames from a single MoQ subgroup.
pub struct MoqSubgroupReader {
    inner: moq_transport::serve::SubgroupReader,
}

impl MoqSubgroupReader {
    /// Read the next frame. Returns `None` when the subgroup ends.
    pub async fn read_frame(&mut self) -> Result<Option<bytes::Bytes>> {
        self.inner
            .read_next()
            .await
            .map_err(|e| Error::Runtime(format!("MoQ frame read error: {e}")))
    }
}

/// Per-runtime publish + subscribe sessions, lazily created on first use.
///
/// One QUIC connection for publishing (multiple tracks), one for subscribing.
#[derive(Clone)]
pub struct SharedMoqSessions {
    relay_url: String,
    broadcast_path: String,
    publish_session: Arc<tokio::sync::OnceCell<Arc<Mutex<MoqPublishSession>>>>,
    subscribe_session: Arc<tokio::sync::OnceCell<Arc<MoqSubscribeSession>>>,
    /// Track names currently being published (for catalog discovery).
    published_tracks: Arc<Mutex<Vec<String>>>,
}

impl SharedMoqSessions {
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

    pub async fn get_publish_session(&self) -> Result<Arc<Mutex<MoqPublishSession>>> {
        let session = self
            .publish_session
            .get_or_try_init(|| async {
                let config = MoqRelayConfig {
                    relay_endpoint_url: self.relay_url.clone(),
                    broadcast_path: self.broadcast_path.clone(),
                    tls_disable_verify: false,
                    timeout_ms: 10000,
                };
                let session = MoqPublishSession::connect(config).await?;
                Ok::<_, Error>(Arc::new(Mutex::new(session)))
            })
            .await?;
        Ok(Arc::clone(session))
    }

    pub async fn get_subscribe_session(&self) -> Result<Arc<MoqSubscribeSession>> {
        let session = self
            .subscribe_session
            .get_or_try_init(|| async {
                let config = MoqRelayConfig {
                    relay_endpoint_url: self.relay_url.clone(),
                    broadcast_path: self.broadcast_path.clone(),
                    tls_disable_verify: false,
                    timeout_ms: 10000,
                };
                let session = MoqSubscribeSession::connect(config).await?;
                Ok::<_, Error>(Arc::new(session))
            })
            .await?;
        Ok(Arc::clone(session))
    }

    pub fn register_published_track(&self, track_name: &str) {
        let mut tracks = self.published_tracks.lock();
        if !tracks.contains(&track_name.to_string()) {
            tracks.push(track_name.to_string());
        }
    }

    pub fn published_track_names(&self) -> Vec<String> {
        self.published_tracks.lock().clone()
    }

    pub fn broadcast_path(&self) -> &str {
        &self.broadcast_path
    }
}

/// Process-global registry keyed by `RuntimeContext::runtime_id()`.
///
/// MoQ processors share one publish session per runtime so all tracks
/// announce under one namespace. The engine no longer holds this state
/// (`@tatolab/moq` is read-only against the engine substrate), so the
/// package keeps its own registry keyed by the runtime's public id.
///
/// Entries persist for the process's lifetime — runtime teardown does
/// not reclaim them. This matches the typical one-runtime-per-process
/// shape; multi-runtime apps that recycle runtimes will accumulate
/// stale entries until process exit.
static RUNTIME_SESSIONS: LazyLock<Mutex<HashMap<String, SharedMoqSessions>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Get or create the [`SharedMoqSessions`] for a runtime, keyed by
/// `RuntimeContext::runtime_id()`.
pub fn sessions_for_runtime(runtime_id: &str) -> SharedMoqSessions {
    let mut map = RUNTIME_SESSIONS.lock();
    map.entry(runtime_id.to_string())
        .or_insert_with(|| SharedMoqSessions::new(runtime_id))
        .clone()
}

/// Look up the [`SharedMoqSessions`] for a runtime without creating one.
///
/// Returns `None` when no MoQ processor has touched this runtime yet.
/// Used by read-only consumers (e.g. the API server's catalog endpoint).
pub fn try_sessions_for_runtime(runtime_id: &str) -> Option<SharedMoqSessions> {
    RUNTIME_SESSIONS.lock().get(runtime_id).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Process-global registry — every test uses a unique runtime id so
    // entries don't collide across tests in the same process.
    fn unique_runtime_id(suffix: &str) -> String {
        format!("test-{}-{}", suffix, uuid_like_counter())
    }

    fn uuid_like_counter() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        COUNTER.fetch_add(1, Ordering::SeqCst)
    }

    #[test]
    fn sessions_for_runtime_returns_same_record_on_repeat_calls() {
        let id = unique_runtime_id("same-record");
        let a = sessions_for_runtime(&id);
        let b = sessions_for_runtime(&id);

        // Same broadcast path — namespacing is deterministic from runtime id.
        assert_eq!(a.broadcast_path(), b.broadcast_path());

        // Track registration on one clone is visible on the other — the
        // two handles are the same logical registry.
        a.register_published_track("video");
        assert!(b.published_track_names().contains(&"video".to_string()));
    }

    #[test]
    fn sessions_for_runtime_isolates_distinct_runtimes() {
        let id_a = unique_runtime_id("isolated-a");
        let id_b = unique_runtime_id("isolated-b");
        let a = sessions_for_runtime(&id_a);
        let b = sessions_for_runtime(&id_b);

        assert_ne!(a.broadcast_path(), b.broadcast_path());

        a.register_published_track("video");
        // Registering on a does not bleed into b.
        assert!(b.published_track_names().is_empty());
    }

    #[test]
    fn try_sessions_for_runtime_returns_none_before_first_create() {
        let id = unique_runtime_id("never-touched");
        assert!(try_sessions_for_runtime(&id).is_none());
    }

    #[test]
    fn try_sessions_for_runtime_finds_existing_record() {
        let id = unique_runtime_id("found");
        let created = sessions_for_runtime(&id);
        created.register_published_track("audio");

        let looked_up =
            try_sessions_for_runtime(&id).expect("sessions_for_runtime created an entry");
        assert_eq!(looked_up.broadcast_path(), created.broadcast_path());
        assert!(
            looked_up
                .published_track_names()
                .contains(&"audio".to_string())
        );
    }

    #[test]
    fn shared_moq_sessions_broadcast_path_uses_runtime_id() {
        let s = SharedMoqSessions::new("my-runtime");
        assert_eq!(s.broadcast_path(), "streamlib/my-runtime");
    }

    #[test]
    fn shared_moq_sessions_register_published_track_dedupes() {
        let s = SharedMoqSessions::new("dedupe-runtime");
        s.register_published_track("video");
        s.register_published_track("video");
        s.register_published_track("audio");
        assert_eq!(s.published_track_names(), vec!["video", "audio"]);
    }
}
