// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// MoQ (Media over QUIC) Session
//
// Manages a QUIC/WebTransport connection to a MoQ relay for publish/subscribe.
// Uses moq-transport (cloudflare/moq-rs) for the MoQ protocol and
// web-transport-quinn for the underlying QUIC/WebTransport connection.

use crate::core::{Result, StreamError};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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

/// Create a WebTransport client based on TLS configuration.
///
/// Uses `web_transport::quinn` (re-exported web-transport-quinn) to ensure
/// type compatibility with `web_transport::Session`.
fn create_webtransport_client(
    tls_disable_verify: bool,
) -> Result<web_transport::quinn::Client> {
    if tls_disable_verify {
        web_transport::quinn::ClientBuilder::new()
            .dangerous()
            .with_no_certificate_verification()
            .map_err(|e| {
                StreamError::Runtime(format!(
                    "MoQ WebTransport client init (insecure) failed: {e}"
                ))
            })
    } else {
        web_transport::quinn::ClientBuilder::new()
            .with_system_roots()
            .map_err(|e| {
                StreamError::Runtime(format!("MoQ WebTransport client init failed: {e}"))
            })
    }
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

        // Create tracks namespace from broadcast path
        let namespace = moq_transport::coding::TrackNamespace::from_utf8_path(
            &config.broadcast_path,
        );
        let (tracks_writer, _tracks_request, tracks_reader) =
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
            _session_task: session_task,
            _announce_task: announce_task,
        })
    }

    /// Publish a frame (opaque bytes) to a track.
    ///
    /// - `track_name`: MoQ track name (typically the schema_name from FramePayload).
    /// - `payload`: Raw bytes to publish.
    /// - `is_keyframe`: If true, starts a new group (MoQ Group = GOP boundary).
    ///   In moq-transport, each `append()` starts a new group regardless, but
    ///   the keyframe flag is retained for semantic clarity.
    pub fn publish_frame(
        &mut self,
        track_name: &str,
        payload: &[u8],
        _is_keyframe: bool,
    ) -> Result<()> {
        let subgroups_writer = self.ensure_track_subgroups_writer(track_name)?;

        let mut subgroup = subgroups_writer.append(0).map_err(|e| {
            StreamError::Runtime(format!("Failed to create MoQ subgroup: {e}"))
        })?;

        subgroup
            .write(bytes::Bytes::copy_from_slice(payload))
            .map_err(|e| {
                StreamError::Runtime(format!("Failed to write MoQ frame: {e}"))
            })?;

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
