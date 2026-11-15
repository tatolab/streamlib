// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Fixtures shared by the engine's tests.

use super::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use crate::core::graph::graph_snapshot::GraphSnapshot;
use crate::core::runtime::RuntimeShutdownEscalation;
use crate::core::runtime::a_project_on_disk_for_a_test::options_loading_a_stream_of_the_test_project_at;
use crate::core::runtime::streams_loaded_in_this_runtime::loaded_stream::LoadedStreamInThisRuntime;
use crate::core::runtime::{HowALoadedStreamEnded, RuntimeStatus};
use crate::core::{Error, Result};
use serial_test::serial;

mod loaded_streams_shutdown_and_wait_tests;
mod runner_construction_and_description_tests;
mod surface_share_service_tests;
mod two_streams_sharing_one_engine_tests;

/// An empty stream named `stream_id` loaded into `runner`, its project in
/// `project_directory`.
fn an_empty_stream_loaded_into(
    runner: &Runner,
    project_directory: &std::path::Path,
    stream_id: &str,
) -> Arc<LoadedStreamInThisRuntime> {
    runner
        .load_an_empty_stream(options_loading_a_stream_of_the_test_project_at(
            project_directory,
            stream_id,
        ))
        .expect("an empty stream loads")
}

/// A project directory the test owns, removed with everything its streams
/// wrote when the test drops it.
fn a_project_directory_this_test_owns() -> tempfile::TempDir {
    crate::core::test_support::a_temporary_directory_at_owner_only_mode()
        .expect("a temporary project directory")
}

/// How long a test waits for a stream to end before calling it hung.
const A_STREAM_ENDS_WITHIN: Duration = Duration::from_secs(10);

/// Records every event published on one stream's topic.
struct EveryEventOnOneStreamsTopic(Arc<Mutex<Vec<crate::core::pubsub::Event>>>);

impl crate::core::pubsub::EventListener for EveryEventOnOneStreamsTopic {
    fn on_event(&mut self, event: &crate::core::pubsub::Event) -> Result<()> {
        self.0.lock().push(event.clone());
        Ok(())
    }

    fn on_events_lost(&mut self, _lost_event_count: u64) -> Result<()> {
        Ok(())
    }
}

/// Subscribe a recorder to `stream`'s topic, returning what it records and
/// the listener that keeps the subscription alive.
fn record_every_event_on_the_topic_of(
    stream: &LoadedStreamInThisRuntime,
) -> (
    Arc<Mutex<Vec<crate::core::pubsub::Event>>>,
    Arc<Mutex<dyn crate::core::pubsub::EventListener>>,
) {
    use crate::core::pubsub::{PUBSUB, topics};

    let recorded = Arc::new(Mutex::new(Vec::new()));
    let listener: Arc<Mutex<dyn crate::core::pubsub::EventListener>> = Arc::new(Mutex::new(
        EveryEventOnOneStreamsTopic(Arc::clone(&recorded)),
    ));
    PUBSUB
        .subscribe(
            &topics::loaded_stream(stream.loaded_stream_identity()),
            Arc::clone(&listener),
        )
        .expect("subscribe establishes the subscriber");
    (recorded, listener)
}

/// Publish a sentinel on `stream`'s topic and wait for `recorded` to hold
/// it, returning everything recorded before it. A listener's queue is FIFO,
/// so the sentinel arriving means every event published before it that
/// reached this topic already has.
fn recorded_before_a_sentinel_on_the_topic_of(
    stream: &LoadedStreamInThisRuntime,
    recorded: &Mutex<Vec<crate::core::pubsub::Event>>,
) -> Vec<crate::core::pubsub::Event> {
    use crate::core::pubsub::{Event, PUBSUB, topics};

    let topic = topics::loaded_stream(stream.loaded_stream_identity());
    let sentinel = Event::custom(&topic, serde_json::json!({"sentinel": true}));
    PUBSUB.publish(&topic, &sentinel);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        {
            let recorded = recorded.lock();
            if let Some(sentinel_at) = recorded.iter().position(|event| *event == sentinel) {
                return recorded[..sentinel_at].to_vec();
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the sentinel published on the stream's topic never arrived"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}
