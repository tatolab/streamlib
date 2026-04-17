---
whoami: amos
name: Clean Exit for Processor-Based Examples
status: completed
description: Fix display window hang on exit — Ctrl+C, SIGTERM, and StreamRuntime shutdown must cleanly close the winit event loop and terminate the process. Branch fix/clean-exit from main.
github_issue: 236
dependencies:
  - "down:@github:tatolab/streamlib#252"
adapters:
  github: builtin
---

@github:tatolab/streamlib#236

## Branch

Create `fix/clean-exit` from `main` (after #252 merges).

## Problem

Every example using DisplayProcessor on Linux hangs on exit:
- Ctrl+C stops the camera from sending frames but the window never closes and the process stays alive
- SIGTERM is ignored — winit's event loop blocks and doesn't respond
- Only Ctrl+Z + `kill -9 %1` or `pkill -9` terminates the process
- Affects: camera-display, moq-av-subscribe, and any example using DisplayProcessor

### Impact on agent workflow

AI agents running examples for validation cannot determine if the example completed or is stuck. The hang forces manual intervention and erodes confidence in automated testing. This blocks reliable E2E validation for all GPU pipeline work (#253, #254).

## Scope

The root cause is in the interaction between:
- winit's `EventLoop` on Linux (X11/Wayland) — once `run()` is called, the loop owns the thread
- StreamRuntime shutdown signaling via PUBSUB — the shutdown event may arrive but winit doesn't yield control to process it
- Signal handlers (SIGINT/SIGTERM) — winit may install its own signal handlers that conflict with the runtime's

## Testing goals

- Ctrl+C exits the process within 2 seconds (no stranded window)
- SIGTERM exits the process within 2 seconds
- StreamRuntime shutdown event closes the display window and exits
- `STREAMLIB_DISPLAY_FRAME_LIMIT` auto-exit works cleanly (no hang after last frame)
- No regression: normal operation (camera streaming, display rendering) unaffected
