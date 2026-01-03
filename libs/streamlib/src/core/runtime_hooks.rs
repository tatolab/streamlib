// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::path::Path;
use std::sync::OnceLock;

use crate::Result;

/// Hook called synchronously when a StreamRuntime is created.
///
/// External crates (e.g., streamlib-python, streamlib-typescript) register
/// implementations via `inventory::submit!`. The runtime calls all registered
/// hooks during `StreamRuntime::new()`.
///
/// Use for: building language wheels, warming caches, verifying toolchains.
///
/// # Example
///
/// ```ignore
/// pub struct PythonRuntimeInitHook;
///
/// impl RuntimeInitHook for PythonRuntimeInitHook {
///     fn name(&self) -> &'static str { "Python" }
///
///     fn on_runtime_init(&self, streamlib_home: &Path) -> Result<()> {
///         // Build wheel, warm cache, etc.
///         Ok(())
///     }
/// }
///
/// inventory::submit! {
///     RuntimeInitHookRegistration::new::<PythonRuntimeInitHook>()
/// }
/// ```
pub trait RuntimeInitHook: Send + Sync + Default + 'static {
    /// Human-readable name for logging (e.g., "Python", "TypeScript").
    fn name(&self) -> &'static str;

    /// Called once per process at first runtime creation.
    ///
    /// Runs synchronously - blocks until complete.
    /// Errors cause runtime creation to fail.
    fn on_runtime_init(&self, streamlib_home: &Path) -> Result<()>;
}

/// Registration struct for RuntimeInitHook implementations.
/// Uses function pointers to avoid non-const Box::new in statics.
pub struct RuntimeInitHookRegistration {
    pub name_fn: fn() -> &'static str,
    pub init_fn: fn(&Path) -> Result<()>,
}

impl RuntimeInitHookRegistration {
    /// Create a registration for a RuntimeInitHook implementation.
    pub const fn new<T: RuntimeInitHook>() -> Self {
        Self {
            name_fn: || T::default().name(),
            init_fn: |path| T::default().on_runtime_init(path),
        }
    }
}

inventory::collect!(RuntimeInitHookRegistration);

/// Run all registered init hooks.
///
/// Called from `StreamRuntime::new()`. Uses `OnceLock` to ensure hooks
/// only run once per process, even if multiple runtimes are created.
pub fn run_init_hooks(streamlib_home: &Path) -> Result<()> {
    use std::sync::atomic::{AtomicBool, Ordering};

    static INIT_DONE: AtomicBool = AtomicBool::new(false);
    static INIT_FAILED: OnceLock<String> = OnceLock::new();

    // If already completed successfully, return Ok
    if INIT_DONE.load(Ordering::Acquire) {
        return Ok(());
    }

    // If previously failed, return the cached error
    if let Some(err_msg) = INIT_FAILED.get() {
        return Err(crate::StreamError::Runtime(err_msg.clone()));
    }

    // Run hooks (first caller wins due to OnceLock semantics)
    let result = run_hooks_inner(streamlib_home);

    match &result {
        Ok(()) => {
            INIT_DONE.store(true, Ordering::Release);
        }
        Err(e) => {
            let _ = INIT_FAILED.set(e.to_string());
        }
    }

    result
}

fn run_hooks_inner(streamlib_home: &Path) -> Result<()> {
    let hooks: Vec<_> = inventory::iter::<RuntimeInitHookRegistration>().collect();

    if hooks.is_empty() {
        tracing::debug!("No RuntimeInitHook implementations registered");
        return Ok(());
    }

    tracing::info!("Running {} init hook(s)", hooks.len());

    for hook in hooks {
        let name = (hook.name_fn)();
        tracing::info!("Running init hook: {}", name);
        (hook.init_fn)(streamlib_home)?;
        tracing::info!("Init hook '{}' completed", name);
    }

    Ok(())
}
