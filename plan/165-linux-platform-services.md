---
whoami: amos
name: "@github:tatolab/streamlib#165"
description: Linux — Platform services — audio clock, thread priority
dependencies:
  - "down:@github:tatolab/streamlib#163"
  - "down:@github:tatolab/streamlib#180"
  - "up:@github:tatolab/streamlib#166"
adapters:
  github: builtin
---

@github:tatolab/streamlib#165

Phase 3 of the Linux support plan. Linux equivalents of Apple platform services.

### AI context (2026-03-21)
- Phase 1 (Vulkan RHI) is complete — no blockers from RHI changes
- Audio clock and thread priority are independent of GPU work

### Needs implementation
- **Audio clock** — `timerfd_create(CLOCK_MONOTONIC)` + epoll, dedicated high-priority thread. `AudioClock` trait already abstract. New `linux/audio_clock.rs`
- **Thread priority** — `SCHED_FIFO` priority 80+ via `pthread_setschedparam`. May need `CAP_SYS_NICE`. New `linux/thread_priority.rs`

### Already correct (no work)
- Runtime thread dispatch — passthrough is correct for Linux
- Permissions — auto-grant is correct (device perms, not app prompts)
- Platform detection — already returns "Linux" / "Vulkan"

### Depends on
#163 (Vulkan RHI) — complete
