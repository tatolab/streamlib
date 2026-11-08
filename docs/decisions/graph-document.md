# Graph document: the graph is a stream's source of truth

Rationale for the `[graph-document]` entries in `docs/plan/ARCHITECTURE.md` §Processor model &
scheduling, §Product and §Control plane & observability, decided 2026-11-07. It supersedes "the
graph is emitted, never authored" in `one-runtime-per-machine.md`. Node kinds, the five families,
variables, Graph Input and Output nodes, islands and viewers are unchanged; the document stores
them.

## Trigger

Read this before writing a graph edit into Python source, before giving a stream a second saved
form beside its document, before addressing a node by its name, and before letting anything but
the runtime write a graph document.

## Decision

Each stream is a canonical JSON graph document in its project folder, and the runtime loads it.
The runtime is the document's one writer: every edit is a transaction with an author and an
inverse, applied whole or refused whole, and the unit of undo and history. Every node has a stable
id; its name is a label. Python defines node types, and a `@stream` function is a generator whose
output is stored as a frozen fragment. Code becomes graph; graph never becomes the author's code.

## Rejected alternatives

- **Graph edits written back into `stream.py` with a concrete-syntax-tree rewrite.** Text becomes
  the meeting point for the studio, the code pane, agents and the runtime; every graph edit becomes
  a text edit and every code edit a re-import and diff.
- **`stream.py` as source with live edits lost on the next start.** An agent's work and the
  studio's layout vanish on a restart, so a dynamic graph is not worth editing.
- **A side file of studio data beside `stream.py`.** It drifts from the code it annotates.
- **A CRDT for the document now.** Two valid edits can merge into an invalid graph, and a hub must
  validate edits anyway; it is worth adding only for editing with no hub.

## Why

- **It is how creative tools work.** TouchDesigner, Unreal, Houdini, DaVinci Fusion and Blender
  keep a tool-owned model edited by operations, and code defines node types, not the scene. Where
  the saved form is code (Houdini's `opadd` scripts, Fusion's Lua tables, Nuke's `.nk`), it is
  generated and flat, so the tool round-trips it. An author's `stream.py` is neither.
- **Expressive code stops costing editability.** A generator may loop, branch, read a file or call
  an API; all of it finishes before a frame flows, so its output is always a static snapshot.
- **Stable ids end edit fights.** A rename and a config change from two places never touch the
  same thing, and links, positions and history follow the id.
- **Transactions are the seam for later work.** Undo, a change feed, agents batching edits, and a
  hub that orders and merges operations all build on the same unit.

## Consequences

- A project has no `stream.py`; a stream's identity comes from its document's path, so every
  existing stream gets a new id.
- The runtime gains a saver and a transaction log, and the local API's tools change from names to
  ids, which every client of the local API follows.
- Tato Studio reads saved positions instead of laying graphs out, and its code pane edits node code
  and generators.
- A package ships graph documents as package data, as a TouchDesigner `.tox` ships a network.
