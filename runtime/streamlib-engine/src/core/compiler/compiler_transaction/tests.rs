// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

fn an_update_of(node: &NodeRunId, config: serde_json::Value) -> PendingOperation {
    PendingOperation::UpdateNodeConfig {
        node_run_id: node.clone(),
        config_to_apply: config,
    }
}

#[test]
fn a_node_holds_the_config_its_last_logged_update_hands_it() {
    let first = NodeRunId::from("first");
    let second = NodeRunId::from("second");
    let tx = CompilerTransactionHandle::new(Arc::new(Mutex::new(Vec::new())));
    assert_eq!(tx.the_config_last_logged_for(&first), None);
    tx.log(an_update_of(&first, serde_json::json!({"a": 1})));
    tx.log(an_update_of(&second, serde_json::json!({"z": 9})));
    tx.log(an_update_of(&first, serde_json::json!({"a": 1, "b": 2})));
    assert_eq!(
        tx.the_config_last_logged_for(&first),
        Some(serde_json::json!({"a": 1, "b": 2}))
    );
    let taken = tx.take_each_config_update_of(&HashSet::from([first.clone()]));
    assert_eq!(taken.len(), 2);
    assert_eq!(tx.the_config_last_logged_for(&first), None);
    assert_eq!(
        tx.the_config_last_logged_for(&second),
        Some(serde_json::json!({"z": 9}))
    );
}
