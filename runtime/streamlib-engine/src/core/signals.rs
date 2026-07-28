// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

#[cfg(all(unix, not(target_os = "macos")))]
use crate::core::runtime::request_runtime_shutdown;
use std::sync::atomic::{AtomicBool, Ordering};

static SIGNAL_HANDLER_INSTALLED: AtomicBool = AtomicBool::new(false);

/// Install native signal handlers for shutdown signals
///
/// Captures SIGTERM and SIGINT (Ctrl+C). Unix/Linux funnels them into
/// [`request_runtime_shutdown`](crate::core::runtime::request_runtime_shutdown)
/// like every other shutdown boundary; macOS instead routes through
/// `NSApplication.terminate`, which reaches the same teardown via
/// `applicationWillTerminate` and never touches the funnel. This function
/// spawns a background thread to handle signals without blocking the signal
/// handler.
///
/// # Platform Support
/// - Unix/Linux: Uses libc signal handling via signal-hook
/// - macOS: Uses ctrlc crate (works with NSApplication GUI apps) + signal-hook for SIGTERM
/// - Windows: Not yet implemented (would use SetConsoleCtrlHandler)
///
/// # Safety
/// Signal handlers must be async-signal-safe. We immediately write to a pipe
/// and handle the event in a separate thread to avoid restrictions.
pub fn install_signal_handlers() -> std::io::Result<()> {
    // Only install once
    if SIGNAL_HANDLER_INSTALLED.swap(true, Ordering::SeqCst) {
        tracing::warn!("Signal handlers already installed, skipping");
        return Ok(());
    }

    #[cfg(target_os = "macos")]
    {
        install_macos_signal_handlers()?;
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        install_unix_signal_handlers()?;
    }

    #[cfg(windows)]
    {
        install_windows_signal_handlers()?;
    }

    Ok(())
}

/// Force reinstall signal handlers (useful if external libraries override them)
///
/// Some libraries (like CoreAudio) may install their own signal handlers that override ours.
/// Call this AFTER initializing processors to re-claim signal handling.
pub fn reinstall_signal_handlers() -> std::io::Result<()> {
    tracing::info!("Force reinstalling signal handlers...");

    #[cfg(target_os = "macos")]
    {
        install_macos_signal_handlers()?;
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        install_unix_signal_handlers()?;
    }

    #[cfg(windows)]
    {
        install_windows_signal_handlers()?;
    }

    Ok(())
}

#[cfg(target_os = "macos")]
fn install_macos_signal_handlers() -> std::io::Result<()> {
    // Use ctrlc crate for Ctrl+C - it works reliably with NSApplication
    ctrlc::set_handler(move || {
        tracing::info!("Ctrl+C received, triggering graceful shutdown");
        trigger_macos_termination();
    })
    .map_err(std::io::Error::other)?;

    // Still use signal-hook for SIGTERM (system shutdown, kill command)
    install_sigterm_handler_macos()?;

    tracing::info!("macOS signal handlers installed (Ctrl+C via ctrlc, SIGTERM via signal-hook)");
    Ok(())
}

#[cfg(target_os = "macos")]
fn install_sigterm_handler_macos() -> std::io::Result<()> {
    use signal_hook::consts::signal::SIGTERM;
    use signal_hook::flag;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    // Use flag approach for SIGTERM - simpler than pipe
    let term_flag = Arc::new(AtomicBool::new(false));
    flag::register(SIGTERM, Arc::clone(&term_flag))?;

    // Monitor the flag in a background thread
    std::thread::spawn(move || {
        loop {
            if term_flag.load(Ordering::Relaxed) {
                tracing::info!("SIGTERM received, triggering graceful shutdown");
                trigger_macos_termination();
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    });

    Ok(())
}

#[cfg(all(unix, not(target_os = "macos")))]
fn install_unix_signal_handlers() -> std::io::Result<()> {
    use signal_hook::consts::signal::{SIGINT, SIGTERM};
    use signal_hook::iterator::Signals;

    // `Signals` owns the async-signal-safe self-pipe and yields the delivered
    // signal NUMBER. `low_level::pipe::register` cannot: its handler writes a
    // fixed `b"X"` wakeup byte, so a reader can never tell SIGINT from SIGTERM
    // — and the shutdown `reason` this funnels into is operator-facing
    // attribution.
    let mut signals = Signals::new([SIGTERM, SIGINT])?;

    // Detached: it runs for the process lifetime, and there is no shutdown
    // path that joins it.
    std::thread::Builder::new()
        .name("signal-handler".to_string())
        .spawn(move || {
            tracing::debug!("Signal handler thread started, waiting for signals");
            for signal in signals.forever() {
                let signal_name =
                    signal_hook::low_level::signal_name(signal).unwrap_or("unrecognized signal");
                if let Err(error) = request_runtime_shutdown(&format!("posix signal {signal_name}"))
                {
                    tracing::error!(%error, "Signal handler: runtime-shutdown request failed");
                }
            }
            tracing::debug!("Signal handler thread exiting");
        })?;

    tracing::info!("Native signal handlers installed (SIGTERM, SIGINT)");
    Ok(())
}

#[cfg(windows)]
fn install_windows_signal_handlers() -> std::io::Result<()> {
    // TODO: Implement Windows signal handling using SetConsoleCtrlHandler
    tracing::warn!("Windows signal handling not yet implemented");
    Ok(())
}

#[cfg(target_os = "macos")]
fn trigger_macos_termination() {
    use dispatch2::DispatchQueue;

    // Call NSApplication.terminate() on the main thread
    // This will trigger applicationWillTerminate: for graceful shutdown
    DispatchQueue::main().exec_async(move || {
        use objc2::MainThreadMarker;
        use objc2_app_kit::NSApplication;

        if let Some(mtm) = MainThreadMarker::new() {
            let app = NSApplication::sharedApplication(mtm);
            tracing::info!("Signal handler: Calling NSApplication.terminate()");
            app.terminate(None);
        } else {
            tracing::error!(
                "Signal handler: Not on main thread, cannot call NSApplication.terminate()"
            );
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_signal_handler_install_once() {
        // Reset flag for test
        SIGNAL_HANDLER_INSTALLED.store(false, Ordering::SeqCst);

        let result1 = install_signal_handlers();
        let result2 = install_signal_handlers();

        assert!(result1.is_ok());
        assert!(result2.is_ok()); // Should succeed but not install twice
    }
}
