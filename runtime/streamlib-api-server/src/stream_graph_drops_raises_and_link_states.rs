// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The parts of a stream's live graph that change without a graph event — each node's drop and
//! refusal counts and the raise its `process()` keeps, and each link's state — sampled per
//! followed stream so a control connection is sent `drops_raises_or_link_states_changed`.

use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::{Map, Value};

/// How often a control connection samples the drops, raises and link states of each stream graph
/// it follows, and so the most often it is told one stream's changed.
pub(crate) const STREAM_GRAPH_DROPS_RAISES_AND_LINK_STATES_SAMPLED_EVERY: Duration =
    Duration::from_millis(500);

/// The drops, raises and link states `graph` renders for one stream, keyed by node and link id.
pub(crate) fn the_drops_raises_and_link_states_of_a_stream_graph(stream_graph: &Value) -> Value {
    let mut nodes = Map::new();
    for node in stream_graph["nodes"].as_array().into_iter().flatten() {
        let Some(node_id) = node["id"].as_str() else {
            continue;
        };
        let components = &node["components"];
        nodes.insert(
            node_id.to_owned(),
            serde_json::json!([
                components["metrics"]["dropped_bags_by_link"],
                components["metrics"]["refused_bags_by_output_port"],
                components["process_raising_because"],
            ]),
        );
    }
    let mut links = Map::new();
    for link in stream_graph["links"].as_array().into_iter().flatten() {
        let Some(link_id) = link["id"].as_str() else {
            continue;
        };
        links.insert(
            link_id.to_owned(),
            serde_json::json!([link["state"], link["error_reason"]]),
        );
    }
    serde_json::json!({ "nodes": nodes, "links": links })
}

/// The drops, raises and link states one connection last sampled of each stream it follows.
#[derive(Default)]
pub(crate) struct StreamGraphDropsRaisesAndLinkStatesLastSampled {
    last_sampled_by_stream: BTreeMap<String, Value>,
}

impl StreamGraphDropsRaisesAndLinkStatesLastSampled {
    /// Keep `sampled_by_stream` as the last sample, answering each stream whose sample differs
    /// from the one before it. A stream sampled for the first time is answered by none, and a
    /// stream left out is forgotten.
    pub(crate) fn streams_whose_sample_changed(
        &mut self,
        sampled_by_stream: BTreeMap<String, Value>,
    ) -> Vec<String> {
        let streams_whose_sample_changed = sampled_by_stream
            .iter()
            .filter(|(stream, sampled)| {
                self.last_sampled_by_stream
                    .get(*stream)
                    .is_some_and(|last_sampled| last_sampled != *sampled)
            })
            .map(|(stream, _)| stream.clone())
            .collect();
        self.last_sampled_by_stream = sampled_by_stream;
        streams_whose_sample_changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn stream_graph(
        dropped_on_the_link: u64,
        process_raising_because: Value,
        link_state: &str,
    ) -> Value {
        json!({
            "stream": "s-main-1",
            "nodes": [{
                "id": "breaker",
                "config": { "every": 3 },
                "components": {
                    "state": "Running",
                    "process_raising_because": process_raising_because,
                    "metrics": {
                        "frames_dropped": dropped_on_the_link,
                        "dropped_bags_by_link": { "L1": dropped_on_the_link },
                        "refused_bags_by_output_port": { "video": 0 },
                        "written_bags_by_output_port": { "video": 600 },
                    },
                },
            }],
            "links": [{ "id": "L1", "state": link_state, "components": { "state": "Wired" } }],
        })
    }

    fn sampled(stream_graph: &Value) -> BTreeMap<String, Value> {
        BTreeMap::from([(
            "s-main-1".to_owned(),
            the_drops_raises_and_link_states_of_a_stream_graph(stream_graph),
        )])
    }

    /// A new drop count, a new raise and a new link state each change the sample; a written
    /// count, which grows with every bag, does not.
    ///
    /// Mental-revert: sampling `written_bags_by_output_port` sends a change on every sample of a
    /// running stream; leaving out any of the three leaves that value stale on a pushed client.
    #[test]
    fn a_drop_a_raise_or_a_link_state_changes_the_sample_and_a_written_count_does_not() {
        let running = stream_graph(0, Value::Null, "wired");
        let mut written_more = running.clone();
        written_more["nodes"][0]["components"]["metrics"]["written_bags_by_output_port"]["video"] =
            json!(900);
        let unchanged = the_drops_raises_and_link_states_of_a_stream_graph(&running);

        assert_eq!(
            the_drops_raises_and_link_states_of_a_stream_graph(&written_more),
            unchanged
        );
        for changed in [
            stream_graph(4, Value::Null, "wired"),
            stream_graph(0, json!("ValueError: every must be odd"), "wired"),
            stream_graph(0, Value::Null, "error"),
        ] {
            assert_ne!(
                the_drops_raises_and_link_states_of_a_stream_graph(&changed),
                unchanged
            );
        }
    }

    /// A stream's first sample answers nothing, a changed one answers the stream, and a stream no
    /// longer sampled is sampled afresh when it comes back.
    ///
    /// Mental-revert: answering a first sample sends every new subscriber a change it already
    /// read; keeping a stream no longer sampled answers a stale difference when it is loaded again.
    #[test]
    fn a_stream_is_answered_only_when_its_sample_differs_from_the_one_before() {
        let mut last_sampled = StreamGraphDropsRaisesAndLinkStatesLastSampled::default();

        assert!(
            last_sampled
                .streams_whose_sample_changed(sampled(&stream_graph(0, Value::Null, "wired")))
                .is_empty()
        );
        assert!(
            last_sampled
                .streams_whose_sample_changed(sampled(&stream_graph(0, Value::Null, "wired")))
                .is_empty()
        );
        assert_eq!(
            last_sampled.streams_whose_sample_changed(sampled(&stream_graph(
                2,
                Value::Null,
                "wired"
            ))),
            vec!["s-main-1".to_owned()]
        );
        assert!(
            last_sampled
                .streams_whose_sample_changed(BTreeMap::new())
                .is_empty()
        );
        assert!(
            last_sampled
                .streams_whose_sample_changed(sampled(&stream_graph(9, Value::Null, "wired")))
                .is_empty()
        );
    }
}
