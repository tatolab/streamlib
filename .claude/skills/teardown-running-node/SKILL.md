---
name: teardown-running-node
description: Stop a running StreamLib node cleanly by signaling its process, then confirm its `runtime_id` is gone from the registry so the worktree's camera/GPU claim is released. Use when done driving a node — to free a `/dev/videoN` camera for another consumer, release the GPU, or clear the registry before starting a fresh run. Wraps `tatolab nodes` (to read the pid, then to confirm removal) plus an OS signal to the process — there is no `tatolab stop` verb.
---

# teardown-running-node

Stops the node the same way you started it — by ending its process. There is deliberately no `tatolab stop` control verb (a control surface shouldn't self-destruct the deployment it operates), so teardown signals the node's process and then verifies the node deregistered. A node removes its own registry entry on clean teardown; `tatolab nodes` also prunes an entry once it is both unreachable and pid-dead.

## Steps

### 1. Read the target's pid
```bash
tatolab nodes
```
Find the row for your `RUNTIME_NAME` (or `RUNTIME_ID`) and note its `PID` — the node's `tatolabd`, which the `tatolab run` / `tatolab dev` that launched the stream started attached. `ALIVE?` `no` means only that the local API did not answer — a wedged `tatolabd` can still hold the camera and GPU. Before signaling such a row, confirm the pid is still this runtime's process and not a recycled one: `ps -o comm= -p <pid>` prints `tatolabd`, and its working directory (`readlink /proc/<pid>/cwd` on Linux, `lsof -a -d cwd -p <pid>` on macOS) is the project directory the `HINT` names. If either check fails, do not signal it.

### 2. Signal the process to stop cleanly
Send `SIGTERM` (the default) so the runtime tears down gracefully and removes its own registry entry:
```bash
kill <pid>
```
Signaling the launching `tatolab run` / `tatolab dev` is equivalent: it forwards SIGINT, SIGTERM and SIGHUP to its `tatolabd` and exits with it. A `tatolabd` that ends first ends the `tatolab run` attached to it, but `tatolab dev` stays up and starts a new `tatolabd` on the next saved edit — to stop a `dev` session for good, signal `tatolab dev` itself. If it is a node you launched in this session's foreground, `Ctrl-C` is equivalent too. Escalate to `kill -9 <pid>` only if `tatolabd` refuses to exit after a graceful signal — a hard kill skips clean teardown: `tatolab nodes` still prunes the stale entry on its next scan (unreachable AND pid-dead), but the node's `local-api-<runtime_id>.sock` (and, on Linux, its `surface-share-<runtime_id>.sock`) stays behind in the runtime directory.

### 3. Confirm the node is gone
```bash
tatolab nodes
```
The node's row should no longer appear (or the whole table reports `No running nodes found`). Its camera / GPU claim is now released for the next run or another worktree. A clean shutdown also removed its local API socket; after a hard kill the stale socket file stays until a later bind at that path clears it.

## Notes
- Get the pid from `tatolab nodes` — it is the authoritative source; do not guess.
- One camera consumer per `/dev/videoN` — tearing the node down is what frees the device for another process in this or another worktree.
- If the entry lingers after a hard kill, re-run `tatolab nodes` once; the scan prunes an entry that is unreachable and whose pid is gone.
