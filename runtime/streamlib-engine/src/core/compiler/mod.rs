// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

mod compilation_plan;
mod compile_phase;
mod compile_result;
#[allow(
    clippy::module_inception,
    reason = "the module holds the `Compiler` type its parent is named for"
)]
mod compiler;
pub(crate) mod compiler_ops;
mod compiler_transaction;
pub(crate) mod node_thread_shutdown;
mod pending_operation;
pub(crate) mod scheduling;

pub(crate) use compiler::Compiler;
pub(crate) use compiler_transaction::CompilerTransactionHandle;
pub(crate) use node_thread_shutdown::NodeThreadKind;
pub(crate) use pending_operation::PendingOperation;
