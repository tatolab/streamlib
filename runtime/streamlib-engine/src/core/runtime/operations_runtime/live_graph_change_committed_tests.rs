// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::sync::Arc;

use super::commit_the_graph_edits_held_for_one_commit;
use crate::core::compiler::{Compiler, WhatTheHeldCommitFound};

/// A commit of held edits that finds the hold already taken back, by a stop forced past an
/// open batch, compiles nothing and says so, so the close refuses rather than answering success
/// for edits none of which was made.
///
/// Mental-revert: a commit that releases a hold it does not find answers as though the edits
/// were made, and the caller writes them to the project.
#[test]
fn a_held_commit_finding_its_hold_taken_back_says_so() {
    let compiler = Arc::new(Compiler::new());
    compiler.hold_the_transaction_for_one_commit();
    assert!(compiler.take_back_the_transaction_held_for_one_commit());

    let found = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("current-thread runtime")
        .block_on(commit_the_graph_edits_held_for_one_commit(&compiler, None));

    assert!(
        matches!(found, Ok(WhatTheHeldCommitFound::TheHoldTakenBack)),
        "the commit finds no hold"
    );
}
