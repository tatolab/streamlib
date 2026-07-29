---
name: research
description: Answer a question from primary sources and file a memo. Use when a decision needs facts the repo cannot provide — driver behavior, spec details, protocol semantics, ecosystem comparisons. Produces no tickets and no code.
---

# Research

1. Spawn a background agent against **primary sources** — official docs, source code,
   specs, first-party APIs — never a secondary write-up of them. Follow every claim back
   to the source that owns it.
2. The memo lands at `docs/research/<slug>.md`: the question, the answer, the evidence
   with URLs, and what remains unknown.
3. Report the answer's summary in chat.

This skill never files tickets, never edits code, never edits the plan. If the findings
demand a decision, say so and point at `/align` or `/propose-change`.
