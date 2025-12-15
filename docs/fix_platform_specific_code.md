# Fix Platform-Specific Code in Core

## Problem

Some files in `libs/streamlib/src/core/` have unguarded imports from `crate::apple::*` that will cause compilation failures on non-macOS platforms.

## Files That Need Fixing

| File | Line | Issue |
|------|------|-------|
| `streaming/rtp.rs` | 4 | Unguarded `use crate::apple::videotoolbox::{parse_nal_units, EncodedVideoFrame};` |

### Details: `streaming/rtp.rs`

The file imports Apple-specific types without any `#[cfg]` guard:

```rust
use crate::apple::videotoolbox::{parse_nal_units, EncodedVideoFrame};
```

This import is used by `convert_video_to_samples()` (lines 10-38), which is then:
- Re-exported via `streaming/mod.rs` line 16
- Re-exported via `core/mod.rs` line 43 (`pub use streaming::*;`)
- Re-exported via `lib.rs` line 33

**Impact**: Will fail to compile on Linux/Windows.

**Fix**: Add `#[cfg(any(target_os = "macos", target_os = "ios"))]` guards:
1. Guard the import at line 4
2. Guard the `convert_video_to_samples` function (lines 10-38)
3. Guard the re-export in `streaming/mod.rs`
4. Guard the re-export in `lib.rs`

## Files Already Using Correct Pattern

All other files in `core/` that use `crate::apple::*` have proper `#[cfg]` guards:

- `media_clock.rs` - `#[cfg(target_os = "macos")]` on line 4
- `runtime/runtime.rs` - `#[cfg(target_os = "macos")]` blocks at lines 329, 653
- `signals.rs` - `#[cfg(target_os = "macos")]` blocks throughout
- `context/runtime_context.rs` - `#[cfg(target_os = "macos")]` blocks at lines 50, 84
- `compiler_ops/start_processor_op.rs` - `#[cfg(any(target_os = "macos", target_os = "ios"))]` at line 136
- `processors/camera.rs` - `#[cfg(target_os = "macos")]` at line 6
- `processors/display.rs` - `#[cfg(target_os = "macos")]` at line 6
- `processors/audio_output.rs` - `#[cfg(target_os = "macos")]` at line 6
- `processors/audio_capture.rs` - `#[cfg(target_os = "macos")]` at line 6
- `processors/mp4_writer.rs` - `#[cfg(target_os = "macos")]` at line 6
