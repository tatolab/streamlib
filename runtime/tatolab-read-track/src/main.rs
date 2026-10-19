// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `tatolab-read-track <relay-url> <broadcast> <track> --count N --out <dir>`:
//! subscribe to one track at a relay as any MoQ client does, and write its
//! first `N` objects into `<dir>` — one file each, an index of their groups and
//! timestamps, and the broadcast's catalog. A stream's exposed port is the
//! track `<node>/<port>` of the broadcast `<machine>/<stream>.hang`; the
//! machine's local relay answers at `http://127.0.0.1:4443`.

// stderr is this tool's progress and refusal channel, as it is the CLI's.
#![allow(clippy::disallowed_macros)]

mod relay_track_object_dump;

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use clap::Parser;

use relay_track_object_dump::{
    RelayTrackObjectDumpDirectory, RelayTrackObjectPayloadForm, RelayTrackObjectRead,
};

/// How long the broadcast's first catalog is waited for before the track is
/// read as sent.
const THE_FIRST_CATALOG_IS_AWAITED_FOR: Duration = Duration::from_secs(5);

/// How long closing the session waits for the relay to hear it.
const THE_RELAY_IS_TOLD_THE_SESSION_CLOSED_WITHIN: Duration = Duration::from_secs(2);

/// Read one track at a relay and write its objects to files.
#[derive(Debug, Parser)]
#[command(name = "tatolab-read-track")]
struct ReadTrackArguments {
    /// The relay's URL; `http://` dials it with the certificate fingerprint the
    /// relay serves.
    relay_url: String,
    /// The broadcast's path at the relay, e.g. `<machine>/<stream>.hang`.
    broadcast: String,
    /// The track's name in the broadcast, e.g. `<node>/<port>`.
    track: String,
    /// How many objects to write before ending.
    #[arg(long = "count", value_name = "N")]
    object_count: u64,
    /// The directory the objects are written into, created when absent.
    #[arg(long = "out", value_name = "DIR")]
    output_directory: PathBuf,
    /// How long to wait for every object before ending without them.
    #[arg(long = "wait-at-most", value_name = "SECONDS", default_value_t = 30)]
    wait_at_most_seconds: u64,
}

fn main() -> ExitCode {
    let arguments = ReadTrackArguments::parse();
    let read = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("no async runtime builds")
        .and_then(|async_runtime| async_runtime.block_on(read_the_track(&arguments)));
    match read {
        Ok(objects_written) => {
            eprintln!(
                "tatolab-read-track: wrote {objects_written} objects of {} into {}",
                arguments.track,
                arguments.output_directory.display()
            );
            ExitCode::SUCCESS
        }
        Err(refusal) => {
            eprintln!("tatolab-read-track: {refusal:#}");
            ExitCode::FAILURE
        }
    }
}

/// Read `arguments.object_count` objects of the track into the output
/// directory, or refuse naming how many arrived in the time allowed.
async fn read_the_track(arguments: &ReadTrackArguments) -> Result<u64> {
    let relay_url: url::Url = arguments
        .relay_url
        .parse()
        .with_context(|| format!("`{}` is not a URL", arguments.relay_url))?;
    let moq_client = moq_tokio::connect::Config::default()
        .init(moq_tokio::quic::Config::default())
        .context("no MoQ client builds")?
        .with_reconnect(false);
    let subscribed_origin = moq_tokio::origin::spawn();
    let connection = moq_client
        .clone()
        .with_subscriber(subscribed_origin.clone())
        .connect(relay_url);
    let announced_broadcasts = subscribed_origin.consume();
    let wait_at_most = Duration::from_secs(arguments.wait_at_most_seconds);

    let read = tokio::time::timeout(wait_at_most, async {
        let broadcast = tokio::select! {
            ended = connection.closed() => {
                return Err(anyhow!("the session with {} ended before the broadcast was announced: {ended:?}", arguments.relay_url));
            }
            broadcast = announced_broadcasts.routed_broadcast(arguments.broadcast.as_str()) => {
                broadcast.with_context(|| format!("the broadcast `{}` was refused", arguments.broadcast))?
            }
        };
        read_the_tracks_objects(&broadcast, arguments).await
    })
    .await;

    let _ = tokio::time::timeout(
        THE_RELAY_IS_TOLD_THE_SESSION_CLOSED_WITHIN,
        moq_client.close(),
    )
    .await;
    read.map_err(|_elapsed| {
        anyhow!(
            "{} objects of `{}` in `{}` did not arrive within {wait_at_most:?}",
            arguments.object_count,
            arguments.track,
            arguments.broadcast
        )
    })?
}

/// Subscribe to the track and the broadcast's catalog, and write the track's
/// objects until `arguments.object_count` are written.
async fn read_the_tracks_objects(
    broadcast: &moq_net::broadcast::Consumer,
    arguments: &ReadTrackArguments,
) -> Result<u64> {
    let mut groups_of_the_track = broadcast
        .track(&arguments.track)?
        .subscribe(None)
        .await
        .with_context(|| format!("the track `{}` was refused", arguments.track))?
        .ordered();
    let catalog_json = the_broadcasts_catalog(broadcast).await;
    let payload_form = catalog_json
        .as_ref()
        .and_then(|catalog_json| serde_json::from_value::<hang::Catalog>(catalog_json.clone()).ok())
        .map(|catalog| {
            RelayTrackObjectPayloadForm::of_the_track_in_the_catalog(&catalog, &arguments.track)
        })
        .unwrap_or(RelayTrackObjectPayloadForm::AsSent);
    let mut dump =
        RelayTrackObjectDumpDirectory::create(&arguments.output_directory, payload_form)?;
    if let Some(catalog_json) = &catalog_json {
        dump.write_the_catalog(catalog_json)?;
    }

    while dump.objects_written() < arguments.object_count {
        let Some(mut group) = groups_of_the_track
            .next_group()
            .await
            .context("the track ended in error")?
        else {
            return Err(anyhow!(
                "the track ended after {} of {} objects",
                dump.objects_written(),
                arguments.object_count
            ));
        };
        let group_sequence = group.sequence;
        let mut starts_group = true;
        while dump.objects_written() < arguments.object_count {
            let object = match group.read_frame().await {
                Ok(Some(object)) => object,
                Ok(None) => break,
                Err(group_ended_early) => {
                    eprintln!(
                        "tatolab-read-track: group {group_sequence} ended early: {group_ended_early}"
                    );
                    break;
                }
            };
            dump.write_the_object(RelayTrackObjectRead {
                group_sequence,
                starts_group: std::mem::take(&mut starts_group),
                transport_timestamp_ns: object.timestamp.as_nanos(),
                payload_as_sent: &object.payload,
            })?;
        }
    }
    Ok(dump.objects_written())
}

/// The broadcast's catalog as it stands, or `None` when it serves none within
/// [`THE_FIRST_CATALOG_IS_AWAITED_FOR`].
async fn the_broadcasts_catalog(
    broadcast: &moq_net::broadcast::Consumer,
) -> Option<serde_json::Value> {
    let catalog_track = broadcast
        .track(hang::Catalog::DEFAULT_NAME)
        .ok()?
        .subscribe(None)
        .await
        .ok()?;
    let mut catalog = moq_json::snapshot::Consumer::<serde_json::Value>::new(
        catalog_track,
        moq_json::snapshot::consumer::Config::default(),
    );
    tokio::time::timeout(THE_FIRST_CATALOG_IS_AWAITED_FOR, catalog.next())
        .await
        .ok()?
        .ok()?
}
