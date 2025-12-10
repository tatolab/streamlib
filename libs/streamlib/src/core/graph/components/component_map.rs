// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use anymap2::Map;

pub trait Component: anymap2::any::Any + Send + Sync + 'static {}

impl<T: anymap2::any::Any + Send + Sync + 'static> Component for T {}

/// TypeMap for component storage (Send + Sync).
pub type ComponentMap = Map<dyn anymap2::any::Any + Send + Sync>;

pub fn default_components() -> ComponentMap {
    ComponentMap::new()
}
