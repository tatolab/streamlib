// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

mod mutation_ops;

pub(crate) use mutation_ops::the_default_name_a_new_node_takes;
mod query_ops;
mod traversal_source;

pub use traversal_source::{
    LinkTraversal, LinkTraversalMut, ProcessorTraversal, ProcessorTraversalMut, TraversalSource,
    TraversalSourceMut,
};
