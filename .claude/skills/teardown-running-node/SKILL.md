---
name: teardown-running-node
description: Stop a running StreamLib node cleanly by signaling its host process, then confirm its `runtime_id` is gone from the registry so the worktree's camera/GPU/port claim is released. Use when done driving a node — to free a `/dev/videoN` camera for another consumer, release the GPU, or clear a control port before starting a fresh run. Wraps `streamlib nodes` (to read the pid, then to confirm removal) plus an OS signal to the process — there is no `streamlib stop` verb.
---

# teardown-running-node

Stops the node the same way you started it — by ending its process. There is deliberately no `streamlib stop` control verb (a control surface shouldn't self-destruct the deployment it operates), so teardown signals the host pid and then verifies the node deregistered. A node removes its own registry entry on clean teardown; `streamlib nodes` also prunes an entry once it is both unreachable and pid-dead.

## Steps

### 1. Read the target's pid
```bash
streamlib nodes
```
Find the row for your `RUNTIME_ID` and note its `PID` (the host process — typically the `cargo run` of the example app). Confirm `ALIVE?` is `yes` before signaling.

### 2. Signal the process to stop cleanly
Send `SIGTERM` (the default) so the runtime tears down gracefully and removes its own registry entry:
```bash
kill <pid>
```
If it is a `cargo run` you launched in this session's foreground, `Ctrl-C` is equivalent. Escalate to `kill -9 <pid>` only if the process refuses to exit after a graceful signal — a hard kill skips clean teardown, but `streamlib nodes` will still prune the stale entry on its next scan (unreachable AND pid-dead).

### 3. Confirm the node is gone
```bash
streamlib nodes
```
The `runtime_id` should no longer appear (or the whole table reports `No running nodes found`). Its camera / GPU / control-port claim is now released for the next run or another worktree.

## Notes
- Get the pid from `streamlib nodes` — it is the authoritative source; do not guess.
- One camera consumer per `/dev/videoN` — tearing the node down is what frees the device for another process in this or another worktree.
- If the entry lingers after a hard kill, re-run `streamlib nodes` once; the scan prunes an entry that is unreachable and whose pid is gone.
