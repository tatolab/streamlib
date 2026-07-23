---
name: author-and-submit-processor
description: Write a brand-new processor from source and submit it into a running StreamLib node as a fresh instance, optionally wiring its ports to existing processors in one transactional step, then confirm it landed and is producing. Use when adding a new stage to a live pipeline during development — a new filter, sink, or generator — without restarting the app. Wraps `streamlib submit` then `graph` + `tap` to confirm.
---

# author-and-submit-processor

Adds a new processor to a running graph from source text. `submit` is transactional: it registers the source, instantiates the first discovered processor type as a fresh `@session/<name>` instance, and — if you pass `--connect` — wires it; a failed wiring rolls the entire submit back. Live submit is Python / TypeScript / Deno only; `rust` is rejected because it is a full cargo build, not a live graph mutation.

## Steps

### 1. Write the processor source to a file
Author the processor in a real file (processors are real modules) — e.g. `/tmp/my_filter.py` (Python), `.ts` (TypeScript), or a Deno module. Note the PascalCase processor type name it defines.

### 2. Inspect the live graph for wiring targets
Run `inspect-live-graph` and note the processor ids and port names you will connect the new processor to. A connect spec is `local_port:role:peer_processor:peer_port`, where `role` is `output` (your `local_port` is an output feeding the peer's input) or `input` (your `local_port` is an input fed by the peer's output).

### 3. Submit the processor
```bash
streamlib submit --node <runtime_id> \
  --language python \
  --source @/tmp/my_filter.py \
  --requested-name my-filter \
  --processor-type-name MyFilter \
  --config '{"gain": 2}' \
  --connect frames_in:input:camera:frames \
  --connect frames_out:output:display:frames
```
- `--language` — `python` | `typescript` | `deno` (`rust` is rejected).
- `--source` — `@<file>` or a plain path reads the file; `-` or omitting `--source` reads source from stdin.
- `--requested-name` — the `@session/<name>` segment to mint under (derived from the type name if omitted).
- `--processor-type-name` — the PascalCase type the source defines (derived from the requested name if omitted).
- `--config` — JSON applied at instantiation (default `{}`).
- `--connect` — repeatable; each is one `local_port:role:peer_processor:peer_port` wiring. Omit to submit unwired and wire later with `streamlib connect`.

Address the node with `--url <control_url>` instead of `--node`, or omit both when exactly one node is live.

### 4. Wire after the fact (only if you skipped `--connect`)
```bash
streamlib connect --node <runtime_id> \
  --from-processor camera --from-port frames \
  --to-processor <new_processor_id> --to-port frames_in
```
Get `<new_processor_id>` from the graph (step 5).

### 5. Confirm it landed and is producing
```bash
streamlib graph --node <runtime_id>
streamlib tap --node <runtime_id> <new_processor_id>/<output_port> --count 10
```
The graph should list the new processor with its links; the tap should show bags flowing on its output channel (see `tap-live-channel`). If `submit` exited non-zero, nothing was added — the transaction rolled back; read the error text, fix the source/wiring, and resubmit.

## Notes
- This authors a NEW processor. To swap an EXISTING processor's source, use `hot-swap-live-processor` (`replace`).
- `submit` does not compile Rust and does not accept it — keep new processors in Python / TypeScript / Deno for the live path.
- `STREAMLIB_MCP_TOKEN`, when set, authorizes the call as a bearer token.
