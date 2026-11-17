// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! How many times each loaded stream's graph was rendered, and how many commits of it began,
//! for a test that proves an operation renders none and compiles nothing.

use std::collections::HashMap;
use std::sync::LazyLock;

use parking_lot::Mutex;

use crate::core::pubsub::LoadedStreamIdentity;

static GRAPH_RENDERS_BY_LOADED_STREAM: LazyLock<Mutex<HashMap<String, u64>>> =
    LazyLock::new(Mutex::default);

static COMMITS_BEGUN_BY_LOADED_STREAM: LazyLock<Mutex<HashMap<String, u64>>> =
    LazyLock::new(Mutex::default);

/// Count one render of the graph of `loaded_stream`.
pub(crate) fn count_a_graph_render_of(loaded_stream: &LoadedStreamIdentity) {
    count_one_of(&GRAPH_RENDERS_BY_LOADED_STREAM, loaded_stream);
}

/// Count one commit of the graph of `loaded_stream` begun, whether or not it has anything to
/// compile.
pub(crate) fn count_a_commit_begun_of(loaded_stream: &LoadedStreamIdentity) {
    count_one_of(&COMMITS_BEGUN_BY_LOADED_STREAM, loaded_stream);
}

/// How many times the graph of `loaded_stream` was rendered.
pub fn graph_renders_of(loaded_stream: &LoadedStreamIdentity) -> u64 {
    the_count_of(&GRAPH_RENDERS_BY_LOADED_STREAM, loaded_stream)
}

/// How many commits of the graph of `loaded_stream` began.
pub fn commits_begun_of(loaded_stream: &LoadedStreamIdentity) -> u64 {
    the_count_of(&COMMITS_BEGUN_BY_LOADED_STREAM, loaded_stream)
}

fn count_one_of(counts: &Mutex<HashMap<String, u64>>, loaded_stream: &LoadedStreamIdentity) {
    *counts.lock().entry(loaded_stream.to_string()).or_default() += 1;
}

fn the_count_of(counts: &Mutex<HashMap<String, u64>>, loaded_stream: &LoadedStreamIdentity) -> u64 {
    counts
        .lock()
        .get(&loaded_stream.to_string())
        .copied()
        .unwrap_or_default()
}
