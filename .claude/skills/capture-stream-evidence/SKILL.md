---
name: capture-stream-evidence
description: Freeze a verifiable record of a running StreamLib stream — a live graph snapshot, N objects read off one or more tracks it sends to a relay on disk, and its log records — into a directory for a PR, a Linear issue, or a before/after comparison. Use when you need durable proof that a pipeline is running and producing, or to capture the "before" and "after" around a stream-code change. Wraps `tatolab graph`, `tatolab-read-track`, and `tatolab logs`.
---

# capture-stream-evidence

Turns a live stream into on-disk artifacts nothing else touches. `graph` and `logs` print to stdout and have no `--output` flag, so redirect them into a directory you control; `tatolab-read-track` writes its own directory with `--out`. Capture the same bundle twice (before and after a change) to produce a diffable comparison.

## Steps

### 1. Choose an evidence directory
```bash
EVIDENCE_DIR=/tmp/stream-evidence-before
mkdir -p "$EVIDENCE_DIR"
```
(Use a `-before` and an `-after` directory around a change.)

### 2. Snapshot the live graph
`graph` renders every node's config, so a stream whose nodes carry secrets (a WHIP or WHEP endpoint carries its key in the URL) never has its graph saved: read it in a pipe (`tatolab graph --stream <stream id> | jq '.nodes[] | {name, ports}'`), record what you need by hand, and leave `graph.json` out of the bundle. Any other stream's graph is saved:
```bash
tatolab graph --stream <stream id> > "$EVIDENCE_DIR/graph.json"
```
Read the addresses of the tracks you want to read out of it — each `MoqGateway`'s `relay_url` and `broadcast`, and each `MoqTrack`'s `track` (see `read-live-track`).

### 3. Read N objects per port to disk
Repeat per track, adding gateway nodes for a port nothing sends yet (`read-live-track`). `RELAY_URL` is the gateway's `relay_url`:
```bash
RELAY_URL=http://127.0.0.1:14443
target/release/tatolab-read-track "$RELAY_URL" <broadcast>.hang <video track> \
  --count 30 --out "$EVIDENCE_DIR/track-<video track>"
target/release/tatolab-read-track "$RELAY_URL" <broadcast>.hang <audio track> \
  --count 30 --out "$EVIDENCE_DIR/track-<audio track>"
```
Each directory holds one file per object, `objects.jsonl` indexing each file with its group and timestamp — an object's size is its file's — and the track's catalog entry. An encoded video track decodes to PNGs with ffmpeg (`verify-live` § Reading pixels); any other holds the records as the node wrote them.

### 4. Capture the stream's log records
```bash
tatolab logs --stream <stream id> > "$EVIDENCE_DIR/logs.txt"
```
Reads the records the runtime holds for that loaded stream, by sequence number, and returns; `-f` would keep following. `--level`, `--source`, `--processor`, `--pipeline` and `--rhi` narrow them. A stream already unloaded is read off disk instead, from its project directory (or with `STREAMLIB_HOME=<project>`), whose `.streamlib/logs/` holds its log files: `tatolab logs --list` names them, and `tatolab logs <name>` renders one.

### 5. Confirm and hand off the bundle
```bash
ls -l "$EVIDENCE_DIR"
```
You now have `graph.json` (unless the stream carries secrets), one `track-<track>/` per track read, and `logs.txt`. Attach them to the PR or the Linear issue (upload with the `attach-artifact` skill), or diff a `-before` directory against an `-after` one to show a change's effect.

## Notes
- Every verb names the stream; there is no runtime to pick — the machine has one.
- Capturing evidence never mutates the graph; adding gateway nodes to send a port is the one change, and `read-live-track` says how to undo it.
- Do not invent an `--output` flag on `graph` or `logs`; redirection is the mechanism.
