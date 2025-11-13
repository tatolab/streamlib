//! Native signal handling for Unix/Linux platforms
//!
//! Captures OS signals (SIGTERM, SIGINT) and publishes them to the event bus,
//! enabling event-driven shutdown without external dependencies.

#[cfg(all(unix, not(target_os = "macos")))]
use crate::core::pubsub::{Event, RuntimeEvent, EVENT_BUS};
use std::sync::atomic::{AtomicBool, Ordering};

static SIGNAL_HANDLER_INSTALLED: AtomicBool = AtomicBool::new(false);

/// Install native signal handlers for shutdown signals
///
/// Captures SIGTERM and SIGINT (Ctrl+C) and publishes RuntimeEvent::RuntimeShutdown
/// to the global EVENT_BUS. This function spawns a background thread to handle
/// signals without blocking the signal handler.
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

#[cfg(target_os = "macos")]
fn install_macos_signal_handlers() -> std::io::Result<()> {
    // Use ctrlc crate for Ctrl+C - it works reliably with NSApplication
    ctrlc::set_handler(move || {
        tracing::info!("Ctrl+C received, triggering graceful shutdown");
        trigger_macos_termination();
    })
    .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;

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
    use std::os::unix::io::AsRawFd;
    use std::os::unix::net::UnixStream;

    // Create a pipe for async-signal-safe communication
    let (mut reader, writer) = UnixStream::pair()?;
    let writer_fd = writer.as_raw_fd();

    // Spawn thread to handle signals from pipe
    let handler_thread = std::thread::Builder::new()
        .name("signal-handler".to_string())
        .spawn(move || {
            use std::io::Read;
            let mut buf = [0u8; 1];

            tracing::debug!("Signal handler thread started, waiting for signals");

            loop {
                tracing::trace!("Signal handler: Waiting to read from pipe");
                match reader.read(&mut buf) {
                    Ok(0) => {
                        // Pipe closed, exit thread
                        tracing::debug!("Signal handler pipe closed, exiting thread");
                        break;
                    }
                    Ok(n) => {
                        if n > 0 {
                            let signal = buf[0];
                            tracing::info!("Signal handler: Received signal {}, publishing shutdown event", signal);

                            // Publish shutdown event directly to event bus
                            let shutdown_event = Event::RuntimeGlobal(RuntimeEvent::RuntimeShutdown);
                            EVENT_BUS.publish(&shutdown_event.topic(), &shutdown_event);
                        }
                    }
                    Err(e) => {
                        tracing::error!("Signal handler thread error: {}", e);
                        break;
                    }
                }
            }
            tracing::debug!("Signal handler thread exiting");
        })?;

    // Detach the thread so it continues running independently
    std::mem::forget(handler_thread);

    // Install signal handlers using signal_hook
    install_sigterm_handler(writer_fd)?;
    install_sigint_handler(writer_fd)?;

    tracing::info!("Native signal handlers installed (SIGTERM, SIGINT)");
    Ok(())
}

#[cfg(all(unix, not(target_os = "macos")))]
fn install_sigterm_handler(pipe_fd: std::os::fd::RawFd) -> std::io::Result<()> {
    use signal_hook::consts::signal::*;
    use signal_hook::low_level::pipe;

    // Register SIGTERM to write to pipe
    pipe::register(SIGTERM, pipe_fd)?;

    Ok(())
}

#[cfg(all(unix, not(target_os = "macos")))]
fn install_sigint_handler(pipe_fd: std::os::fd::RawFd) -> std::io::Result<()> {
    use signal_hook::consts::signal::*;
    use signal_hook::low_level::pipe;

    // Register SIGINT (Ctrl+C) to write to pipe
    pipe::register(SIGINT, pipe_fd)?;

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
            tracing::error!("Signal handler: Not on main thread, cannot call NSApplication.terminate()");
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
