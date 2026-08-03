# One control-plane surface

Rationale for the `[control-plane-one-surface]` entries in `docs/plan/ARCHITECTURE.md`
§Control plane & observability, decided 2026-07-29.

## Trigger

Read this before adding any second way to inspect or mutate a running node (a new
socket, a bespoke CLI channel, a side API), before moving or duplicating the api-server,
or before changing node discovery or the tap's delivery guarantees.

## Decision

There is exactly one control plane: the api-server's HTTP + WebSocket + MCP surface,
hosted in-process by any runtime that enables it. The MCP tool set is the canonical
control vocabulary — the CLI is a pure JSON-RPC client of the same tools agents call, so
humans, scripts, and agents drive identical verbs; REST/WS routes expose the same
operations for programmatic clients. The api-server is engine-side infrastructure and
relocates into the `runtime/` tree: it is a host (statically linked, never dlopen'd) and
cannot follow the packages tree out of the repo.

Node discovery is a per-user on-disk registry — one JSON file per live node in the OS's
standard per-user runtime directory — written only by control-plane-hosting runtimes and
pruned only when both liveness signals (a control round-trip and a process check) fail.

Observability: the JSONL log schema is a durable contract; tap forwards bags verbatim
(no transcode) and trades completeness for guaranteed non-interference with the
pipeline; graph and health inspection ride the same control plane.

Auth and remote-access posture remain OPEN — nothing here decides a security model.

## Rejected alternatives

- **A second control path** (bespoke CLI socket, side-channel debug API) — two
  vocabularies drift; agents and humans stop seeing the same system.
- **api-server as a distributable package** — it drives the registry, pubsub, and the
  graph API; ~~a host cannot cross the plugin ABI, and its current home made it a
  standing exception to "packages are downstream consumers."~~ — Rationale updated
  2026-08-02 by `importable-python-library.md` (the plugin ABI and packages doctrine
  are deleted): the api-server is statically-linked engine infrastructure hosted by
  the wheel and the `streamlib` crate; the relocation into `runtime/` stands and is a
  sequencing prerequisite of the rip-out. Its mutation verbs (submit / replace /
  connect / remove) and their MCP tools are removed — the vocabulary is
  observation-shaped.
- **A discovery daemon or well-known port** — files in the per-user runtime directory
  need no daemon, survive nothing they shouldn't, and prune safely on double-dead
  evidence.
- **A lossless tap** — a parked tap consumer on a lossless channel back-pressures the
  source processor; verbatim-but-droppable is the deliberate trade.

## Consequences

- New control verbs are added once, as MCP tools, and every client gets them.
- Relocating the api-server is engine-tree work to schedule; until it lands the plan
  supersedes the old "exception" framing.
- Remote access and auth need their own decision before any networked-control work.
