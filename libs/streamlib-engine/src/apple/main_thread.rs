// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// TODO(@jonathan): Main thread module has unused utilities (execute_on_main_thread(), execute_on_main_thread_async())
// Review if these GCD dispatch queue utilities are needed for future UI features or can be removed
#![allow(dead_code)]

use crate::core::Result;
use std::sync::mpsc;

pub fn execute_on_main_thread<F, T>(f: F) -> Result<T>
where
    F: FnOnce() -> Result<T> + Send + 'static,
    T: Send + 'static,
{
    use dispatch2::DispatchQueue;

    let (tx, rx) = mpsc::sync_channel(1);

    DispatchQueue::main().exec_sync(move || {
        let result = f();
        let _ = tx.send(result);
    });

    rx.recv().map_err(|e| {
        crate::core::StreamError::Runtime(format!("Main thread dispatch failed: {}", e))
    })?
}

pub async fn execute_on_main_thread_async<F, T>(f: F) -> Result<T>
where
    F: FnOnce() -> Result<T> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(move || execute_on_main_thread(f))
        .await
        .map_err(|e| crate::core::StreamError::Runtime(format!("Task join error: {}", e)))?
}
