// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::Mutex;
use serde_json::Value as JsonValue;

use super::JsonSerializableComponent;

/// The shader nodes one evaluator runs as one island, and how its evaluations
/// cut them into kernels.
#[derive(Debug)]
pub struct ShaderNodeFusedGroupOfAnIsland {
    evaluated_by: String,
    member_names_in_evaluation_order: Vec<String>,
    member_names_of_each_kernel_last_evaluated: Mutex<Option<Vec<Vec<String>>>>,
    submissions: AtomicU64,
    dispatches: AtomicU64,
}

impl ShaderNodeFusedGroupOfAnIsland {
    /// The group of `member_names_in_evaluation_order`, evaluated by the node named `evaluated_by`.
    pub fn of_the_members(
        evaluated_by: String,
        member_names_in_evaluation_order: Vec<String>,
    ) -> Self {
        Self {
            evaluated_by,
            member_names_in_evaluation_order,
            member_names_of_each_kernel_last_evaluated: Mutex::new(None),
            submissions: AtomicU64::new(0),
            dispatches: AtomicU64::new(0),
        }
    }

    /// Note one evaluation: one submission running one dispatch per kernel,
    /// each kernel the members it fused, by name.
    pub fn note_an_evaluation_of_the_kernels(&self, member_names_of_each_kernel: Vec<Vec<String>>) {
        self.submissions.fetch_add(1, Ordering::Relaxed);
        self.dispatches
            .fetch_add(member_names_of_each_kernel.len() as u64, Ordering::Relaxed);
        *self.member_names_of_each_kernel_last_evaluated.lock() = Some(member_names_of_each_kernel);
    }

    /// The submissions every evaluation so far recorded.
    pub fn submissions(&self) -> u64 {
        self.submissions.load(Ordering::Relaxed)
    }

    /// The dispatches every evaluation so far recorded.
    pub fn dispatches(&self) -> u64 {
        self.dispatches.load(Ordering::Relaxed)
    }
}

/// The fused group a shader node belongs to, rendered by `graph` as
/// `fused_group`: the node that evaluates it, its members in evaluation order,
/// the members each kernel of the last evaluation fused (`null` before the
/// first), and the submissions and dispatches every evaluation recorded.
pub struct ShaderNodeFusedGroupComponent(pub Arc<ShaderNodeFusedGroupOfAnIsland>);

impl JsonSerializableComponent for ShaderNodeFusedGroupComponent {
    fn json_key(&self) -> &'static str {
        "fused_group"
    }

    fn to_json(&self) -> JsonValue {
        let fused_group = &self.0;
        serde_json::json!({
            "evaluated_by": fused_group.evaluated_by,
            "members": fused_group.member_names_in_evaluation_order,
            "kernels": *fused_group.member_names_of_each_kernel_last_evaluated.lock(),
            "submissions": fused_group.submissions(),
            "dispatches": fused_group.dispatches(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_renders_no_kernels_until_an_evaluation_then_the_last_and_the_counts() {
        let fused_group = Arc::new(ShaderNodeFusedGroupOfAnIsland::of_the_members(
            "vignette".into(),
            vec!["tint".into(), "blur".into(), "vignette".into()],
        ));
        let component = ShaderNodeFusedGroupComponent(Arc::clone(&fused_group));
        assert_eq!(component.to_json()["kernels"], JsonValue::Null);
        assert_eq!(component.to_json()["submissions"], 0);

        fused_group.note_an_evaluation_of_the_kernels(vec![
            vec!["tint".into()],
            vec!["blur".into(), "vignette".into()],
        ]);
        fused_group.note_an_evaluation_of_the_kernels(vec![
            vec!["tint".into()],
            vec!["blur".into(), "vignette".into()],
        ]);
        let rendered = component.to_json();
        assert_eq!(rendered["evaluated_by"], "vignette");
        assert_eq!(
            rendered["members"],
            serde_json::json!(["tint", "blur", "vignette"])
        );
        assert_eq!(
            rendered["kernels"],
            serde_json::json!([["tint"], ["blur", "vignette"]])
        );
        assert_eq!(rendered["submissions"], 2);
        assert_eq!(rendered["dispatches"], 4);
    }
}
