// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

fn abandoned_thread_of(
    processor_id: &str,
    until_released: std::sync::mpsc::Receiver<()>,
) -> AbandonedProcessorThreadStillRunning {
    AbandonedProcessorThreadStillRunning {
        processor: ProcessorDisplayNameAndId {
            processor_id: ProcessorUniqueId::from(processor_id),
            processor_display_name: processor_id.to_string(),
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
    let abandoned_processor_threads = Mutex::new(vec![
        abandoned_thread_of("Pkaleidoscope", kaleidoscope_waits),
        abandoned_thread_of("Pnot_restarted", other_waits),
    ]);
    let restarted = [
        ProcessorUniqueId::from("Pmix"),
        ProcessorUniqueId::from("Pkaleidoscope"),
    ];

    let held = the_restarted_nodes_an_abandoned_thread_still_runs(
        &abandoned_processor_threads,
        &restarted,
    );
    assert_eq!(
        held.iter()
            .map(|processor| processor.processor_id.clone())
            .collect::<Vec<_>>(),
        vec![ProcessorUniqueId::from("Pkaleidoscope")]
    );

    release_kaleidoscope.send(()).unwrap();
    while abandoned_processor_threads.lock().iter().any(|thread| {
        thread.processor.processor_id == "Pkaleidoscope" && !thread.join_handle.is_finished()
    }) {
        std::thread::yield_now();
    }
    assert!(
        the_restarted_nodes_an_abandoned_thread_still_runs(
            &abandoned_processor_threads,
            &restarted,
        )
        .is_empty()
    );
    release_other.send(()).unwrap();
}
