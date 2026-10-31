# failed-load-logs

Builds the no-`@stream` clause of §Product's "A load is never refused" entry (ADR
`docs/decisions/failed-load-logs.md`, aligned in #2977). The entry's log clauses are built by
`queued-stream-actions.md`, whose one log per project serves a failed stream's records.

Today a `stream.py` that imports and defines no `@stream` is refused.

**After this change:** a `stream.py` with no `@stream` is a failed project, not a refusal.

**Scale gate — this skill; the ADR exists.**
- **Contracts changed:** the compile document (a fifth answer).
- **Not touched:** the RHI, the iceoryx2 wire, the processor model, `@stream` and the rest of
  the authoring API, the stream id, the stream record, and the JSONL record shape.

**Precondition.** §Product is DECIDED, and its entry carries `[failed-load-logs]`.

## Current state (recon, 2026-10-29, `main` `39031084d`)

- **No `@stream`.** `select_stream_function` raises `_no_stream_defined_refusal`
  (`sdk/tatolab-stream/tatolab/stream/_project_stream_entry_execution.py:150,182`), so the
  compile entry exits 1 and `run_stream` is refused.
- **A failed stream's reason.** `Runner::run_stream` records an entry that did not import
  through `fail_every_record_of_a_project_whose_entry_did_not_import`
  (`runtime/streamlib-engine/src/core/runtime/stream_actions_of_this_runtime.rs`).

## ADDED

- **A compile answer for no `@stream`.** With no target, an entry that imports and defines no
  `@stream` writes `{project_directory, no_stream_defined_because}` and exits 0 — the reason the
  refusal gives today, its sample included. A target naming a function stays a
  `not_defined_because`. The runtime answers it as an entry that did not import, the reason the
  missing `@stream`: every record of the project failed, or the project's `stream.py` record.

## MODIFIED

- **The compile entry** (`_project_stream_compile_entry.py`) documents the fifth answer in its
  module docstring; `_project_stream_entry_execution.py` raises a `StreamEntryDefinesNoStreamError`
  that the entry turns into the document.

## REMOVED

- REMOVED: _no_stream_defined_refusal
  The refusal of an entry that defines no `@stream`.

## Test plan

- **Compile op:** `no_stream_defined_because` parses, and is answered as an entry that did not
  import.
- **Stream suite** (`test_project_stream_compile_entry.py`): an entry with no `@stream` and no
  target writes `no_stream_defined_because` with the sample; with a target it stays
  `not_defined_because`.
- **Engine** (`ProjectWithAStubCompile`): an entry with no `@stream` records the project's
  `stream.py` record failed, or every recorded stream of the project, naming the missing
  `@stream`.
