// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

fn a_raise_worded(raising_because: &str) -> FunctionNodeRaiseDescribed {
    FunctionNodeRaiseDescribed {
        raising_because: raising_because.to_string(),
        traceback: Some(format!(
            "Traceback (most recent call last):\n{raising_because}\n"
        )),
    }
}

#[test]
fn a_raise_is_kept_from_its_first_words_until_the_node_runs_cleanly() {
    let raises = FunctionNodeRaisesInThisHelper::default();
    raises.note_that_it_raised("island_0", "Pbeat", || a_raise_worded("first"));
    raises.note_that_it_raised("island_0", "Pbeat", || unreachable!("a raise is held"));
    assert_eq!(
        raises.why_each_function_node_is_raising(),
        BTreeMap::from([("Pbeat".to_string(), "first".to_string())])
    );
    raises.note_that_it_ran_cleanly("island_0", "Pbeat");
    assert!(raises.why_each_function_node_is_raising().is_empty());
}

#[test]
fn a_node_two_islands_call_raises_while_either_still_finds_it_raising() {
    let raises = FunctionNodeRaisesInThisHelper::default();
    raises.note_that_it_raised("island_0", "Pbeat", || a_raise_worded("broke"));
    raises.note_that_it_raised("island_1", "Pbeat", || a_raise_worded("broke"));
    raises.note_that_it_ran_cleanly("island_0", "Pbeat");
    assert_eq!(
        raises.why_each_function_node_is_raising(),
        BTreeMap::from([("Pbeat".to_string(), "broke".to_string())])
    );
    raises.forget_the_island_feeding("island_1");
    assert!(raises.why_each_function_node_is_raising().is_empty());
}

#[test]
fn only_the_first_raise_of_a_run_is_held_for_the_log_once() {
    let raises = FunctionNodeRaisesInThisHelper::default();
    raises.note_that_it_raised("island_0", "Pbeat", || a_raise_worded("broke"));
    raises.note_that_it_raised("island_0", "Pbeat", || unreachable!("a raise is held"));
    raises.note_that_it_raised("island_1", "Pbeat", || a_raise_worded("broke again"));
    assert_eq!(
        raises.take_each_raise_newly_noted(),
        [FunctionNodeRaiseNewlyNoted {
            function_node_id: "Pbeat".to_string(),
            raise: a_raise_worded("broke"),
        }]
    );
    assert!(raises.take_each_raise_newly_noted().is_empty());

    raises.note_that_it_ran_cleanly("island_0", "Pbeat");
    raises.note_that_it_ran_cleanly("island_1", "Pbeat");
    raises.note_that_it_raised("island_0", "Pbeat", || a_raise_worded("broke once more"));
    assert_eq!(
        raises.take_each_raise_newly_noted(),
        [FunctionNodeRaiseNewlyNoted {
            function_node_id: "Pbeat".to_string(),
            raise: a_raise_worded("broke once more"),
        }],
        "a raise after a clean call begins a new run"
    );
}
