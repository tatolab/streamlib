// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

mod mutation_ops;
mod query_ops;
mod traversal_source;

pub(crate) use mutation_ops::the_requested_node_name_unless_taken;

pub use traversal_source::{
    LinkTraversal, LinkTraversalMut, ProcessorTraversal, ProcessorTraversalMut, TraversalSource,
    TraversalSourceMut,
};
