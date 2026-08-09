---
paths:
  - "sdk/streamlib-python-wheel/**"
  - "runtime/streamlib-ipc-types/**"
  - "packages/escalate/**"
---

# Polyglot

- **One Python processor, one helper process, one GIL** — hosting a processor in the app's
  interpreter is a STOP-WORK violation. See `.claude/rules/placement.md`.
- **Python is the sole focus runtime** (`docs/plan/ARCHITECTURE.md` §Language SDKs & parity).
  TypeScript authoring is paused, not rejected, and a future SDK follows the same
  importable-library model — so a surface built for Python today owes nothing to a second
  runtime, and "parity" never means shipping two SDKs in one change.
- **A schema change regenerates every consumer of that schema.** An `escalate_*.yaml` (or any
  JTD schema) edit is followed by `cargo xtask generate-schemas` and a rebuild of the Rust
  parent and the Python wheel together, so the two halves of the escalate wire stay in
  lock-step.
- **Helper-process Vulkan is the import-side carve-out only** — `vkImportMemoryFdKHR` + bind +
  map, layout transitions on imported handles, timeline wait/signal. No allocation, no modifier
  choice, no kernel construction; everything privileged escalates to the parent.
