# uncommitted-live-edits

> **Proposed, awaiting the owner (session, 2026-11-11).** The runtime keeps, per held project, the
> live graph edits not yet in its document, and serves them as a list with a change notification,
> a commit and a drop. They stay in memory, never in history, until one of those, a close or a
> runtime restart. Tato Studio's History panel heads its change feed with them ("2 live changes,
> not saved", **Commit**, **Drop**) (tatolab/streamlib#3486). The `[NEEDS DECISION]` below is the
> owner's; the rest is the session's recommendation.

## Current state (recon, `main` `93c2ba6`)

- **A live edit is applied and forgotten.** `mode: "live"` applies a transaction to the held
  graph and opens no repository (`runtime/streamlib-api-server/src/mcp_stream_graph_tools.rs:61`,
  `:1483-1484`); the engine reconciles the running graph
  (`runtime/streamlib-engine/src/core/runtime/stream_graph_operation_applied_live.rs:22`, `:173`).
  Nothing records which operations were live, who sent them or when.
- **The plan says the function wins on the next start.** §Processor model, the stream-load entry:
  "Live edits are never recorded, so the function wins on the next start." That held while Stop
  unloaded the stream.
- **Playback-controls (#3542, #3543) broke the reset.** Stop now keeps the held graph and Play
  instantiates from it (`docs/plan/changes/playback-controls.md`, "Play runs what was built"), and
  the soft `reload_project` re-reads only documents that changed. A live edit therefore now
  survives Stop, Play and a soft reload, and the held graph drifts from the document with nothing
  that names the difference. Only `close_project` / `open_project` or a runtime restart resets it.
- **The change feed is the one place a client reads a project's changes.**
  `tatolab://…/transactions` serves applied, refused and media-facts entries with one
  `notifications/resources/updated` (`runtime/streamlib-api-server/src/mcp_project_change_feed.rs:192`,
  `:213`).
- **A Graph Input write is not a graph edit.** `write_graph_input` (live-graph-inputs) publishes a
  value on a running node; it never touches the held graph, so it has nothing to commit or drop.

## [NEEDS DECISION] What happens to a live edit nobody commits

1. **Kept until committed or dropped (recommended).** Pending live edits stay on the held graph
   through Stop, Play, Pause and a soft reload, listed until the person commits or drops them, and
   go only on `close_project` or a runtime restart. Stop never loses a tweak silently, and the bar
   in History stays the one place that says the graph differs from what is saved.
2. **Dropped at Stop, as Unreal's play-in-editor does.** Stop drops every pending live edit,
   reapplying the document, so a playing session's tweaks end with it unless committed first.
   Simple to explain, but a person who stops to look at a result loses it.
3. **Never recorded, and Stop reapplies the document.** No list and no verbs; Studio's bar is cut,
   and a slider commits one transaction at gesture end as the edit-mode docs already say. This
   keeps the plan's sentence and only fixes the drift playback-controls introduced.

The rest of this proposal is written for option 1; option 2 changes only the Stop bullet, and
option 3 replaces it with one MODIFIED line on Stop.

## ADDED

- **A held project's uncommitted live edits.** The runtime keeps, per held project, each live
  transaction applied since the project was opened or last committed or dropped: its author, its
  operations, its stream and `applied_at`. They are coalesced as they arrive so the list stays the
  difference from the document, not the gesture's history: a later live `set_node_config` or
  `set_pin_value` of the same field replaces the earlier one, and a node added then removed live
  leaves nothing. In memory only, never in the document or the history.
- **A committed edit supersedes a pending one.** A `commit`-mode transaction (or an undo or redo)
  that sets a field a pending live edit set removes that field from the pending edits, so a
  slider's live drag followed by its gesture-end commit leaves nothing pending.
- **The list on the change feed.** The project's `transactions` resource gains
  `uncommitted_live_edits`: one entry per stream with pending edits, each carrying its stream id,
  the coalesced `operations`, the authors' kinds and names, and the first and last `applied_at`.
  It is empty when the held graph equals the document. The resource's existing
  `notifications/resources/updated` fires whenever it changes; no second resource.
- **`commit_live_edits {project, author?, idempotency_key?}`.** Records every pending live edit of
  the project as one transaction by the committing author, its operations the coalesced ones and
  its inverse the document's prior values, so `undo` reverses it whole. It writes the documents and
  touches nothing running. Refused, in the runtime's words, when nothing is pending or the
  project is not held. Answers the transaction as `transact` does.
- **`drop_live_edits {project}`.** Reapplies each affected stream's document to the held graph
  through the existing live reconcile, so a running node takes the document's config at once and a
  live-added node goes. Records nothing. Refused when nothing is pending.
- **Both verbs name the project, not a stream.** The bar in History is per project; a per-stream
  commit or drop waits for a client that needs it.

## MODIFIED

- §Processor model, the stream-load entry: "Live edits are never recorded, so the function wins on
  the next start" becomes "a live edit is never in the document or the history; it is held, listed
  on the project's change feed until committed or dropped, and gone when the project closes or the
  runtime restarts".
- §Processor model, the project-and-assets entry ("written unless the caller marks a graph edit
  live"): a live edit is held as uncommitted until `commit_live_edits` records it or
  `drop_live_edits` reverts it.
- `GRAPH_EDIT_MODE_ARGUMENT_DESCRIPTION` (`mcp_stream_graph_tools.rs:61`): `live` reads "applied to
  the held graph, listed as uncommitted on the project's change feed until committed or dropped,
  never in history".
- §Control plane & observability: `commit_live_edits` and `drop_live_edits` join the graph tools,
  as operations of the operation registry once #3547 lands.
- Glossary: **Live edit** — a graph edit applied to the held graph and not yet in its document.

## REMOVED

- REMOVED: Live edits are never recorded, so
- REMOVED: gone on the next load, never in history

## Not in this change

- **An ordinal per transaction** ("edit 131" on Studio's canvas). The feed carries no position in
  the project's history. A count of commits is derivable from the history and small; it is noted
  here and not built unless Studio asks for it.
- **Saving a Graph Input's value as its `default`.** Perform's control values are data, never graph
  edits, so they are not pending edits. Keeping one as the Input's `default` is an ordinary
  `set_node_config` commit a client sends.
- **Surviving a crash.** Pending edits are memory, so a restart loses them, as "runtime state is
  never saved" holds. A recovery file under `.tatolab/` is separate work if it is ever wanted.

## Tickets (after approval)

One tracer bullet: the pending record and coalescing in the graph transaction path, the feed
field, both verbs, the plan and description words. Engine tests over a real engine: a live
`set_node_config` then commit records one transaction, empties the list and undo restores the
old value; a live edit then drop restores the document's value on the running node; a live drag
then a commit-mode edit of the same field leaves nothing pending; a live edit survives Stop and
Play.

## Consumers

Tato Studio's History panel ("live changes, not saved" bar) reads the feed field and calls the two
verbs. That is tatolab-app backlog, filed once this ships, and never blocks it.
