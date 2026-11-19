// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A commit of the operations the compiler's transaction logged: compiled in the turn a commit
//! holds, keeping or releasing the transaction's hold for one commit.

use std::sync::Arc;

use super::Compiler;
use crate::core::context::RuntimeContext;
use crate::core::error::{Error, Result};

/// A commit that failed, and whether it failed after its compile took something out of the
/// running stream or built into it, so a take-back of what it was asked to do leaves the stream
/// changed.
#[derive(Debug)]
pub(crate) struct ACommitFailed {
    pub(crate) refusal: Error,
    pub(crate) after_changing_what_runs: bool,
}

/// Whether a commit keeps the transaction's hold for one commit or releases it.
#[derive(Clone, Copy)]
enum TheHoldForOneCommit {
    Kept,
    Released,
}

impl Compiler {
    /// Flush transaction. Callable from any thread - compile() is dispatched to main thread.
    pub(crate) fn commit(&self, runtime_ctx: &Arc<RuntimeContext>) -> Result<()> {
        self.commit_saying_how_far_it_got(runtime_ctx, TheHoldForOneCommit::Kept)
            .map_err(|failed| failed.refusal)
    }

    /// Release the transaction's hold for one commit and compile every operation logged, in
    /// the one step a commit holds its turn for, so no other commit takes them first; a failure
    /// says whether it came after the compile changed what runs.
    pub(crate) fn commit_releasing_the_hold_for_one_commit(
        &self,
        runtime_ctx: &Arc<RuntimeContext>,
    ) -> std::result::Result<(), ACommitFailed> {
        self.commit_saying_how_far_it_got(runtime_ctx, TheHoldForOneCommit::Released)
    }

    #[tracing::instrument(name = "compiler.commit", skip_all)]
    fn commit_saying_how_far_it_got(
        &self,
        runtime_ctx: &Arc<RuntimeContext>,
        the_hold: TheHoldForOneCommit,
    ) -> std::result::Result<(), ACommitFailed> {
        #[cfg(any(test, feature = "test-support"))]
        crate::core::runtime::graph_renders_and_commits_counted_for_a_test::count_a_commit_begun_of(
            runtime_ctx.loaded_stream_identity(),
        );
        let _one_commit_at_a_time = self.one_commit_at_a_time.lock();
        let held_since = match the_hold {
            TheHoldForOneCommit::Kept => *self.transaction_held_for_one_commit.lock(),
            TheHoldForOneCommit::Released => {
                self.transaction_held_for_one_commit.lock().take();
                None
            }
        };
        let operations = match held_since {
            Some(logged_before) => {
                self.take_the_config_updates_logged_under_the_hold(logged_before)
            }
            None => self.take_the_logged_operations_to_compile(),
        };
        if operations.is_empty() {
            tracing::info!("[commit] No pending operations");
            return Ok(());
        }

        tracing::debug!(
            "[commit] Processing {} pending operations (batched)",
            operations.len()
        );

        let compiled = Self::compile(
            Arc::clone(&self.graph),
            operations,
            runtime_ctx,
            &self.abandoned_node_threads,
        );
        self.configs_a_commit_is_applying.lock().clear();
        compiled
    }
}
