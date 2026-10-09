---
name: discover-running-nodes
description: Enumerate the live StreamLib runtimes on this machine whose local API answers so you can pick a target before running any other control verb. Use when you need to know which running nodes exist — after launching a stream with `tatolab run` / `tatolab dev` in a worktree, when a control verb errors that zero or more-than-one node is live, or any time you must resolve a runtime name / `runtime_id` / local API socket to drive. Wraps `tatolab nodes` only.
---

# discover-running-nodes

The entry point for every control-verb workflow: list the running nodes so a later verb can target one. A StreamLib node appears here only if it hosts a control plane — the `tatolabd` that `tatolab run` and `tatolab dev` start hosts one for every stream they launch; a Rust app hosts one by calling `streamlib_api_server::serve_the_local_api_for_an_engine` with its `Runner` and holding what it returns for as long as the engine should be reachable. Hosting is what serves the local API on a user-only Unix socket, `local-api-<runtime_id>.sock` in the StreamLib runtime directory, and writes the node's entry, `<runtime_id>.json`, into the `nodes/` folder of the StreamLib runtime directory: `$XDG_RUNTIME_DIR/streamlib/nodes/` when `XDG_RUNTIME_DIR` is set and non-empty, otherwise `/tmp/streamlib-<uid>/nodes/`. `tatolab nodes` resolves that directory exactly as the engine does, so run it with the same `XDG_RUNTIME_DIR` the node was launched with. A runtime with no local API is intentionally absent, not missing. The registry is this machine's only: a node on another machine is reached by running the verb there (`ssh <machine> tatolab nodes`).

## Steps

### 1. List the nodes
```bash
tatolab nodes
```
This scans the registry, liveness-checks every entry (an MCP handshake over its local API socket — an answer, or a refusal in MCP's own words, counts as reachable — plus a host-pid check), prunes the entries that are definitively gone (unreachable AND no live pid), and prints an aligned table:

```
RUNTIME_NAME      RUNTIME_ID  LOCAL_API_SOCKET                                 PID  ALIVE?  HINT
desk-my-app-8kq3  Rabc123     /run/user/1000/streamlib/local-api-Rabc123.sock  12345  yes     tatolabd (/path/to/project)
```

- `RUNTIME_NAME` — the runtime's name, the first chunk of its tap channels: `--runtime-name` on
  `run` / `dev`, else `STREAMLIB_RUNTIME_NAME`, else derived from the host and the project
  directory, so it is stable across runs of one stream. Pass it to any control verb as
  `--node <runtime name>`; `--node` matches a name before an id, so this is the identifier
  to prefer.
- `RUNTIME_ID` — per-run, and also accepted as `--node <runtime_id>`. Use it to
  disambiguate when two live nodes were given the same name.
- `LOCAL_API_SOCKET` — the Unix socket the node serves its local API on. The verbs reach it through `--node`; you need the path only for raw HTTP: `curl --unix-socket <path> http://local/api/...`.
- `PID` — the node's `tatolabd`; `teardown-running-node` signals this to stop the node.
- `ALIVE?` — local API reachability (`yes` means the MCP handshake answered). An entry can show `no` transiently (pid still alive, control plane briefly slow) without being pruned.
- `HINT` — a human breadcrumb (e.g. the stream's project directory).

### 2. Read the outcome
- **One `yes` row** — that is your target; control verbs with no `--node` default to this sole live node, so you can often skip pinning entirely.
- **Several `yes` rows** — pick one and pin it with `--node <runtime name>` on every subsequent verb — or `--node <runtime_id>` when two rows share a name, since a shared name is refused; a verb given no `--node` with more than one live node errors and lists the candidates.
- **`No running nodes found in <directory>`** — the message names the registry folder it scanned. Start a node first (`tatolab run --dir <project>`), or check that this shell's `XDG_RUNTIME_DIR` matches the one the node was launched with, then re-run.
- **`error: the StreamLib runtime directory /tmp/streamlib-<uid> cannot be trusted: …`** — the fallback folder exists but is a symlink, owned by another uid, or open to group or other; entries planted there are never read. Remove the folder, or set `XDG_RUNTIME_DIR`.

## Notes
- No flags. `tatolab nodes` takes none.
- No token: the socket is created `0600`, so only the user who ran the node can reach it.
- To pin the chosen target and health-check it in one move, hand off to `drive-running-node`.
