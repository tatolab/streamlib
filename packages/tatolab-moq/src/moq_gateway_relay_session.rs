// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

//! A gateway's one session at its relay: the broadcast it publishes, with its
//! catalog and a track per declared track node, and the origin its receive
//! reads subscribe through.

use std::time::Instant;

use moq_net::{Timescale, Timestamp};

use crate::error::{MoqModuleError, Result};
use crate::moq_gateway_relay_settings::MoqGatewayRelayDial;

/// The dial socket on a host with no IPv6: an ephemeral IPv4 port.
const A_DIAL_SOCKET_ON_IPV4_ALONE: std::net::SocketAddr = std::net::SocketAddr::V4(
    std::net::SocketAddrV4::new(std::net::Ipv4Addr::UNSPECIFIED, 0),
);

/// One dialed session, publishing and subscribing over one connection.
pub struct MoqGatewayRelaySession {
    pub dialed: MoqGatewayRelayDial,
    pub broadcast: MoqGatewayBroadcastAtTheRelay,
    /// What the relay announces, for the receive reads.
    pub announced_broadcasts: moq_net::origin::Consumer,
    connection: moq_tokio::Connection,
    _published_origin: moq_net::origin::Producer,
    _subscribed_origin: moq_net::origin::Producer,
}

impl MoqGatewayRelaySession {
    /// Dial `dialed` and announce its broadcast with an empty catalog; the
    /// session redials by itself while it is held. Called inside the gateway's
    /// async runtime.
    pub fn dial(dialed: MoqGatewayRelayDial) -> Result<Self> {
        let mut configuration = moq_tokio::connect::Config::default();
        configuration.tls.fingerprint = dialed.certificate_fingerprints.clone();
        let client = match configuration
            .clone()
            .init(moq_tokio::quic::Config::default())
        {
            Ok(client) => client,
            Err(dual_stack_refusal) => {
                tracing::debug!(
                    "MoqGateway: no dual-stack dial socket binds ({dual_stack_refusal}), so the \
                     relay is dialed over IPv4"
                );
                configuration.bind = Some(A_DIAL_SOCKET_ON_IPV4_ALONE);
                configuration
                    .init(moq_tokio::quic::Config::default())
                    .map_err(|refusal| MoqModuleError::Relay {
                        what: format!(
                            "no MoQ client builds for {}: {refusal}",
                            dialed.relay_origin
                        ),
                    })?
            }
        };
        let published_origin = moq_tokio::origin::spawn();
        let subscribed_origin = moq_tokio::origin::spawn();
        let connection = client
            .with_publisher(&published_origin)
            .with_subscriber(subscribed_origin.clone())
            .with_reconnect(true)
            .connect(dialed.dial_url_carrying_the_credential.clone());
        let broadcast =
            MoqGatewayBroadcastAtTheRelay::announced_on(&published_origin, &dialed.broadcast_path)?;
        tracing::info!(
            "MoqGateway: dialing {} to publish `{}`",
            dialed.relay_origin,
            dialed.broadcast_path
        );
        Ok(Self {
            announced_broadcasts: subscribed_origin.consume(),
            dialed,
            broadcast,
            connection,
            _published_origin: published_origin,
            _subscribed_origin: subscribed_origin,
        })
    }

    /// Whether the session is connected to the relay now.
    pub fn is_connected(&self) -> bool {
        self.connection.connected()
    }

    /// Whether the credential the session dialed with is due to be replaced.
    pub fn its_credential_is_due_at(&self, now: Instant) -> bool {
        self.dialed.renew_by.is_some_and(|renew_by| now >= renew_by)
    }
}

/// The gateway's broadcast, with its catalog in both of hang's spellings and
/// the catalog last published on them.
pub struct MoqGatewayBroadcastAtTheRelay {
    broadcast: moq_net::broadcast::Producer,
    catalog: moq_json::snapshot::Producer<serde_json::Value>,
    compressed_catalog: moq_json::snapshot::Producer<serde_json::Value>,
    catalog_published_last: serde_json::Value,
    catalog_last_written_whole_at: Instant,
}

impl MoqGatewayBroadcastAtTheRelay {
    fn announced_on(origin: &moq_net::origin::Producer, broadcast_path: &str) -> Result<Self> {
        let created = (|| -> std::result::Result<Self, moq_net::Error> {
            let broadcast = origin.create_broadcast(broadcast_path)?;
            let catalog = moq_json::snapshot::Producer::new(
                broadcast.create_track(
                    hang::Catalog::DEFAULT_NAME,
                    hang::Catalog::default_track_info(),
                )?,
                moq_json::snapshot::Config::default(),
            );
            let mut compressed = moq_json::snapshot::Config::default();
            compressed.compression = moq_json::Compression::Deflate;
            let compressed_catalog = moq_json::snapshot::Producer::new(
                broadcast.create_track(
                    hang::Catalog::COMPRESSED_NAME,
                    hang::Catalog::default_track_info(),
                )?,
                compressed,
            );
            Ok(Self {
                broadcast,
                catalog,
                compressed_catalog,
                catalog_published_last: serde_json::Value::Null,
                catalog_last_written_whole_at: Instant::now(),
            })
        })();
        let mut broadcast = created.map_err(|refusal| MoqModuleError::Relay {
            what: format!("the broadcast `{broadcast_path}` could not be created: {refusal}"),
        })?;
        broadcast.publish_the_catalog(&serde_json::json!({}));
        broadcast
            .broadcast
            .announce(Default::default())
            .map_err(|refusal| MoqModuleError::Relay {
                what: format!("the broadcast `{broadcast_path}` could not be announced: {refusal}"),
            })?;
        Ok(broadcast)
    }

