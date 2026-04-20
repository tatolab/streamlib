---
whoami: amos
name: "Polyglot IPC: escalate-on-behalf for Python/Deno processors"
status: in_review
description: Extend subprocess-host processors to accept escalate IPC requests from Python/Deno. Subprocess sees only a sandbox; IPC is its escalate channel, routed through the host's serialized queue.
github_issue: 325
dependencies:
  - "down:Restrict GpuContextLimitedAccess API surface to safe ops"
adapters:
  github: builtin
---

@github:tatolab/streamlib#325

## Branch

`feat/polyglot-escalate-ipc` from `main`.

## Steps

1. Define IPC schema: `EscalateRequest { op: AcquireTexture { … } | AcquirePixelBuffer { … } | ReleaseHandle { id } | … }`, `EscalateResponse { handle_id, error? }`. Place in `schemas/` per existing JTD conventions.
2. In `SubprocessHostProcessor` (Python) and `DenoSubprocessHostProcessor` (Deno), add control-channel handling: receive `EscalateRequest`, call `self.sandbox.escalate(|full| full.acquire_*(…))`, ship the resulting handle / ID back.
3. Update Python bindings: add `ctx.escalate(op)` helper that awaits the response over IPC.
4. Update Deno bindings likewise.
5. Handle lifetime: allocations live in the host's pools; subprocess references by ID and releases via `ReleaseHandle` (or drop signal on subprocess death).

## Verification

- Python example: processor requests a new-shape pixel buffer mid-stream via escalate, writes to it, returns it.
- Deno equivalent.
- Concurrency: host Rust + subprocess escalate simultaneously — serialized correctly, no race, no `DEVICE_LOST`.

## Parent

#319
