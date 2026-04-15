---
whoami: amos
name: Migrate RHI from ash to vulkanalia
status: pending
description: Replace ash 0.38 (Vulkan 1.3) with vulkanalia 0.35 (Vulkan 1.4) throughout the RHI. Branch refactor/ash-to-vulkanalia from main.
github_issue: 252
dependencies:
  - "down:@github:tatolab/streamlib#251"
adapters:
  github: builtin
---

@github:tatolab/streamlib#252

## Branch

Create `refactor/ash-to-vulkanalia` from `main` (after #251 merges).

## Dependencies

Use tatolab fork until upstream accepts VMA 3.3.0:
```toml
vulkanalia = { git = "https://github.com/tatolab/vulkanalia.git", branch = "tatolab/update-vma-3.3.0-patched", features = ["libloading", "provisional"] }
vulkanalia-vma = { git = "https://github.com/tatolab/vulkanalia.git", branch = "tatolab/update-vma-3.3.0-patched" }

[patch.crates-io]
vulkanalia = { git = "https://github.com/tatolab/vulkanalia.git", branch = "tatolab/update-vma-3.3.0-patched" }
vulkanalia-sys = { git = "https://github.com/tatolab/vulkanalia.git", branch = "tatolab/update-vma-3.3.0-patched" }
```

## Knowledge to Carry Forward

These learnings from the vulkan video branches must be incorporated during migration:
- Graphics queue mutex for concurrent submissions (commit 9e53362)
- NV12 is 12bpp not 16bpp for pixel buffer allocation (commit 78ab0ff)
- Secondary graphics queue for concurrent decode submissions (commit 019e516)
- Video encode/decode queue family discovery (commit 3f86402)
- VK_KHR_video_maintenance1 extension enablement (commit ddbb7c5)
- samplerYcbcrConversion device feature enablement (commit 429bb3b)
- Vulkan API version 1.4 (was 1.3.296)
