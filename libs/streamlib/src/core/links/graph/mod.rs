// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Graph-related types for links.
//!
//! Contains the link blueprint (graph edge) and ECS components.

mod link_graph_edge;
mod link_id;
mod link_instance_component;
mod link_state_ecs_component;

pub use link_graph_edge::{Link, LinkDirection, LinkEndpoint};
pub use link_id::{LinkUniqueId, LinkUniqueIdError};
pub use link_instance_component::{LinkInstanceComponent, LinkTypeInfoComponent};
pub use link_state_ecs_component::{LinkState, LinkStateComponent};
