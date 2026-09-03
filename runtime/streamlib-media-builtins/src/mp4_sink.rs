// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Built-in MP4 sink: encoded bags in, one fragmented file out.
//!
//! The writer is [`Mp4FragmentedFileWriter`]. What lives here is the port
//! surface, the registration name, and the file the run writes into.
//!
//! One input takes any number of links and **each inbound link is one track**,
//! named by its source channel name — so two cameras are two video tracks and
//! three microphones three audio tracks with no configuration between them.

use std::fs::File;
use std::io::BufWriter;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use streamlib::sdk::context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::processors::ReactiveProcessor;

use crate::mp4_fragmented_file_writer::Mp4FragmentedFileWriter;

/// The registration name, and what every refusal names itself by.
pub const MP4_SINK_PROCESSOR_NAME: &str = "Mp4Sink";

/// How often a link that has never delivered a sync point is named.
const SILENT_LINK_REPORT_INTERVAL: Duration = Duration::from_secs(1);

/// Where the recording is written.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Mp4SinkConfig {
    /// The file to write, created or truncated at `setup()`.
    ///
    /// Truncating is the call: an app is re-run from the same `app.py`, and
    /// wall-clock file naming would be a fifth clock surface the plan bans.
    pub path: PathBuf,
}

#[streamlib::sdk::processor(
    description = "Records encoded video and audio bags to one fragmented MP4 file, one track per inbound link",
    execution = reactive,
    scheduling = high,
    config = crate::mp4_sink::Mp4SinkConfig,
    input(
        "tracks",
        delivery_profile = "ordered",
        description = "Encoded video or audio bags; each inbound link becomes one track named by its source channel"
    ),
)]
pub struct Mp4Sink {
    file_writer: Option<Mp4FragmentedFileWriter<BufWriter<File>>>,
    last_silent_link_report: Option<Instant>,
}

impl ReactiveProcessor for Mp4Sink::Processor {
    fn setup(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        // WIRE precedes `setup()`, so the links are readable here — which is
        // how the sink learns how many tracks it owes before a bag arrives.
        let inbound_link_names: Vec<String> = self
            .inputs
            .inbound_link_names("tracks")
            .into_iter()
            .map(|link| link.as_str().to_string())
            .collect();
        if inbound_link_names.is_empty() {
            return Err(Error::Runtime(format!(
                "{MP4_SINK_PROCESSOR_NAME}: no link enters `tracks`, so there is no track to \
                 record — connect at least one encoder's output to this sink"
            )));
        }

        let path = &self.config.path;
        let file = File::create(path).map_err(|open_failure| {
            Error::Runtime(format!(
                "{MP4_SINK_PROCESSOR_NAME}: `{}` could not be opened for writing: \
                 {open_failure}",
                path.display()
            ))
        })?;
        tracing::info!(
            path = %path.display(),
            tracks = inbound_link_names.len(),
            inbound_link_names = ?inbound_link_names,
            "{MP4_SINK_PROCESSOR_NAME}: recording, one track per inbound link"
        );
        self.file_writer = Some(Mp4FragmentedFileWriter::new(
            BufWriter::new(file),
            &inbound_link_names,
        ));
        Ok(())
    }

    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        if !self.inputs.has_data("tracks") {
            return Ok(());
        }
        let Some(file_writer) = self.file_writer.as_mut() else {
            return Ok(());
        };

        while let Some((bag_bytes, frame_header_timestamp_ns, inbound_link_name)) =
            self.inputs.read_raw_from_inbound_link("tracks")?
        {
            file_writer.accept_bag(
                inbound_link_name.as_str(),
                &bag_bytes,
                frame_header_timestamp_ns,
            )?;
        }

        if !file_writer.header_already_written() {
            let due = self
                .last_silent_link_report
                .is_none_or(|last| last.elapsed() >= SILENT_LINK_REPORT_INTERVAL);
            if due {
                let still_silent = file_writer.inbound_links_still_silent();
                if !still_silent.is_empty() {
                    tracing::info!(
                        inbound_links_still_silent = ?still_silent,
                        "{MP4_SINK_PROCESSOR_NAME}: waiting to write `moov` — a sample entry \
                         needs each track's parameter sets or Opus header"
                    );
                }
                self.last_silent_link_report = Some(Instant::now());
            }
        }
        Ok(())
    }

    fn teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        let Some(file_writer) = self.file_writer.take() else {
            return Ok(());
        };
        let tally = file_writer.finish()?;
        tracing::info!(
            path = %self.config.path.display(),
            fragments_written = tally.fragments_written,
            samples_written = tally.samples_written,
            bags_dropped_out_of_order = tally.bags_dropped_out_of_order,
            bags_discarded_after_latch = tally.bags_discarded_after_latch,
            tracks_latched = tally.tracks_latched,
            "{MP4_SINK_PROCESSOR_NAME}: teardown — the open fragment is closed"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use streamlib::sdk::processors::GeneratedProcessor;

    #[test]
    fn the_only_port_is_one_ordered_input_and_there_is_no_output() {
        let descriptor = <Mp4Sink::Processor as GeneratedProcessor>::descriptor()
            .expect("the macro emits a descriptor");

        assert_eq!(descriptor.inputs.len(), 1);
        assert_eq!(
            descriptor.outputs.len(),
            0,
            "a sink writes a file and publishes nothing"
        );
        let tracks = &descriptor.inputs[0];
        assert_eq!(tracks.name, "tracks");
        assert_eq!(
            tracks.delivery_profile.as_deref(),
            Some("ordered"),
            "a recording drops nothing silently"
        );
        assert!(
            tracks.audio_window.is_none(),
            "a windowed port refuses a second link, and this one takes any number"
        );
    }

    #[test]
    fn the_config_names_the_file_and_nothing_else() {
        let config: Mp4SinkConfig =
            serde_json::from_value(serde_json::json!({ "path": "/tmp/recording.mp4" }))
                .expect("path is the whole config");
        assert_eq!(config.path, PathBuf::from("/tmp/recording.mp4"));
    }
}
