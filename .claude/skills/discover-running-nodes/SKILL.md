---
name: discover-running-nodes
description: Enumerate the live StreamLib runtimes reachable over their control plane so you can pick a target before running any other control verb. Use when you need to know which running nodes exist — after `cargo run`-ing an example app in a worktree, when a control verb errors that zero or more-than-one node is live, or any time you must resolve a `runtime_id` / `control_url` to drive. Wraps `streamlib nodes` only.
---

# discover-running-nodes

The entry point for every control-verb workflow: list the running nodes so a later verb can target one. A StreamLib node appears here only if its graph hosts an `@tatolab/api-server/ApiServer` processor — that processor is what binds a `POST /mcp` control endpoint and registers the node under `$XDG_RUNTIME_DIR/streamlib/nodes/<runtime_id>.json`. A runtime with no control endpoint is intentionally absent, not missing.

## Steps

### 1. List the nodes
```bash
streamlib nodes
```
This scans the registry, liveness-checks every entry (a `graph` control-plane round-trip plus, on unix, a host-pid check), prunes the entries that are definitively gone (unreachable AND no live pid), and prints an aligned table:

```
RUNTIME_ID  CONTROL_URL            PID    ALIVE?  HINT
Rabc123     http://127.0.0.1:8080  12345  yes     streamlib (/path/to/app)
```

- `RUNTIME_ID` — pass to any control verb as `--node <runtime_id>`.
- `CONTROL_URL` — pass to any control verb as `--url <control_url>` (its `POST /mcp` base).
- `PID` — the host process; `teardown-running-node` signals this to stop the node.
- `ALIVE?` — control-plane reachability (`yes` means a `graph` round-trip answered). An entry can show `no` transiently (pid still alive, control plane briefly slow) without being pruned.
- `HINT` — a human breadcrumb (e.g. the app's cwd).

### 2. Read the outcome
- **One `yes` row** — that is your target; control verbs with neither `--url` nor `--node` default to this sole live node, so you can often skip pinning entirely.
- **Several `yes` rows** — pick one and pin it with `--node <runtime_id>` (or `--url`) on every subsequent verb; a verb given neither flag with more than one live node errors and lists the candidates.
- **`No running nodes found`** — start a node first (`cargo run` an example app whose graph hosts an `ApiServer` processor), then re-run.

## Notes
- No flags. `streamlib nodes` takes none.
- When the control plane requires auth, export `STREAMLIB_MCP_TOKEN` before running — it rides the liveness probe (and every control verb) as an `authorization: Bearer` header.
- To pin the chosen target and health-check it in one move, hand off to `drive-running-node`.
