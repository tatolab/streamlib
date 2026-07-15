---
paths:
  - "sdk/streamlib-python/**"
  - "sdk/streamlib-python-native/**"
  - "sdk/streamlib-deno/**"
  - "sdk/streamlib-deno-native/**"
  - "packages/escalate/**"
---

# Polyglot

- **Python AND Deno land together.** Pipeline-level work (new processor + scenario, new escalate
  op end-to-end, new FD-passing story) ships both runtimes in the same PR, or files paired tickets
  that block each other and land in the same milestone. "Python first, Deno deferred" is the
  failure mode this rule prevents. The only legitimate split is schema-only / language-specific by
  construction — say so explicitly.
- **Schema changes regenerate all three runtimes.** An `escalate_*.yaml` (or any JTD schema) edit
  is followed by `cargo xtask generate-schemas` and a rebuild of Rust + Python + Deno so the wire
  shapes stay in lock-step.
- **Subprocess Vulkan is the import-side carve-out only** — `vkImportMemoryFdKHR` + bind + map,
  layout transitions on imported handles, timeline wait/signal. No allocation, no modifier choice,
  no kernel construction; everything privileged escalates to the host.
