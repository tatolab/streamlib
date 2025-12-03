//! Graph-related types for links.
//!
//! Contains the link blueprint (graph edge) and ECS components.

pub mod link_graph_edge;
pub mod link_id;
pub mod link_state_ecs_component;

pub use link_graph_edge::{Link, LinkDirection, LinkEndpoint, DEFAULT_LINK_CAPACITY};
pub use link_id::{LinkId, LinkIdError};
pub use link_state_ecs_component::{LinkState, LinkStateComponent};
