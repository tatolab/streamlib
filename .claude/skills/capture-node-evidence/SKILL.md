---
name: capture-node-evidence
description: Freeze a verifiable record of a running StreamLib node — a live graph snapshot, N tapped bags from one or more channels on disk, and a bounded log excerpt — into a directory for a PR, an issue, or a before/after comparison. Use when you need durable proof that a pipeline is running and producing, or to capture the "before" and "after" around a stream-code change. Wraps `tatolab graph`, `tatolab tap`, and `tatolab logs` with shell redirection.
---

# capture-node-evidence

Turns a live node into on-disk artifacts nothing else touches. All three verbs print to stdout; none has an `--output` flag, so redirect stdout to files in a directory you control. Capture the same bundle twice (before and after a change) to produce a diffable comparison.

## Steps

### 1. Choose an evidence directory
```bash
EVIDENCE_DIR=/tmp/node-evidence-before
mkdir -p "$EVIDENCE_DIR"
```
(Use a `-before` and a `-after` dir around a change.)

### 2. Snapshot the live graph
```bash
tatolab graph --node <runtime_id> > "$EVIDENCE_DIR/graph.json"
```
Read the channel names you want to tap out of this snapshot (`<runtime_name>/<node>/<port>` — see `inspect-live-graph`).

### 3. Tap N bags per channel to disk
`tap` has NO `--output` flag — redirect stdout. Repeat per channel:
```bash
tatolab tap --node <runtime_id> lab-one/camera/frames --count 30 > "$EVIDENCE_DIR/frames-camera.json"
tatolab tap --node <runtime_id> lab-one/convert/frames --count 30 > "$EVIDENCE_DIR/frames-convert.json"
```
Each file holds the hex-preview-plus-byte-length sample for that channel (bytes-flowing proof, not decoded pixels).

### 4. Capture a bounded log excerpt
```bash
tatolab logs --node <runtime_id> --count 200 > "$EVIDENCE_DIR/logs.txt"
```
`--count` bounds the sample of the runtime event stream (all topics) within a short window. In this live mode `logs` is addressed by `--node` and bounded by `--count` — there is no positional `runtime_id` here (that form is the offline on-disk log reader, a different mode).

### 5. Confirm and hand off the bundle
```bash
ls -l "$EVIDENCE_DIR"
```
You now have `graph.json`, one `frames-<channel>.json` per tapped channel, and `logs.txt`. Attach them to the PR/issue (upload frame/graph artifacts with the `attach-artifact` skill), or diff a `-before` dir against a `-after` dir to show a change's effect.

## Notes
- Every verb targets the node with `--node` (or none, when one node is live).
- All three are read-only — capturing evidence never mutates the graph.
- Do not invent a `--output` flag on `tap` or `graph`; redirection is the mechanism.
