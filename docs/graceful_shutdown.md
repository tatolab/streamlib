# macOS Graceful Shutdown Implementation

## Overview
This document explains how graceful shutdown works in streamlib on macOS.

## Critical Discovery: stdin Blocking Issues

### Issue 1: Background Thread stdin Blocking

**Problem**: Blocking stdin on a background thread prevents Ctrl+C from being delivered to the main thread.

When a background thread calls `stdin.lock().lines()` and blocks waiting for input, the main thread can no longer receive Ctrl+C (SIGINT) signals properly. This is why:
- `camera-display` (no stdin blocking) → Ctrl+C works ✅
- `camera-audio-recorder` (with 'q' + Enter handler that blocks stdin) → Ctrl+C fails ❌

**Root Cause**: The OS delivers keyboard signals (including Ctrl+C) to the thread that owns stdin. When a background thread locks stdin, it "steals" signal delivery from the main thread.

**Solution**: Do NOT use stdin blocking on background threads in applications that need Ctrl+C support. Let the main thread handle all keyboard input via the NSApplication event loop and signal handlers.

### Issue 2: Using `tee` Breaks Ctrl+C

**Problem**: Running with `tee` to capture logs prevents Ctrl+C from working.

```bash
# ❌ Does NOT work - Ctrl+C is intercepted by tee
RUST_LOG=debug cargo run 2>&1 | tee output.log

# ✅ Works correctly
RUST_LOG=debug cargo run

# ✅ Alternative: redirect to file (Ctrl+C still works)
RUST_LOG=debug cargo run 2>&1 > output.log
```

**Root Cause**: `tee` creates a pipeline that intercepts signals. The Ctrl+C signal is sent to the entire process group, but the pipeline handling can prevent it from reaching the application correctly.

**Solution**: Use direct output redirection (`>`) instead of `tee` if you need to capture logs. Or run without redirection and copy logs from terminal afterward.

## Shutdown Flow

### 1. Signal Reception (Ctrl+C or SIGTERM)
- **File**: `libs/streamlib/src/core/signals.rs`
- Ctrl+C is handled by the `ctrlc` crate (works with NSApplication)
- SIGTERM is handled by `signal-hook` crate
- Both trigger `trigger_macos_termination()` which dispatches to main thread

### 2. Main Thread Termination
- **File**: `libs/streamlib/src/apple/runtime_ext.rs:179-196`
- `trigger_macos_termination()` calls `NSApplication.terminate(None)` on main thread
- This is the macOS-standard way to request application termination

### 3. Application Will Terminate Callback
- **File**: `libs/streamlib/src/apple/runtime_ext.rs:21-38`
- NSApplicationDelegate method `applicationWillTerminate:` is invoked
- **IMPORTANT**: Does NOT log anything - ALL I/O (stdout/stderr/stdin) is shutting down
- Even `eprintln!()` will panic with SIGABRT during shutdown
- Spawns background thread to run shutdown callback (avoids deadlock)
- Background thread CAN log (tracing) since it's not in the termination path

### 4. Shutdown Callback Execution
- **File**: `libs/streamlib/src/apple/runtime_ext.rs:55-127`
- **Step 1**: Publishes `RuntimeEvent::RuntimeShutdown` to EVENT_BUS
  - Pull mode processors using `shutdown_aware_loop` will exit their loops
- **Step 2**: Sends shutdown signals via channels to all processor threads
  - Push mode processors receive shutdown signal
  - Wakeup event sent to unblock waiting processors
- **Step 3**: Joins all processor threads with 120-second timeout
  - Waits for each processor's teardown to complete
  - Updates processor status to `Stopped`

### 5. Processor Teardown
- **File**: `libs/streamlib/src/apple/processors/mp4_writer.rs:559-613`
- MP4 writer's `teardown()` method is called
- Finalizes video/audio inputs: `finishWritingWithCompletionHandler()`
- Waits for completion via channel (blocks until AVAssetWriter completes)
- Ensures MP4 file is properly written and playable

## Expected Console Output on Ctrl+C

When you press Ctrl+C, you should see:

```
[timestamp] INFO streamlib::core::signals: Ctrl+C received, triggering graceful shutdown
[timestamp] INFO streamlib::core::signals: Signal handler: Calling NSApplication.terminate()
[timestamp] INFO Shutdown callback: Published RuntimeShutdown event to EVENT_BUS
[timestamp] INFO Shutdown callback: Sent shutdown signals to 5 processors
[timestamp] INFO [camera] Thread stopped successfully
[timestamp] INFO [audio-capture] Thread stopped successfully
[timestamp] INFO [resampler] Thread stopped successfully
[timestamp] INFO [channel-converter] Thread stopped successfully
[timestamp] INFO [mp4-writer] Thread stopped successfully
[timestamp] INFO Shutdown callback: Completed processor shutdown
```

Note: `applicationWillTerminate` itself produces NO output (would cause SIGABRT), but the shutdown callback (running on background thread) can log normally.

## Key Design Decisions

### Why Background Thread in `applicationWillTerminate`?
- Processor threads may dispatch work to main thread (e.g., AVAssetWriter operations)
- If we block main thread waiting for processors, and a processor tries to dispatch to main thread, we deadlock
- Solution: Run shutdown callback on background thread, main thread waits for it

### Why No Logging in `applicationWillTerminate`?
- ALL I/O streams (stdout, stderr, stdin) are being shut down when `applicationWillTerminate` is called
- Even `eprintln!()` (direct stderr write) will panic with SIGABRT
- Using `tracing::info!()` will also panic
- The background thread running the shutdown callback CAN log safely (not in termination path)

### Why 120-Second Timeout?
- AVAssetWriter finalization can take time for large files
- Audio/video buffer flushing needs time to complete
- User suggested 2 minutes as reasonable upper bound
- If timeout occurs, thread is forcefully terminated when process exits

## Testing Checklist

- [ ] Press Ctrl+C during recording
- [ ] Verify shutdown messages appear in console (via eprintln)
- [ ] Verify no crash occurs (no SIGABRT)
- [ ] Verify `recording.mp4` exists and is playable
- [ ] Verify video/audio are synchronized in final file
- [ ] Verify graceful shutdown completes within reasonable time (<5 seconds for short recordings)

## Related Files

- `libs/streamlib/src/core/signals.rs` - Signal handlers (Ctrl+C, SIGTERM)
- `libs/streamlib/src/apple/runtime_ext.rs` - NSApplicationDelegate, shutdown callback
- `libs/streamlib/src/core/runtime.rs` - Processor management, thread joining
- `libs/streamlib/src/apple/processors/mp4_writer.rs` - MP4 finalization in teardown()
- `libs/streamlib/src/core/loop_utils.rs` - Pull mode shutdown via EVENT_BUS
