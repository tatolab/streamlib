---
name: read-live-track
description: Read a bounded run of objects off one exposed output port of a running StreamLib stream, as a track at the machine's local relay, into a directory — to confirm data is actually flowing, or to get an encoded port's frames onto disk for decoding. Use to answer "are frames/samples really moving out of this port?" — after inspecting the graph, or to spot-check a port before and after a stream-code change. Wraps `tatolab expose` and `tatolab-read-track`.
---

# read-live-track

Reads a port the way any user reads one: as a MoQ subscriber at a relay. The runtime has no door of its own for this — an exposed port is a track at the machine's local relay, and `tatolab-read-track` is a stock subscriber that writes what it reads to files. An internal port is not a track; exposing it private makes it one, readable on this machine only.

## Steps

### 1. Get the address from the live graph
Run `inspect-live-graph` and read the machine's name (`machine`), the stream's name (`stream`), the node's `name` and the output port under its `ports.outputs`. A port is the track `<node>/<port>` of the broadcast `<machine>/<stream>.hang`:
```
stream lab, node encoder, port encoded_video, machine desk
→ broadcast desk/lab.hang, track encoder/encoded_video
```
`graph`'s `exposed` lists the ports already exposed; so does the runtime's loopback listing, `curl -s http://127.0.0.1:4446/`.

### 2. Expose the port if it is internal
```bash
tatolab expose lab encoder encoded_video        # private: this machine only
```
Make it internal again when the read is done (`tatolab expose --remove lab encoder encoded_video`) — only a port you exposed. On a kept stream, both are recorded over what its function exposes, across restarts.

### 3. Read a bounded run of objects
```bash
cargo build --release -p tatolab-read-track
target/release/tatolab-read-track http://127.0.0.1:4443 desk/lab.hang encoder/encoded_video \
  --count 30 --out /tmp/lab-encoded-video
```
`http://127.0.0.1:4443` is the local relay's loopback listener, which serves every private and public port; a runtime of the session's own serves it at the port its `tatolabd` was given (`drive-running-stream`). It is a development tool built from this checkout, never part of the runtime unit. A track the catalog does not list as video or audio is read as sent once `--catalog-wait-at-most` seconds (default 5) pass without an entry. It ends once `--count` objects are written, and fails naming how many arrived when they do not within `--wait-at-most` seconds (default 30). A relay URL carrying a credential is passed as `-` and read from stdin, never as an argument.

### 4. Read the dump
The directory holds one `object-NNNNNN.bin` per object, `objects.jsonl` indexing them (`index`, `group_sequence`, `starts_group`, `timestamp_ns`, `file`, `payload`), and `catalog_entry.json`, the track's entry in the broadcast's catalog when one listed it. `payload` says what each file holds:
- **`codec_payload_of_the_hang_legacy_container`** — an encoded video or audio port: each file is one codec packet. An H.264 / H.265 track's files concatenated in index order are an Annex-B `.h264` / `.hevc` stream ffmpeg decodes (`verify-live` § Reading pixels); an Opus track's are one packet each, with no container joining them. The dump opens on a group's first object, where a decoder can start.
- **`as_sent`** — any other port: the msgpack record the node wrote. Decode it with the port's wire contract (`docs/plan/ARCHITECTURE.md`). A raw-frame port's record carries a surface id, which means nothing outside the runtime — to see its pixels, chain an encoder off it and read that.

Interpreting it:
- **`--count` objects written, sizes non-zero** — the port is producing.
- **`timestamp_ns` advancing at the expected cadence** — live data rather than a stuck producer.
- **`0 of M objects … arrived within …`** — most often the broadcast was never announced: the reader waits for `<machine>/<stream>.hang` rather than refusing it, so a misspelt machine or stream, or a stream with nothing exposed, ends here. Check the address against `graph` and the port against `exposed` (or `curl -s http://127.0.0.1:4446/`) first; then that the node is `Running` and its counters move (`inspect-live-graph`).
- **`N of M objects … arrived within …`, N above 0** — the port is slow or quiet; read again with a larger `--wait-at-most`.
- **`the track … was refused`** — the broadcast is announced but does not carry that track; re-read the port's name off the graph.

## Notes
- Read-only toward the stream: a subscriber never blocks the producer or disturbs its other readers. Exposing is the one change, and it is the user's own verb.
- An internal port is seen through its stream's logs alone unless it is exposed or a node is chained off it.
- In a browser, `http://127.0.0.1:4446/watch/<machine>/<stream>/<node>/<port>` plays an encoded video track.
