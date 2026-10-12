---
name: list-running-streams
description: List the streams the machine's one StreamLib runtime holds — attached, kept, stopped or failed — so you can pick one by name before running any other control verb. Use when you need to know what is running — after loading a stream with `tatolab run` / `tatolab dev`, when a verb refuses a stream name, or any time you must find the stream name every other verb takes. Wraps `tatolab streams`.
---

# list-running-streams

The entry point for every control-verb workflow. A machine runs one runtime, `tatolabd`, and every stream on it loads into that one process; `tatolab streams` asks it which streams it holds. Every verb reaches the runtime through one socket at a fixed path, `local-api.sock` in this user's runtime directory (`$XDG_RUNTIME_DIR/streamlib/`, else `/tmp/streamlib-<uid>/`), so no verb takes a target flag. A runtime on another machine is reached by running the verb there (`ssh <machine> tatolab streams`).

## Steps

### 1. List the streams
```bash
tatolab streams
```
An aligned table, then a line under it per failed stream saying why it failed:

```
NAME     STATE     NODES  PROJECT
camera   attached  4      /home/me/camera
lab      kept      6      /home/me/lab
old      stopped   -      /home/me/old
```

- `NAME` — the stream's name, which `stop`, `start`, `rm`, `expose`, `graph --stream`, `logs --stream` and the first part of a `tap` / `exchange` channel take. It is the function's name, or `--name` on `run`, cast to lowercase URL-safe.
- `STATE` — `attached` (lives as long as the `tatolab run`, `tatolab dev` or MCP connection that loaded it), `kept` (loaded with `run -d`; comes back on every start of the runtime), `stopped` (kept, unloaded by `tatolab stop`), or `failed` (kept, implicated in the runtime's last two crashes in a row or unable to re-load at its start).
- `NODES` — the loaded graph's node count; `-` when the stream is not loaded.
- `PROJECT` — the project directory the runtime compiled it in.

### 2. Read the outcome
- **A row in `attached` or `kept`** — that stream is loaded; hand its name to `drive-running-stream`.
- **`No streams in this runtime.`** — the runtime answers and holds nothing. Load one with `tatolab run --dir <project>` (attached) or `tatolab run -d --dir <project>` (kept).
- **`no runtime is running on this machine: nothing answers at <socket>`** — no runtime is up for this user. The machine's runtime is a service (`systemctl --user start tatolabd`) or `tatolabd` in a terminal; a session that needs a runtime of its own starts the test-only build instead (`drive-running-stream`). When another user holds the machine, the refusal names them.
- **`stopped` / `failed`** — not loaded. `tatolab start <name>` loads it again (retrying a failed one); `tatolab rm <name>` forgets it.

## Notes
- No flags. `tatolab streams` takes none.
- No token: the socket is created `0600`, so only the user the runtime serves can reach it.
- `tatolab graph` with no `--stream` returns every loaded stream's graph under the machine's name — use it when you need topology, not just names (`inspect-live-graph`).
