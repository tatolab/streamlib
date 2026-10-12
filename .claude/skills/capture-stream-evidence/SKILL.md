---
name: capture-stream-evidence
description: Freeze a verifiable record of a running StreamLib stream — a live graph snapshot, N tapped bags from one or more channels on disk, and its log records — into a directory for a PR, an issue, or a before/after comparison. Use when you need durable proof that a pipeline is running and producing, or to capture the "before" and "after" around a stream-code change. Wraps `tatolab graph`, `tatolab tap`, and `tatolab logs` with shell redirection.
---

# capture-stream-evidence

Turns a live stream into on-disk artifacts nothing else touches. All three verbs print to stdout; none has an `--output` flag, so redirect stdout to files in a directory you control. Capture the same bundle twice (before and after a change) to produce a diffable comparison.

## Steps

### 1. Choose an evidence directory
```bash
EVIDENCE_DIR=/tmp/stream-evidence-before
mkdir -p "$EVIDENCE_DIR"
```
(Use a `-before` and an `-after` directory around a change.)

### 2. Snapshot the live graph
```bash
tatolab graph --stream <stream> > "$EVIDENCE_DIR/graph.json"
```
Read the channel addresses you want to tap out of this snapshot (`<stream>/<node>/<port>` — see `inspect-live-graph`). A graph whose nodes carry secrets in their config (WHIP / WHEP endpoints) is never saved — read it in a pipe instead.

### 3. Tap N bags per channel to disk
Repeat per channel:
```bash
tatolab tap <stream>/camera/video --count 30 > "$EVIDENCE_DIR/bags-camera-video.json"
tatolab tap <stream>/effect/video --count 30 > "$EVIDENCE_DIR/bags-effect-video.json"
```
Each file holds the hex-plus-byte-length sample for that port (bytes-flowing proof, not decoded pixels). For pixels, `tatolab exchange --channel <stream>/<node>/<port> --out "$EVIDENCE_DIR" --count N` writes PNGs and prints their paths (`verify-live`).

### 4. Capture the stream's log records
```bash
tatolab logs --stream <stream> > "$EVIDENCE_DIR/logs.txt"
```
Reads the records the runtime holds for that loaded stream, by sequence number, and returns; `-f` would keep following. `--level`, `--source`, `--processor`, `--pipeline` and `--rhi` narrow them. A stream already unloaded is read off disk instead: `tatolab logs --list` names its log files, and `tatolab logs <name>` renders one.

### 5. Confirm and hand off the bundle
```bash
ls -l "$EVIDENCE_DIR"
```
You now have `graph.json`, one `bags-<channel>.json` per tapped channel, and `logs.txt`. Attach them to the PR / issue (upload with the `attach-artifact` skill), or diff a `-before` directory against an `-after` one to show a change's effect.

## Notes
- Every verb names the stream; there is no runtime to pick — the machine has one.
- All three are read-only — capturing evidence never mutates the graph.
- Do not invent an `--output` flag on `tap`, `graph` or `logs`; redirection is the mechanism.
