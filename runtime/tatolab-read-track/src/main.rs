// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `tatolab-read-track <relay-url> <broadcast> <track> --count N --out <dir>`:
//! subscribe to one track at a relay as any MoQ client does, and write its
//! first `N` objects into `<dir>` — one file each, an index of their groups,
//! timestamps and payload form, and the track's catalog entry. A stream's exposed port is the
//! track `<node>/<port>` of the broadcast `<machine>/<stream>.hang`; the
//! machine's local relay answers at `http://127.0.0.1:4443`. A relay URL
//! carrying a credential is read from stdin with `-`, never an argument.

// stderr is this tool's progress and refusal channel, as it is the CLI's.
#![allow(clippy::disallowed_macros)]

mod relay_track_object_dump;

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use clap::Parser;

use streamlib_relay_track_reader::{
    RelayTrackGroupsReleasedInSequenceOrder, ReleasedRelayTrackGroup,
};

use relay_track_object_dump::{
    RelayTrackCatalogEntry, RelayTrackObjectDumpDirectory, RelayTrackObjectPayloadForm,
    RelayTrackObjectRead,
};

/// How long the broadcast's catalog is read for a snapshot listing the track
/// before the track is written as sent, unless `--catalog-wait-at-most` says:
/// a port's entry may join the catalog only once its first message is written.
const THE_TRACKS_CATALOG_ENTRY_IS_AWAITED_FOR_SECONDS: u64 = 5;

/// How long closing the session waits for the relay to hear it.
const THE_RELAY_IS_TOLD_THE_SESSION_CLOSED_WITHIN: Duration = Duration::from_secs(2);

/// Read one track at a relay and write its objects to files.
#[derive(Debug, Parser)]
#[command(name = "tatolab-read-track")]
struct ReadTrackArguments {
    /// The relay's URL; `http://` dials it with the certificate fingerprint the
    /// relay serves. `-` reads it from stdin, for a URL carrying a credential,
    /// which is then named by its scheme, host and port alone.
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
    /// How long to read the broadcast's catalog for an entry listing the track
    /// before writing its objects as sent.
    #[arg(
        long = "catalog-wait-at-most",
        value_name = "SECONDS",
        default_value_t = THE_TRACKS_CATALOG_ENTRY_IS_AWAITED_FOR_SECONDS
    )]
    catalog_wait_at_most_seconds: u64,
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

/// What a relay URL argument of `-` names: the URL is read from stdin.
const RELAY_URL_READ_FROM_STDIN: &str = "-";

/// The relay URL `relay_url_argument` names, and how a message names it: as
/// written, or by its scheme, host and port alone when it was read from stdin
/// and may carry a credential anywhere else in it.
fn the_relay_url_and_how_it_is_named(relay_url_argument: &str) -> Result<(url::Url, String)> {
    if relay_url_argument != RELAY_URL_READ_FROM_STDIN {
        let relay_url = relay_url_argument
            .parse()
            .with_context(|| format!("`{relay_url_argument}` is not a URL"))?;
        return Ok((relay_url, relay_url_argument.to_string()));
    }
    let mut relay_url_read = String::new();
    std::io::Read::read_to_string(&mut std::io::stdin(), &mut relay_url_read)
        .context("the relay URL could not be read from stdin")?;
    let relay_url: url::Url = relay_url_read
        .trim()
        .parse()
        .map_err(|_| anyhow!("the relay URL read from stdin is not a URL"))?;
    let relay_as_named = the_relay_named_by_its_origin_alone(&relay_url);
    Ok((relay_url, relay_as_named))
}

/// `relay_url` as `scheme://host[:port]`, nothing of its path or query.
fn the_relay_named_by_its_origin_alone(relay_url: &url::Url) -> String {
    relay_url.origin().ascii_serialization()
}

/// Read `arguments.object_count` objects of the track into the output
/// directory, or refuse naming how many arrived in the time allowed.
async fn read_the_track(arguments: &ReadTrackArguments) -> Result<u64> {
    let (relay_url, relay_as_named) = the_relay_url_and_how_it_is_named(&arguments.relay_url)?;
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
                return Err(anyhow!("the session with {relay_as_named} ended before the broadcast was announced: {ended:?}"));
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
    let mut groups_of_the_track =
        RelayTrackGroupsReleasedInSequenceOrder::subscribe_waiting_the_tracks_retention(
            &broadcast.track(&arguments.track)?,
        )
        .await
        .with_context(|| format!("the track `{}` was refused", arguments.track))?;
    let (payload_form, catalog_entry) = match the_tracks_catalog_entry(
        broadcast,
        &arguments.track,
        Duration::from_secs(arguments.catalog_wait_at_most_seconds),
    )
    .await
    {
        Ok(catalog_entry) => (catalog_entry.payload_form()?, Some(catalog_entry)),
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
        let mut group = match groups_of_the_track
            .next_released_group()
            .await
            .context("the track ended in error")?
        {
            Some(ReleasedRelayTrackGroup::InSequence(group)) => group,
            Some(ReleasedRelayTrackGroup::GivenUp { groups }) => {
                eprintln!("tatolab-read-track: {groups} groups were given up");
                continue;
            }
            None => {
                return Err(anyhow!(
                    "the track ended after {} of {} objects",
                    dump.objects_written(),
                    arguments.object_count
                ));
            }
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
/// that lists it within `wait_at_most`, or why none did.
async fn the_tracks_catalog_entry(
    broadcast: &moq_net::broadcast::Consumer,
    track_name: &str,
    wait_at_most: Duration,
) -> Result<RelayTrackCatalogEntry> {
    let awaited_from = std::time::Instant::now();
    let catalog_track = broadcast
        .track(hang::Catalog::DEFAULT_NAME)?
        .subscribe(None)
        .await
        .context("the broadcast serves no catalog")?;
    let mut catalog = moq_json::snapshot::Consumer::<serde_json::Value>::new(
        catalog_track,
        moq_json::snapshot::consumer::Config::default(),
    );
    tokio::time::timeout(wait_at_most, async {
        loop {
            let Some(snapshot) = catalog.next().await.context("the catalog does not read")? else {
                return Err(anyhow!("the catalog ended without listing `{track_name}`"));
            };
            if let Some(catalog_entry) =
                RelayTrackCatalogEntry::of_the_track_named(&snapshot, track_name)
            {
                eprintln!(
                    "tatolab-read-track: the catalog listed `{track_name}` after {:.1?}",
                    awaited_from.elapsed()
                );
                return Ok(catalog_entry);
            }
        }
    })
    .await
    .map_err(|_elapsed| {
        anyhow!("the catalog did not list `{track_name}` within {wait_at_most:?}")
    })?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_relay_read_from_stdin_is_named_by_its_origin_and_never_its_path_or_query() {
        for (relay_url, named) in [
            (
                "https://draft-16.relay.example/sub-token-91c2",
                "https://draft-16.relay.example",
            ),
            (
                "https://cdn.relay.example/pb1test?jwt=eyJ.secret.sig",
                "https://cdn.relay.example",
            ),
            ("http://127.0.0.1:4443/?jwt=abc", "http://127.0.0.1:4443"),
        ] {
            assert_eq!(
                the_relay_named_by_its_origin_alone(&relay_url.parse().unwrap()),
                named
            );
        }
    }

    #[test]
    fn a_relay_url_given_as_an_argument_is_named_as_written() {
        let (relay_url, named) =
            the_relay_url_and_how_it_is_named("http://127.0.0.1:4443").unwrap();
        assert_eq!(relay_url.as_str(), "http://127.0.0.1:4443/");
        assert_eq!(named, "http://127.0.0.1:4443");
    }
}
