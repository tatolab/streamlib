// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! What the engine's two suites of links between streams on one machine share:
//! bag tallies, a started stream, a link as `graph` renders it, and a wait on
//! one stream's events. The including module names the engine crate
//! `the_engine_under_test`.

use std::collections::HashMap;
use std::sync::{Arc, Condvar, LazyLock, Mutex};
use std::time::{Duration, Instant};

use super::the_engine_under_test::core::Result;
use super::the_engine_under_test::core::graph::LinkUniqueId;
use super::the_engine_under_test::core::pubsub::{Event, EventListener, PUBSUB, topics};
use super::the_engine_under_test::core::runtime::LoadedStreamInThisRuntime;

/// How long a test waits for something the engine does on its own threads.
pub const THE_ENGINE_ANSWERS_WITHIN: Duration = Duration::from_secs(20);

/// Every sink's tally by name, and the signal a sink raises on each bag.
static BAG_TALLIES: LazyLock<(Mutex<HashMap<String, usize>>, Condvar)> =
    LazyLock::new(|| (Mutex::default(), Condvar::new()));

/// Count `read` bags into `tally` and wake every test waiting on a tally.
pub fn tally_bags_read(tally: &str, read: usize) {
    let (tallies, a_bag_arrived) = &*BAG_TALLIES;
    *tallies
        .lock()
        .unwrap()
        .entry(tally.to_string())
        .or_default() += read;
    a_bag_arrived.notify_all();
}

/// How many bags `tally` counts.
pub fn bags_tallied_as(tally: &str) -> usize {
    BAG_TALLIES
        .0
        .lock()
        .unwrap()
        .get(tally)
        .copied()
        .unwrap_or(0)
}

/// Wait until `tally` counts at least `at_least` bags, saying whether it did
/// within the budget.
pub fn wait_until_tallied(tally: &str, at_least: usize) -> bool {
    let (tallies, a_bag_arrived) = &*BAG_TALLIES;
    let (tallies, _) = a_bag_arrived
        .wait_timeout_while(
            tallies.lock().unwrap(),
            THE_ENGINE_ANSWERS_WITHIN,
            |tallies| tallies.get(tally).copied().unwrap_or(0) < at_least,
        )
        .unwrap();
    tallies.get(tally).copied().unwrap_or(0) >= at_least
}

/// Start `stream` and wait until every processor of it runs.
pub fn start(stream: &Arc<LoadedStreamInThisRuntime>) {
    stream.start().expect("the stream starts");
    stream
        .wait_until_every_processor_is_running(THE_ENGINE_ANSWERS_WITHIN)
        .expect("every processor of the stream runs");
}

/// The link `link_id` as `graph` renders it in `stream`, or `Null` once it is
/// gone.
pub fn the_link_as_graph_renders_it(
    stream: &LoadedStreamInThisRuntime,
    link_id: &LinkUniqueId,
) -> serde_json::Value {
    stream.to_json().expect("the graph renders")["links"]
        .as_array()
        .and_then(|links| links.iter().find(|link| link["id"] == link_id.as_str()))
        .cloned()
        .unwrap_or(serde_json::Value::Null)
}

struct ForwardsEveryEvent(std::sync::mpsc::Sender<()>);

impl EventListener for ForwardsEveryEvent {
    fn on_event(&mut self, _event: &Event) -> Result<()> {
        let _ = self.0.send(());
        Ok(())
    }
}

/// Every event on one stream's topic, as a signal to look at its graph again.
pub struct EveryEventOfOneStream {
    _listener: Arc<parking_lot::Mutex<dyn EventListener>>,
    an_event_arrived: std::sync::mpsc::Receiver<()>,
}

impl EveryEventOfOneStream {
    /// Listen to every event on `stream`'s topic.
    pub fn listen_to(stream: &LoadedStreamInThisRuntime) -> Self {
        let (forward, an_event_arrived) = std::sync::mpsc::channel();
        let listener: Arc<parking_lot::Mutex<dyn EventListener>> =
            Arc::new(parking_lot::Mutex::new(ForwardsEveryEvent(forward)));
        PUBSUB
            .subscribe(
                &topics::loaded_stream(stream.loaded_stream_identity()),
                Arc::clone(&listener),
            )
            .expect("the listener subscribes");
        Self {
            _listener: listener,
            an_event_arrived,
        }
    }

    /// Wait, looking again at each event, until link `link_id` of `stream`
    /// renders as `holds` wants it; the last rendering either way.
    pub fn until_the_link(
        &self,
        stream: &LoadedStreamInThisRuntime,
        link_id: &LinkUniqueId,
        holds: impl Fn(&serde_json::Value) -> bool,
    ) -> serde_json::Value {
        let deadline = Instant::now() + THE_ENGINE_ANSWERS_WITHIN;
        loop {
            let rendered = the_link_as_graph_renders_it(stream, link_id);
            if holds(&rendered) {
                return rendered;
            }
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                return rendered;
            };
            if self.an_event_arrived.recv_timeout(left).is_err() {
                return the_link_as_graph_renders_it(stream, link_id);
            }
        }
    }
}

/// Whether `link`, as `graph` renders it, reads `wired`.
pub fn reads_wired(link: &serde_json::Value) -> bool {
    link["state"] == "wired"
}

/// Whether a link, as `graph` renders it, reads `awaiting_remote` with a
/// reason naming `why`.
pub fn reads_awaiting_remote_because(why: &'static str) -> impl Fn(&serde_json::Value) -> bool {
    move |link| {
        link["state"] == "awaiting_remote"
            && link["awaiting_remote_reason"]
                .as_str()
                .is_some_and(|reason| reason.contains(why))
    }
}
