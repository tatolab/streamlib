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
    payload_form_of_the_track_with_the_entry, the_catalog_entry_of_the_track_named,
};

/// How long the broadcast's catalog is read for a snapshot listing the track
/// before the track is written as sent: a port's entry may join the catalog
/// only once its first message is written.
const THE_TRACKS_CATALOG_ENTRY_IS_AWAITED_FOR: Duration = Duration::from_secs(5);

/// How old a group may grow behind a newer one before the read gives it up.
/// The stack's default gives up at once, which suits a player; a dump wants
/// every group a reordering delivered late, up to what the publisher keeps.
const A_GROUP_DELIVERED_LATE_IS_WAITED_FOR: Duration = Duration::from_secs(5);

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
    let objects_written = std::cell::Cell::new(0);

    let read = tokio::time::timeout(wait_at_most, async {
        let broadcast = tokio::select! {
            ended = connection.closed() => {
                return Err(anyhow!("the session with {} ended before the broadcast was announced: {ended:?}", arguments.relay_url));
            }
            broadcast = announced_broadcasts.routed_broadcast(arguments.broadcast.as_str()) => {
                broadcast.with_context(|| format!("the broadcast `{}` was refused", arguments.broadcast))?
            }
        };
        read_the_tracks_objects(&broadcast, arguments, &objects_written).await
    })
    .await;

    if tokio::time::timeout(
        THE_RELAY_IS_TOLD_THE_SESSION_CLOSED_WITHIN,
        moq_client.close(),
    )
    .await
    .is_err()
    {
        eprintln!("tatolab-read-track: the relay did not hear the session close");
    }
    read.map_err(|_elapsed| {
        anyhow!(
            "{} of {} objects of `{}` in `{}` arrived within {wait_at_most:?}",
            objects_written.get(),
            arguments.object_count,
            arguments.track,
            arguments.broadcast
        )
    })?
}

/// Subscribe to the track and the broadcast's catalog, and write the track's
/// objects until `arguments.object_count` are written, counting them into
/// `objects_written`.
async fn read_the_tracks_objects(
    broadcast: &moq_net::broadcast::Consumer,
    arguments: &ReadTrackArguments,
    objects_written: &std::cell::Cell<u64>,
) -> Result<u64> {
    let mut groups_of_the_track = broadcast
        .track(&arguments.track)?
        .subscribe(
            moq_net::track::Subscription::default()
                .with_max_age(A_GROUP_DELIVERED_LATE_IS_WAITED_FOR),
        )
        .await
        .with_context(|| format!("the track `{}` was refused", arguments.track))?
        .ordered();
    let (payload_form, catalog_entry) =
        match the_tracks_catalog_entry(broadcast, &arguments.track).await {
            Ok(catalog_entry) => (
                payload_form_of_the_track_with_the_entry(&catalog_entry),
                Some(catalog_entry),
            ),
            Err(why_not) => {
                eprintln!("tatolab-read-track: {why_not}; its objects are written as sent");
                (RelayTrackObjectPayloadForm::AsSent, None)
            }
        };
    let mut dump =
        RelayTrackObjectDumpDirectory::create(&arguments.output_directory, payload_form)?;
    if let Some(catalog_entry) = &catalog_entry {
        dump.write_the_catalog_entry(catalog_entry)?;
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
                transport_timestamp_ns: (!object.timestamp.is_zero())
                    .then(|| object.timestamp.as_nanos()),
                payload_as_sent: &object.payload,
            })?;
            objects_written.set(dump.objects_written());
        }
    }
    Ok(dump.objects_written())
}

/// The track's entry in the broadcast's catalog, read from the first snapshot
/// that lists it within [`THE_TRACKS_CATALOG_ENTRY_IS_AWAITED_FOR`], or why
/// none did.
async fn the_tracks_catalog_entry(
    broadcast: &moq_net::broadcast::Consumer,
    track_name: &str,
) -> Result<serde_json::Value> {
    let catalog_track = broadcast
        .track(hang::Catalog::DEFAULT_NAME)?
        .subscribe(None)
        .await
        .context("the broadcast serves no catalog")?;
    let mut catalog = moq_json::snapshot::Consumer::<serde_json::Value>::new(
        catalog_track,
        moq_json::snapshot::consumer::Config::default(),
    );
    tokio::time::timeout(THE_TRACKS_CATALOG_ENTRY_IS_AWAITED_FOR, async {
        loop {
            let Some(snapshot) = catalog.next().await.context("the catalog does not read")? else {
                return Err(anyhow!("the catalog ended without listing `{track_name}`"));
            };
            if let Some(catalog_entry) = the_catalog_entry_of_the_track_named(&snapshot, track_name)
            {
                return Ok(catalog_entry);
            }
        }
    })
    .await
    .map_err(|_elapsed| {
        anyhow!(
            "the catalog did not list `{track_name}` within {THE_TRACKS_CATALOG_ENTRY_IS_AWAITED_FOR:?}"
        )
    })?
}
