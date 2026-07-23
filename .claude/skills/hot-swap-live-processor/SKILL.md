---
name: hot-swap-live-processor
description: Swap an existing processor's source registration in a running StreamLib node without restarting the app, rewire ports if the port shape changed, and confirm the new behavior on live data. Use when you have edited a processor's source and want the change reflected in a running pipeline during development. Wraps `streamlib replace` (type-level), with `submit` / `connect` / `remove` as needed, then `graph` + `tap` to confirm.
---

# hot-swap-live-processor

Replaces the SOURCE REGISTRATION for a live `@session/<name>` module. This is type-level: `replace` swaps what the type resolves to, but already-running instances are NOT swapped in place — they keep running the prior source until removed and re-instantiated. So the flow depends on whether you need running instances to pick up the change immediately, and whether the port shape changed.

## Steps

### 1. Inspect the live graph first
Run `inspect-live-graph`. Note the target `@session/<name>` module, the running instance's processor id, and its current input/output port names. Decide whether your edit changed the port shape (added/removed/renamed a port).

### 2. Replace the source registration
```bash
streamlib replace --node <runtime_id> \
  --target-session-module '@session/widget@*' \
  --language python \
  --source @/tmp/widget_v2.py \
  --requested-name widget \
  --processor-type-name Widget
```
- `--target-session-module` — the `@session/<name>@<range>` module to replace, e.g. `@session/widget@*`.
- `--language` — `python` | `typescript` | `deno` (`rust` is rejected, same as `submit`).
- `--source` — `@<file>` / plain path / `-` / stdin, same rules as `submit`.
- `--requested-name`, `--processor-type-name` — the segment and PascalCase type of the replacement.

`replace` is transactional: a failed replacement restores the prior registration. It updates the registration only — see step 4 to make a running instance actually run the new source.

### 3. Rewire if the port shape changed
If the edit added, removed, or renamed ports, fix the wiring against the new shape:
```bash
streamlib connect --node <runtime_id> \
  --from-processor <upstream> --from-port <out> \
  --to-processor <target_processor_id> --to-port <new_in>
```
Remove a link that no longer fits by removing and re-instantiating the affected instance (there is no standalone disconnect verb — `remove` drops the instance and its links).

### 4. Re-instantiate to run the new source in a live instance
Because `replace` does not swap running instances in place, a running instance keeps the old source. To run the new source now, remove the old instance and instantiate a fresh one from the updated registration:
```bash
streamlib remove --node <runtime_id> --processor-id <old_instance_id>
streamlib submit --node <runtime_id> --language python \
  --source @/tmp/widget_v2.py --requested-name widget --processor-type-name Widget \
  --connect <local_port>:<role>:<peer>:<peer_port>
```
(Skip this step if you only needed the registration updated for future instantiations.)

### 5. Confirm the new behavior
```bash
streamlib graph --node <runtime_id>
streamlib tap --node <runtime_id> <target_processor_id>/<output_port> --count 10
streamlib logs --node <runtime_id> --count 50
```
The graph reflects the new topology; the tap shows the changed output on the next bags; `logs` surfaces any errors from the swap (see `tap-live-channel` for reading a tap sample).

## Notes
- Type-level is the intended semantic — do not expect `replace` alone to mutate an already-running instance; step 4 is how a live instance takes the new source.
- Every verb here targets the node with `--url` / `--node` (or neither, when one node is live); `STREAMLIB_MCP_TOKEN` authorizes when set.
- To ADD a new processor rather than swap one, use `author-and-submit-processor`.
