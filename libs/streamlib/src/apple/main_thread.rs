//! Main thread dispatcher for macOS
//!
//! macOS requires certain operations (AVFoundation, NSWindow) to run on the main thread.
//! This module provides utilities to dispatch work from tokio worker threads to the main thread.

use crate::core::Result;
use std::sync::mpsc;

/// Execute a function on the main thread and wait for the result
///
/// This function dispatches work to the macOS main queue (via Grand Central Dispatch)
/// and blocks the current thread until the work completes. This is safe to call from
/// tokio worker threads.
///
/// # Arguments
/// * `f` - Function to execute on the main thread
///
/// # Returns
/// The result of the function execution
///
/// # Example
///
/// ```ignore
/// // From a tokio worker thread
/// let camera = execute_on_main_thread(|| {
///     CameraProcessor::new()  // Requires main thread
/// }).await?;
/// ```
pub fn execute_on_main_thread<F, T>(f: F) -> Result<T>
where
    F: FnOnce() -> Result<T> + Send + 'static,
    T: Send + 'static,
{
    use dispatch2::DispatchQueue;

    // Create a channel to receive the result
    let (tx, rx) = mpsc::sync_channel(1);

    // Dispatch to main queue (synchronously - blocks until complete)
    DispatchQueue::main().exec_sync(move || {
        let result = f();
        let _ = tx.send(result);
    });

    // Receive the result (will be immediate since exec_sync blocks)
    rx.recv()
        .map_err(|e| crate::core::StreamError::Runtime(format!("Main thread dispatch failed: {}", e)))?
}

/// Execute a function on the main thread asynchronously
///
/// This is an async-friendly version that doesn't block the current thread.
/// It uses tokio's spawn_blocking to avoid blocking the async executor.
///
/// # Arguments
/// * `f` - Function to execute on the main thread
///
/// # Returns
/// Future that resolves to the function result
///
/// # Example
///
/// ```ignore
/// // From an async context
/// let camera = execute_on_main_thread_async(|| {
///     CameraProcessor::new()
/// }).await?;
/// ```
pub async fn execute_on_main_thread_async<F, T>(f: F) -> Result<T>
where
    F: FnOnce() -> Result<T> + Send + 'static,
    T: Send + 'static,
{
    // Use spawn_blocking to avoid blocking the tokio executor
    tokio::task::spawn_blocking(move || execute_on_main_thread(f))
        .await
        .map_err(|e| crate::core::StreamError::Runtime(format!("Task join error: {}", e)))?
}
