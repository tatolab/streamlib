// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Graph compilation pipeline.
//!
//! Converts graph topology changes into running processor instances.
//! The compilation process has 3 phases:
//! 1. PREPARE - Attach infrastructure components to processor nodes
//! 2. SPAWN - Spawn processor threads (threads create instances, setup runs on thread)
//! 3. WIRE - Create ring buffers and connect ports

mod compilation_plan;
mod compile_phase;
mod compile_result;
#[allow(clippy::module_inception)]
mod compiler;
pub mod compiler_ops;
mod compiler_transaction;
mod link_config_change;
mod pending_operation;
mod pending_operation_queue;
mod processor_config_change;
pub(crate) mod scheduling;

pub use compiler::Compiler;
pub use pending_operation::PendingOperation;