    /// A track of the broadcast named `track_name`, its stamps in nanoseconds.
    pub fn create_track(&self, track_name: &str) -> Result<moq_net::track::Producer> {
        self.broadcast
            .create_track(
                track_name,
                moq_net::track::Info::default().with_timescale(Timescale::NANO),
            )
            .map_err(|refusal| MoqModuleError::Relay {
                what: format!("the track `{track_name}` could not be created: {refusal}"),
            })
    }

    /// Publish `catalog` unless it is the one published last.
    pub fn publish_the_catalog(&mut self, catalog: &serde_json::Value) {
        if *catalog == self.catalog_published_last {
            return;
        }
        for (catalog_track, producer) in [
            (hang::Catalog::DEFAULT_NAME, &mut self.catalog),
            (hang::Catalog::COMPRESSED_NAME, &mut self.compressed_catalog),
        ] {
            if let Err(refusal) = producer.update(catalog) {
                tracing::warn!(
                    "MoqGateway: the catalog track `{catalog_track}` was not updated: {refusal}"
                );
            }
        }
        self.catalog_published_last = catalog.clone();
    }

    /// Write the catalog last published again, whole, as the first frame of a
    /// fresh group, once `every` has passed since it was last written whole.
    pub fn rewrite_the_catalog_whole_when_due(&mut self, every: std::time::Duration, now: Instant) {
        if now < self.catalog_last_written_whole_at + every {
            return;
        }
        self.catalog_last_written_whole_at = now;
        for (catalog_track, producer) in [
            (hang::Catalog::DEFAULT_NAME, &mut self.catalog),
            (hang::Catalog::COMPRESSED_NAME, &mut self.compressed_catalog),
        ] {
            if let Err(refusal) = producer
                .cut()
                .and_then(|()| producer.update(&self.catalog_published_last).map(|_| ()))
            {
                tracing::warn!(
                    "MoqGateway: the catalog track `{catalog_track}` was not written again whole: \
                     {refusal}"
                );
            }
        }
    }
}

impl Drop for MoqGatewayBroadcastAtTheRelay {
    fn drop(&mut self) {
        self.broadcast.close();
    }
}

/// One declared track's objects written at the relay: a group per group start,
/// and after a loss nothing until the next group start, never inside a group.
pub struct MoqGatewayTrackWriter {
    track: moq_net::track::Producer,
    group_being_written: Option<moq_net::group::Producer>,
    waiting_for_a_group_start: bool,
}

impl MoqGatewayTrackWriter {
    /// Write onto `track`, from the next group start on.
    pub fn new(track: moq_net::track::Producer) -> Self {
        Self {
            track,
            group_being_written: None,
            waiting_for_a_group_start: true,
        }
    }

    /// Drop the rest of the group being written; writing resumes at the next
    /// group start.
    pub fn give_up_the_group_being_written(&mut self) {
        if let Some(group) = self.group_being_written.take() {
            let _ = group.finish();
        }
        self.waiting_for_a_group_start = true;
    }

    /// Write one object stamped `timestamp_ns`, opening a group when it starts
    /// one.
    pub fn write_object(&mut self, payload: &[u8], timestamp_ns: i64, starts_group: bool) {
        if starts_group {
            if let Some(group) = self.group_being_written.take() {
                let _ = group.finish();
            }
            match self.track.append_group() {
                Ok(group) => {
                    self.group_being_written = Some(group);
                    self.waiting_for_a_group_start = false;
                }
                Err(refusal) => {
                    tracing::debug!("MoqGateway: a group was not opened: {refusal}");
                    self.give_up_the_group_being_written();
                    return;
                }
            }
        }
        if self.waiting_for_a_group_start {
            return;
        }
        let Some(group) = &mut self.group_being_written else {
            return;
        };
        if let Err(refusal) =
            group.write_frame(the_object_timestamp_of(timestamp_ns), payload.to_vec())
        {
            tracing::debug!("MoqGateway: an object was not written, so its group ends: {refusal}");
            self.give_up_the_group_being_written();
        }
    }
}

impl Drop for MoqGatewayTrackWriter {
    fn drop(&mut self) {
        if let Some(group) = self.group_being_written.take() {
            let _ = group.finish();
        }
        let _ = self.track.finish();
    }
}

/// A stamp as an object's timestamp: nanoseconds, a stamp before the clock's
/// zero read as zero.
fn the_object_timestamp_of(timestamp_ns: i64) -> Timestamp {
    Timestamp::from_nanos(u64::try_from(timestamp_ns).unwrap_or(0)).unwrap_or(Timestamp::ZERO)
}
