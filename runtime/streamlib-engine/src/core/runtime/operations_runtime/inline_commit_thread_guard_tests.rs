// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn a_node_execution_thread_never_commits_inline_and_other_threads_still_do() {
    assert!(this_thread_may_commit_inline());

    let from_a_node_thread = std::thread::spawn(|| {
        mark_this_thread_as_a_node_execution_thread();
        this_thread_may_commit_inline()
    })
    .join()
    .expect("the marked thread finishes");
    assert!(
        !from_a_node_thread,
        "a node's thread must not wait for a compile"
    );

    assert!(this_thread_may_commit_inline(), "the marker is per thread");
}
