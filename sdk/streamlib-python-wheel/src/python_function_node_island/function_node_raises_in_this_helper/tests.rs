// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn a_raise_is_kept_from_its_first_words_until_the_node_runs_cleanly() {
    let raises = FunctionNodeRaisesInThisHelper::default();
    raises.note_that_it_raised("island_0", "Pbeat", || "first".to_string());
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
    raises.note_that_it_raised("island_0", "Pbeat", || "broke".to_string());
    raises.note_that_it_raised("island_1", "Pbeat", || "broke".to_string());
    raises.note_that_it_ran_cleanly("island_0", "Pbeat");
    assert_eq!(
        raises.why_each_function_node_is_raising(),
        BTreeMap::from([("Pbeat".to_string(), "broke".to_string())])
    );
    raises.forget_the_island_feeding("island_1");
    assert!(raises.why_each_function_node_is_raising().is_empty());
}
