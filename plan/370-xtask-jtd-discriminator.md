---
whoami: amos
name: "xtask: support JTD discriminator schemas in generate-schemas"
status: pending
description: "Update xtask/src/generate_schemas.rs post-processing so jtd-codegen v0.4.1 discriminator output (tagged Rust enums, TS unions, Python discriminated classes) is emitted cleanly for all three runtimes. Unblocks replacing hand-authored escalate_request/escalate_response types with generated ones; required before adding further escalate ops."
github_issue: 370
dependencies:
  - "down:Polyglot IPC: escalate-on-behalf for Python/Deno processors"
adapters:
  github: builtin
---

@github:tatolab/streamlib#370

See the GitHub issue for full context.

## Priority

medium

## Parent

#319

## Depends on

#325 (introduces the first discriminator schemas and the hand-authored types this ticket replaces).

## Branch

`feat/xtask-jtd-discriminator-schemas` from `main`.
