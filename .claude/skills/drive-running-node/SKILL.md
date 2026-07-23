---
name: drive-running-node
description: Resolve exactly one running StreamLib node and health-check it with a live `graph` round-trip, then pin its `--url` / `--node` so every following control verb targets the same node. Use at the start of a session against a running app — after `discover-running-nodes`, or whenever you are about to inspect, tap, submit, replace, connect, or capture and need a confirmed target locked in. Wraps `streamlib nodes` then `streamlib graph`.
---

# drive-running-node

Turns "some node is running" into "this specific node answers, and here is how I address it." A `graph` call is the cheapest full control-plane round-trip, so it doubles as the health check: if `graph` returns the live topology, the node is drivable.

## Steps

### 1. Find the candidates
```bash
streamlib nodes
```
Read the `RUNTIME_ID` and `CONTROL_URL` of the row you want (see `discover-running-nodes` for the columns). Copy one identifier — either the `runtime_id` (for `--node`) or the `control_url` (for `--url`).

### 2. Health-check + pin the target
Pick ONE addressing form and reuse it verbatim on every later verb:

By registry `runtime_id`:
```bash
streamlib graph --node <runtime_id>
```
By explicit control URL:
```bash
streamlib graph --url <control_url>
```
If exactly one node is live, both flags may be omitted and the resolver uses that sole node:
```bash
streamlib graph
```

A JSON graph dump (processors, links, states, metrics) means the node is healthy and the address is good. A non-zero exit means it is not drivable:
- `no live StreamLib nodes found` — nothing is running; start a node.
- `N live nodes found; disambiguate with --node ... or --url ...` — more than one is live and you passed neither flag; re-run with a specific `--node`/`--url`.
- `no registered node with runtime_id <id>` — the `--node` value is wrong or the node exited; re-run `streamlib nodes`.
- A transport/HTTP error — the endpoint is unreachable or auth failed (set `STREAMLIB_MCP_TOKEN`).

### 3. Pin it for the rest of the session
Record the chosen `--node <runtime_id>` (preferred — stable across a port change) or `--url <control_url>`, and pass the same flag to every subsequent verb (`inspect-live-graph`, `tap-live-channel`, `author-and-submit-processor`, `hot-swap-live-processor`, `capture-node-evidence`, `teardown-running-node`).

## Notes
- Prefer `--node <runtime_id>` over `--url` when several nodes may be live — it is unambiguous and survives the `ApiServer`'s port auto-increment.
- `STREAMLIB_MCP_TOKEN`, when set, rides as the bearer token on the round-trip; export it once for the whole session.
