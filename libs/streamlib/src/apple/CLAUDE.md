# Apple Platform Implementation

**DO NOT use this module directly in your code!**

## What This Is

This module contains **macOS/iOS-specific implementations** of streamlib processors:

- `AppleCameraProcessor` - CoreVideo/AVFoundation camera capture
- `AppleDisplayProcessor` - Metal rendering to NSWindow
- `AppleAudioCaptureProcessor` - CoreAudio microphone input
- `AppleAudioOutputProcessor` - CoreAudio speaker output

## Why You Shouldn't Use This

These are **internal implementation details**. They:
- Are not part of the public API
- May change without notice
- Don't work on other platforms
- Are automatically selected by the platform-agnostic layer

## What You Should Use Instead

**Use the platform-agnostic traits from `core::processors` instead:**

```rust
// ❌ WRONG - Platform-specific, breaks on Linux/Windows
use streamlib::apple::AppleCameraProcessor;
let camera = AppleCameraProcessor::new(None)?;

// ✅ CORRECT - Platform-agnostic, works everywhere
use streamlib::CameraProcessor;
let camera = CameraProcessor::new(None)?;
```

The `streamlib` crate automatically routes to `AppleCameraProcessor` on macOS/iOS, and to the appropriate implementation on other platforms.

## When To Look At This Code

**Only when:**
- Debugging platform-specific issues
- Implementing a new platform
- Understanding Metal/CoreAudio integration
- Contributing to streamlib internals

## Architecture

```text
User Code
    ↓
streamlib (facade crate)
    ↓
core::processors (traits - CameraProcessor, AudioCaptureProcessor, etc.)
    ↓
apple::processors (implementations - AppleCameraProcessor, etc.)
    ↓
Metal/CoreAudio/AVFoundation (platform APIs)
```

**The user only sees the top layer - everything else is internal.**

## Platform-Specific Details

This implementation uses:
- **Metal** - GPU acceleration, texture management
- **CoreAudio** - Low-latency audio I/O
- **AVFoundation** - Camera/video capture
- **CoreVideo** - Video frame buffers (CVPixelBuffer)
- **IOSurface** - Zero-copy GPU↔CPU texture sharing
- **GCD** - Dispatch queues for threading

If you need to understand how streamlib achieves <10ms latency on macOS, this is where to look.

## Related Files

- `../core/processors/` - The traits you should use
- `../../lib.rs` - Public API facade
- `../linux/` - Linux implementations (future)
- `../windows/` - Windows implementations (future)
