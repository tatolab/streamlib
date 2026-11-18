// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use anymap2::Map;

use super::{LinkComponentGraphRenders, NodeComponentGraphRenders};

/// Anything a node's component map can hold.
///
/// Storage and rendering are separate concerns and this is the storage half: a
/// component the control plane reads through something other than a
/// `components` key of its own — the settled `match_device` contracts, which
/// render on the ports that settled them — is storable without owing a
/// rendering it would never use.
pub trait StorableComponent: anymap2::any::Any + Send + Sync + 'static {}

impl<T: anymap2::any::Any + Send + Sync + 'static> StorableComponent for T {}

/// A node's component that also renders as its own key in `graph`.
pub trait NodeComponent: StorableComponent + NodeComponentGraphRenders {}

impl<T: StorableComponent + NodeComponentGraphRenders> NodeComponent for T {}

/// A link's component that also renders as its own key in `graph`.
pub trait LinkComponent: StorableComponent + LinkComponentGraphRenders {}

impl<T: StorableComponent + LinkComponentGraphRenders> LinkComponent for T {}

/// TypeMap for component storage (Send + Sync).
pub type ComponentMap = Map<dyn anymap2::any::Any + Send + Sync>;

/// Closure that renders one component of the map onto what `graph` renders of its node or link,
/// `Rendered`.
pub type ComponentSerializer<Rendered> = Box<dyn Fn(&ComponentMap, &mut Rendered) + Send + Sync>;

pub fn default_components() -> ComponentMap {
    ComponentMap::new()
}

pub fn default_component_serializers<Rendered>() -> Vec<ComponentSerializer<Rendered>> {
    Vec::new()
}
