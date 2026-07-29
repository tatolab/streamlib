---
paths:
  - "sdk/streamlib-python/**"
  - "sdk/streamlib-python-native/**"
  - "sdk/streamlib-deno/**"
  - "sdk/streamlib-deno-native/**"
  - "packages/escalate/**"
---

# Polyglot

- **Runtime parity is a plan decision, not a per-PR mandate.** The architecture plan
  (`docs/plan/`) states which surfaces require Python/Deno parity and which runtime leads during
  MVP. Where the plan marks a surface parity-required, both runtimes land together; everywhere
  else a single runtime may lead and the lag is expected, not a ticket.
- **Schema changes regenerate all three runtimes.** An `escalate_*.yaml` (or any JTD schema) edit
  is followed by `cargo xtask generate-schemas` and a rebuild of Rust + Python + Deno so the wire
  shapes stay in lock-step.
- **Subprocess Vulkan is the import-side carve-out only** — `vkImportMemoryFdKHR` + bind + map,
  layout transitions on imported handles, timeline wait/signal. No allocation, no modifier choice,
  no kernel construction; everything privileged escalates to the host.
