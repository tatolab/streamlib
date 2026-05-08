// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::sync::Arc;

use parking_lot::Mutex;

use super::PendingOperation;

/// Handle for logging operations to the compiler transaction.
pub struct CompilerTransactionHandle {
    inner: Arc<Mutex<Vec<PendingOperation>>>,
}

impl CompilerTransactionHandle {
    pub(crate) fn new(inner: Arc<Mutex<Vec<PendingOperation>>>) -> Self {
        Self { inner }
    }

    pub fn log(&self, op: PendingOperation) {
        self.inner.lock().push(op);
    }
}
