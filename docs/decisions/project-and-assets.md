# Projects and assets: a project is a versioned repository the runtime writes

Rationale for the `[project-and-assets]` entries in `docs/plan/ARCHITECTURE.md` §Processor model &
scheduling, §Product and §Control plane & observability, decided 2026-11-07 and 2026-11-08. It
supersedes "the graph is emitted, never authored" in `one-runtime-per-machine.md`. Node kinds, the
five families, variables, Graph Input and Output nodes, islands and viewers are unchanged; a
stream's graph document stores them.

## Trigger

Read this before writing a graph edit into Python source, before referencing a file by path in a
node's config, before addressing a node or an asset by its name, before relying on the person's own
Git, before copying another project's stream in by default, and before letting anything but the
runtime write a project.

## Decision

A project is a versioned repository of assets, one file per asset; a stream's graph document is
one kind of asset beside media, images, USD, FMUs and the rest. The runtime generates a UUID for
every asset and node instance when it is created, and every reference names a UUID. The runtime is
the project's one writer: every change is a transaction with an author and an inverse, the unit of
undo and history. Each project has its own Git history, written by a Git engine the runtime
carries, in a form Cloudflare Artifacts takes as a remote. Large binary content lives in a
content-addressed store beside history, with only a pointer in it. Another project's asset is a
linked dependency pinned to a revision, updated by choice and copied only by a fork. Python defines
node types; a `@stream` function is a generator whose output is stored as a frozen fragment.

## Rejected alternatives

- **Graph edits written back into `stream.py` with a concrete-syntax-tree rewrite.** Text becomes
  the meeting point for the studio, the code pane, agents and the runtime, and expressive code
  refuses edits.
- **One file for the whole project, as a TouchDesigner `.toe`.** Every edit by anyone touches the
  same file, so merges and partial fetches stop working for a team.
- **Paths as references.** A move or a rename breaks every node that named the file; Unreal's
  asset registry and Unity's `.meta` GUIDs exist for this.
- **The person's own Git.** Many creatives have none installed, and each project needs its own
  history, apart from any repository the folder sits in.
- **Media inside history.** Artifacts caps a repository at 1 GB and a file at 32 MiB and has no Git
  LFS; history holds a pointer, as LFS does.
- **Copying a dependency's stream by default.** It severs the link to the team that maintains it,
  so updates stop arriving.
- **A CRDT now.** Two valid edits can merge into an invalid graph, and a hub must validate edits
  anyway.

## Why

- **It is how creative tools work.** Unreal, Houdini, TouchDesigner, DaVinci and Blender keep a
  tool-owned model of a project and its assets, edited by operations, and code defines node types,
  not the scene. Houdini's digital assets and Unreal's plugins are the linked-dependency shape.
- **UUIDs end edit fights.** A rename and a config change from two places never touch the same
  thing, and links, positions, state and history follow the UUID.
- **Transactions and Git-shaped history are the seam for multiplayer.** A hub that orders
  transactions without a runtime, per-person branches, merges and releases all build on them.

## Consequences

- A project has no `stream.py`; every existing stream gets a new id, derived from UUIDs.
- The runtime gains a saver, a Git engine, a content store and asset verbs, and the local API's
  tools change from names to UUIDs, which every client follows.
- Tato Studio reads saved positions, shows assets and dependencies, and its code pane edits node
  code and generators.
- Syncing content to the cloud needs storage beside the Artifacts remote, which is later work.
