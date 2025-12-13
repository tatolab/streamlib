// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::graph::LinkUniqueId;

/// Link config change (capacity, buffer strategy, future use).
#[derive(Debug, Clone)]
pub struct LinkConfigChange {
    pub id: LinkUniqueId,
    pub new_capacity: Option<usize>,
}
