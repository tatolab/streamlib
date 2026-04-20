---
whoami: amos
name: '@github:tatolab/streamlib#380'
adapters:
  github: builtin
description: '[BLOCKED — do not start] Umbrella: realign Python polyglot SDK to RHI-mediated GPU model — BLOCKED — queued behind #319 (GPU capability umbrella) and all its children, plus #347 DMA-BUF FD research and #369 polyglot acquire_texture. Do NOT pick up. Umbrella to rip the parallel GPU stack (IOSurface FFI, CGL helpers, XPC broker) out of libstreamlib_python_native and libs/streamlib-python, and route every GPU op through escalate IPC → host → GpuContext → RHI so platform-specific code stays inside the RHI''s platform subdirectories instead of the Python cdylib.'
github_issue: 380
blocked_by:
- '@github:tatolab/streamlib#319'
- '@github:tatolab/streamlib#347'
- '@github:tatolab/streamlib#369'
---

@github:tatolab/streamlib#380

See the GitHub issue for full context.

## Why this exists

The Python polyglot SDK predates the RHI stabilization. It carries its
own IOSurface FFI, CGL OpenGL helpers, and XPC broker client inside
`libstreamlib_python_native` — a parallel GPU stack that bypasses
`VulkanDevice` / `GpuContext`. That violates the engine model in
CLAUDE.md and frames the Linux port as "implement a second SDK
backend" rather than "turn on the Linux RHI backend and Python follows
for free."

## Shape of the fix

Python sees opaque surface handles only. All GPU allocation, resolution,
and host-visible mapping routes through escalate IPC → host → RHI.
`libstreamlib_python_native` shrinks to: msgpack I/O + escalate plumbing
+ host-visible pointer access. `cgl_context.py` moves out of the SDK
into per-example helpers. Examples needing their own GL/WebGPU context
carry that code at the project level.

## Sequencing

Gated behind #319 and its children closing. Do NOT pick up before. A
naive Linux port of the current Python SDK should also land (or be
explicitly skipped) first so there is something concrete to rip out and
a working E2E reference to simplify.

## Branch

`refactor/python-sdk-rhi-realignment` from `main` once unblocked.
