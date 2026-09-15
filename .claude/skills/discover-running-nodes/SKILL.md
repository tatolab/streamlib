---
name: discover-running-nodes
description: Enumerate the live StreamLib runtimes reachable over their control plane so you can pick a target before running any other control verb. Use when you need to know which running nodes exist — after launching an app with `streamlib run` / `streamlib dev` in a worktree, when a control verb errors that zero or more-than-one node is live, or any time you must resolve a `runtime_id` / `control_url` to drive. Wraps `streamlib nodes` only.
---

# discover-running-nodes

The entry point for every control-verb workflow: list the running nodes so a later verb can target one. A StreamLib node appears here only if it hosts a control plane — `streamlib run` and `streamlib dev` host one for every app they launch; a plain `python app.py` hosts one only by calling `rt.host_control_plane()` before `run()`; a Rust app hosts one by adding `streamlib_api_server::ApiServerProcessor`. Hosting is what binds the `POST /mcp` control endpoint and writes the node's entry, `<runtime_id>.json`, into the `nodes/` folder of the StreamLib runtime directory: `$XDG_RUNTIME_DIR/streamlib/nodes/` when `XDG_RUNTIME_DIR` is set and non-empty, otherwise `/tmp/streamlib-<uid>/nodes/`. `streamlib nodes` resolves that directory exactly as the engine does, so run it with the same `XDG_RUNTIME_DIR` the node was launched with. A runtime with no control endpoint is intentionally absent, not missing.

## Steps

### 1. List the nodes
```bash
streamlib nodes
```
This scans the registry, liveness-checks every entry (a `graph` call to its `POST /mcp` — any HTTP answer, a `401` included, counts as reachable — plus a host-pid check), prunes the entries that are definitively gone (unreachable AND no live pid), and prints an aligned table:

```
RUNTIME_ID  CONTROL_URL            PID    ALIVE?  HINT
Rabc123     http://127.0.0.1:9000  12345  yes     python (/path/to/app)
```

- `RUNTIME_ID` — pass to any control verb as `--node <runtime_id>`.
- `CONTROL_URL` — pass to any control verb as `--url <control_url>` (its `POST /mcp` base).
- `PID` — the host process; `teardown-running-node` signals this to stop the node.
- `ALIVE?` — control-plane reachability (`yes` means a `graph` round-trip answered). An entry can show `no` transiently (pid still alive, control plane briefly slow) without being pruned.
- `HINT` — a human breadcrumb (e.g. the app's cwd).

### 2. Read the outcome
- **One `yes` row** — that is your target; control verbs with neither `--url` nor `--node` default to this sole live node, so you can often skip pinning entirely.
- **Several `yes` rows** — pick one and pin it with `--node <runtime_id>` (or `--url`) on every subsequent verb; a verb given neither flag with more than one live node errors and lists the candidates.
- **`No running nodes found in <directory>`** — the message names the registry folder it scanned. Start a node first (`streamlib run --dir <app>`), or check that this shell's `XDG_RUNTIME_DIR` matches the one the node was launched with, then re-run.
- **`error: the StreamLib runtime directory /tmp/streamlib-<uid> cannot be trusted: …`** — the fallback folder exists but is a symlink, owned by another uid, or open to group or other; entries planted there are never read. Remove the folder, or set `XDG_RUNTIME_DIR`.

## Notes
- No flags. `streamlib nodes` takes none.
- When the control plane requires auth, export `STREAMLIB_MCP_TOKEN` before running — it rides the liveness probe (and every control verb) as an `authorization: Bearer` header.
- To pin the chosen target and health-check it in one move, hand off to `drive-running-node`.
