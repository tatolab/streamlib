// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

fn abandoned_thread_of(
    node_run_id: &str,
    until_released: std::sync::mpsc::Receiver<()>,
) -> AbandonedNodeThreadStillRunning {
    AbandonedNodeThreadStillRunning {
        node: NodeDisplayNameAndId {
            node_run_id: NodeRunId::from(node_run_id),
            node_display_name: node_run_id.to_string(),
        },
        join_handle: std::thread::spawn(move || {
            let _ = until_released.recv();
        }),
    }
}

#[test]
fn a_restarted_node_an_earlier_compile_abandoned_is_still_held_until_its_thread_returns() {
    let (release_kaleidoscope, kaleidoscope_waits) = std::sync::mpsc::channel();
    let (release_other, other_waits) = std::sync::mpsc::channel();
    let abandoned_node_threads = Mutex::new(vec![
        abandoned_thread_of("Pkaleidoscope", kaleidoscope_waits),
        abandoned_thread_of("Pnot_restarted", other_waits),
    ]);
    let restarted = [NodeRunId::from("Pmix"), NodeRunId::from("Pkaleidoscope")];

    let held =
        the_restarted_nodes_an_abandoned_thread_still_runs(&abandoned_node_threads, &restarted);
    assert_eq!(
        held.iter()
            .map(|node| node.node_run_id.clone())
            .collect::<Vec<_>>(),
        vec![NodeRunId::from("Pkaleidoscope")]
    );

    release_kaleidoscope.send(()).unwrap();
    while abandoned_node_threads.lock().iter().any(|thread| {
        thread.node.node_run_id == "Pkaleidoscope" && !thread.join_handle.is_finished()
    }) {
        std::thread::yield_now();
    }
    assert!(
        the_restarted_nodes_an_abandoned_thread_still_runs(&abandoned_node_threads, &restarted,)
            .is_empty()
    );
    release_other.send(()).unwrap();
}
