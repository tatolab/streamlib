// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! How many times each loaded stream's graph was rendered, for a test that proves an operation
//! renders none.

use std::collections::HashMap;
use std::sync::LazyLock;

use parking_lot::Mutex;

use crate::core::pubsub::LoadedStreamIdentity;

static GRAPH_RENDERS_BY_LOADED_STREAM: LazyLock<Mutex<HashMap<String, u64>>> =
    LazyLock::new(Mutex::default);

/// Count one render of the graph of `loaded_stream`.
pub(crate) fn count_a_graph_render_of(loaded_stream: &LoadedStreamIdentity) {
    *GRAPH_RENDERS_BY_LOADED_STREAM
        .lock()
        .entry(loaded_stream.to_string())
        .or_default() += 1;
}

/// How many times the graph of `loaded_stream` was rendered.
pub fn graph_renders_of(loaded_stream: &LoadedStreamIdentity) -> u64 {
    GRAPH_RENDERS_BY_LOADED_STREAM
        .lock()
        .get(&loaded_stream.to_string())
        .copied()
        .unwrap_or_default()
}
