// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use serde_json::Value as JsonValue;

use super::JsonComponent;

/// Marker component indicating an entity is pending deletion (soft-delete).
///
/// When `remove_processor` or `disconnect` is called, this component is added
/// to the entity immediately. The entity remains in the graph but is marked
/// for deletion. On the next `commit()` (when runtime is started), the compiler
/// processes the deletion: shuts down instances, unwires links, removes ECS
/// components, and finally removes from topology.
///
/// External observers can check for this component to know if an entity
/// is scheduled for removal but not yet fully deleted.
pub struct PendingDeletionComponent;

impl JsonComponent for PendingDeletionComponent {
    fn json_key(&self) -> &'static str {
        "pending_deletion"
    }

    fn to_json(&self) -> JsonValue {
        serde_json::json!(true)
    }
}
