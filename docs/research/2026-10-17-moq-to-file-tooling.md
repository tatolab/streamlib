# Research memo: what writes a hang track at a MoQ relay to a file today?

2026-10-17, for the moq-relays change (`docs/plan/changes/moq-relays.md`), which deletes
`Mp4Sink` on the ground that "a stream's last mile — writing a track to a file — is the
user's, through any MoQ client of a relay", and whose plan text said "ffmpeg reads the MoQ
URL". Read against `moq-dev/moq` at `c0238812` (2026-10-17, `main`), FFmpeg `n9.0.2` and
`master`, the FFmpeg Forgejo PR tracker, and `bluenviron/mediamtx` `main`. One live run was
made against the public demo relay (see §Live run).

Evidence is tagged **[V] verified** (source line or page read), **[M] measured here** (a
command run on this machine) and **[I] inferred** (reasoning, not observed).

## Question

1. Does stock ffmpeg (release or master) have any MoQ / moq-lite / WebTransport input?
2. What does moq-dev's CLI offer for subscribing to a broadcast and writing it out?
3. What other MoQ clients can dump or record a hang track, and in which dialect?
4. What is the shortest working path today from a public hang video track to an `.mp4`?
5. Does hang's `cmaf` container kind change the answer?

## Answer

**ffmpeg cannot read a MoQ URL.** Neither FFmpeg 9.0.2 nor git master has a MoQ,
WebTransport, QUIC or HTTP/3 protocol or demuxer; the one MoQ change in flight (Forgejo PR
#23263) is an output-only muxer (`-f moq`, publish through `moq-c`), still open with blocking
review findings. **The last mile is moq-dev's own `moq` CLI**, not ffmpeg: `moq --connect
<relay> --broadcast <name> export fmp4 > out.mp4` reads a hang broadcast in either dialect the
relay speaks and writes fragmented MP4 (also `mkv`, `ts`, `flv`, raw `h264`/`h265`) to stdout,
from which ffmpeg can remux or transcode through a pipe. It consumes the `legacy` container
StreamLib will publish (the public demo broadcast is `legacy` too) and needs only what the
plan's catalog entry already carries (codec string, coded extent). The other readers — the
`moqsrc` GStreamer element (same stack, elementary streams out), `export archive`
(moq-archive's own object-store layout, not MP4), MediaMTX's MoQ client (IETF drafts 16–19
with an MSF catalog, so not a hang broadcast) and the browser `@moq/watch` player (no
recording) — do not shorten it. **`cmaf` changes nothing for the CLI path**: `export fmp4`
re-muxes either kind; it only matters for a future ffmpeg demuxer, since the FFmpeg PR and
`draft-ietf-moq-cmsf` are fMP4-shaped. So "ffmpeg reads the MoQ URL" is wrong as written; the
accurate sentence is "the `moq` CLI reads the relay and ffmpeg reads its stdout".

## Evidence

### 1. FFmpeg has no MoQ input

- [V] `libavformat/protocols.c` on `master` declares 57 `URLProtocol`s; none is moq,
  webtransport, quic or http3 (nearest: `ff_http_protocol`, `ff_https_protocol`).
  <https://raw.githubusercontent.com/FFmpeg/FFmpeg/master/libavformat/protocols.c>
- [V] `libavformat/allformats.c` on `master` has no demuxer or muxer named moq, webtransport,
  quic or warp; `ff_hls_demuxer`, `ff_dash_demuxer` and `ff_whip_muxer` are the nearest.
  <https://raw.githubusercontent.com/FFmpeg/FFmpeg/master/libavformat/allformats.c>
- [V] The same `protocols.c` at tag `n9.0.2` (newest release, 2026-09-18 per
  <https://ffmpeg.org/download.html>) has no such line either.
- [M] Local `ffmpeg version 8.0`: `ffmpeg -protocols` and `-demuxers` list nothing matching
  moq / quic / webtransport.
- [V] Patchwork search for "moq" returns one series: "Media over QUIC (MoQ) support for FFmpeg
  using fragmented mp4 (PR #23263)", 2026-05-28.
  <https://patchwork.ffmpeg.org/project/ffmpeg/list/?q=moq&archive=both>
- [V] FFmpeg Forgejo PR #23263 (author Ole Andre Birkedal): **open**, adds
  `libavformat/moqenc.c` — a *muxer*; "An initial implementation of MoQ for FFmpeg using
  fragmented mp4 (fMP4)"; depends on `libmoq` (now `moq-c`) via pkg-config; usage is
  `-f moq http://<relay>`; reviewers cite `draft-ietf-moq-cmsf-01` for the one-track-per-stream
  layout. No demuxer, subscribe or input side anywhere in the PR. Review on 2026-09-10 still
  reports "major issues"; the author's 2026-10-06 comment says the timing problem cannot be
  fixed with the current approach and the design is moving to moq-c's muxer; the PR shows
  merge conflicts and no maintainer decision.
  <https://code.ffmpeg.org/FFmpeg/FFmpeg/pulls/23263>
- [V] The one QUIC transport PR, #23478 "avformat/http3: add HTTP/3 (QUIC) protocol over
  ngtcp2/nghttp3", was closed 2026-06-16 (maintainer opposed; author withdrew in favour of a
  libcurl approach). Neither MoQ nor WebTransport is mentioned.
  <https://code.ffmpeg.org/FFmpeg/FFmpeg/pulls/23478>
- [V] `moq-c` 0.7.0 does expose a consume side (`moq_consume_catalog`, `moq_consume_video`,
  `moq_consume_audio`, `moq_consume_track`, `moq_consume_frame`) — `rs/moq-c/README.md:113-135`
  — so a demuxer is buildable on it; nobody has filed one. [I]
- Forks: no out-of-tree ffmpeg fork with a MoQ input was found. Fraunhofer FAMIUM MoQ claims
  "full FFmpeg integration (ffmpeg as publisher, ffplay as subscriber)" on
  draft-ietf-moq-transport-18, but names no repository and sells commercial licences
  (<https://www.fokus.fraunhofer.de/en/fame/solutions/moq.html>) — **unverified**, and not
  stock ffmpeg either way.

### 2. The `moq` CLI (crate `moq-cli`, binary `moq`)

Read at `rs/moq-cli` 0.14.2 in the tree (`Cargo.toml:8`); crates.io's newest is 0.14.6,
published 2026-10-16 (<https://crates.io/api/v1/crates/moq-cli>). `hang` 0.21.11, `moq-net`
0.3.10, `moq-tokio` 0.19.23, `moq-relay` 0.17.2, `moq-mux` 0.10.11 in the same tree. The `hang`
crate ships no binary; `moq` is the CLI (`rs/moq-cli/Cargo.toml:19-21`).

- [V] Grammar (`rs/moq-cli/src/args.rs:1-30`): `moq <MoQ side> <import|export> <endpoint>
  [-- <stage>]...`, plus `play`, `announced`, `fetch`. The MoQ side is `--connect <url>`
  (WebTransport `https`/`http`, WebSocket `ws`/`wss`, raw QUIC `moqt`/`moql`, qmux `tcp`/`unix`;
  `http://` first fetches `/certificate.sha256` for a self-signed fingerprint —
  `rs/moq-tokio/src/connect.rs:496-503`) and `--broadcast <name>` (`args.rs:420-428`).
- [V] Export sinks (`args.rs:967-993`): `fmp4` "Fragmented MP4 / CMAF to stdout", `mkv`
  "Matroska / WebM to stdout", `ts`, `flv`, `h264` / `h265` "Annex-B elementary stream to
  stdout", and the gateways `hls`, `rtmp`, `srt`, `rtc`, `archive`. Rendition selection
  `--video-name`, `--audio-name`, `--video-codec`, `--audio-codec`, `--no-video`, `--no-audio`
  (`rs/moq-cli/src/subscribe.rs:91-115`); `--catalog-format hang|hangz|msf` (`:30-34`).
- [V] Documented commands (`doc/bin/cli.md:106-110`, `rs/moq-cli/README.md:44-49`):
  `moq ... export --no-video fmp4 > audio.mp4`, `moq ... export --video-name hd --no-audio
  mkv > hd.mkv`, `moq --connect https://relay.example.com --broadcast my-stream.hang export ts
  | ffplay -`. The README's own framing: "It works with FFmpeg for encoding and decoding."
- [V] fMP4 semantics (`doc/bin/cli.md:115-127`): one fragment per publisher group,
  `--fragment-duration` caps it; the init segment declares every enabled rendition once and
  the track set is then fixed; "Annex-B H.264 and H.265 whose catalog codec string and
  dimensions fix the sample entry are described at once as `avc3` or `hev1`, with SPS, PPS,
  and VPS kept in the samples" — exactly the shape the plan's `H264Encoder` entry has
  (`avc1.PPCCLL`, `codedWidth`/`codedHeight`, no `description`, Annex-B access units —
  `docs/plan/changes/moq-relays.md:117-120,127-128`). `--max-delay` (default 500 ms) skips a
  stalled group. Opus is muxed with its TOC-stated durations
  (`rs/moq-mux/src/container/fmp4/muxer.rs:58,95`).
- [V] `moq fetch <track> [--group N] [--json]` writes one group's raw frames to stdout
  (`rs/moq-cli/src/fetch.rs:1-32`); the relay's `GET /fetch/<broadcast>/<track>?group=N` is the
  same over HTTP (`doc/bin/relay/http.md:16`). Raw `legacy` frames, no container.
- [V] `export archive <file:///dir | s3:// | gs:// | az://>` records every track into
  moq-archive's own layout — per-track `.info` JSON plus a binary segment envelope, "versioned
  hang recordings on any ObjectStore" (`rs/moq-archive/README.md`, `rs/moq-cli/src/archive.rs:22-30`);
  `import archive` replays it. Not an MP4; the exporters above are the way to one.
- [V] `export hls --listen` serves HLS/DASH, which ffmpeg can read — but "Export never
  subscribes to media. It reads each rendition's timeline track … A broadcast whose catalog
  advertises no timelines is skipped" (`doc/bin/hls.md:20-33`). The plan's catalog entries
  carry no timeline (`moq-relays.md:101-123`), so this path is closed to a StreamLib broadcast
  until a publisher writes timelines. [I]
- [V] Dialects: `moq-net` advertises ALPNs `moq-lite-06..03`, `moql`, and `moqt-22..14`
  (`rs/moq-net/src/version.rs:24-38`), with SETUP negotiation of lite-02/01 and draft-14
  (`:12-16`). The CLI therefore reads a broadcast from `moq-relay` (moq-lite) or from an IETF
  draft-14–22 relay alike; what it needs on top of the transport is a hang catalog.
- [V] Known defect, fixed: #4770 "moq export fmp4: a video + audio export starts at the first
  video keyframe; audio published before it is not written" — closed 2026-10-06 by PR #4883
  (<https://github.com/moq-dev/moq/issues/4770>). Recordings on a CLI older than that lose
  leading audio.

### 3. Other readers

| Reader | What it writes | Dialect / catalog | Maturity |
| --- | --- | --- | --- |
| `moqsrc` (GStreamer, `rs/moq-gst` 0.4.12; apt/rpm/nix packages) | one pad per rendition: `video/x-h264, alignment=au, stream-format=avc3\|avc\|byte-stream`, `video/x-h265`, `video/x-av1`, `audio/x-opus`, `audio/mpeg` (`rs/moq-gst/src/source/imp.rs:1297-1370`) — so `! h264parse ! mp4mux ! filesink` is the user's muxer [I] | same stack as the CLI: moq-lite + drafts, hang catalog | released; README shows only decode-to-window pipelines (`rs/moq-gst/README.md:40-47`), no recording example |
| `moq export archive` | moq-archive object layout | hang | 0.0.11; replay via `import archive` or `export hls` |
| MediaMTX (`internal/protocols/moq`) | records to fMP4 or MPEG-TS (`recordFormat: fmp4`); `source: moqt://host:port/path` pulls "from another MoQ server" (`mediamtx.yml`) | ALPN `moqt-16..19` only (`internal/defs/api_moq.go:31-34`); catalog "spec: draft-ietf-moq-msf-00" with a `packaging` field (`internal/protocols/moq/catalog/catalog.go:5,14`) | released; **cannot read a hang broadcast** (no moq-lite, no hang catalog) [I] |
| `@moq/watch` / `@moq/hang` (browser) | nothing to disk: no `MediaRecorder`, `Blob` or `createObjectURL` in `js/watch/src`, `js/hang/src`, `js/publish/src` [V]; the only "download" is the `visible` attribute gating video fetch (`js/watch/README.md:70-88`) | moq-lite over WebTransport/WebSocket | released |
| moxygen (Meta) | text / FLV samples; README names no file writer | drafts 16 and 18 (<https://github.com/facebookexperimental/moxygen>) | interop testing; no hang |
| FFmpeg PR #23263 | publish only (§1) | moq-c, CMSF-shaped fMP4 | open, unmerged |

### 4. Shortest working path

[V] The public demo broadcast `bbb.hang` at `https://cdn.moq.dev/demo` is a `legacy`
container hang broadcast with `avc3.64001f` 1280×720 and `avc3.64001e` 640×360 video,
`mp4a.40.2` audio, a `timeline.z` archive track — read with
`curl https://cdn.moq.dev/fetch/demo/bbb.hang/catalog.json` (HTTP 200). It is a stand-in for a
StreamLib track: same container, same codec-string-plus-extent shape, no `description`.

#### Live run

[M] `moq` 0.14.2 built from the tree read (`cargo build -p moq-cli --release
--no-default-features --features quic`, 6 min), local `ffmpeg 8.0`; the CLI negotiated
`version=moq-lite-06 transport=webtransport` with `cdn.moq.dev`. Each command ran for 10–12 s
under `timeout -s INT`:

```sh
# 1. relay -> fragmented MP4 on disk (both video renditions and the audio unless selected)
moq --connect https://cdn.moq.dev/demo --broadcast bbb.hang export --video-name 0.avc3 fmp4 > hd.mp4
#    ffprobe: h264 1280x720 + aac; start_time=382654.5 (the broadcast clock), a valid fMP4
# 2. flat .mp4 with zero start time, no re-encode
ffmpeg -i hd.mp4 -c copy flat.mp4
#    ffprobe: duration=10.22 s, 24/1 fps, 236 frames counted
# 3. the same in one pipe
moq --connect https://cdn.moq.dev/demo --broadcast bbb.hang export --video-name 0.avc3 fmp4 \
  | ffmpeg -i pipe:0 -c copy piped.mp4          # exit 0, 10.85 s, h264 + aac
# 4. the raw Annex-B path, no container on the wire side
moq --connect https://cdn.moq.dev/demo --broadcast bbb.hang export --video-name 0.avc3 --no-audio h264 \
  | ffmpeg -f h264 -i pipe:0 -c copy annexb.mp4 # exit 0, 10.00 s, h264 1280 wide
```

Command 1 alone is the answer to the question as asked: one line, one binary, a playable
`.mp4`. Commands 2–3 are what "ffmpeg reads" honestly means. Not reproduced: `moq fetch
0.avc3 --json` subscribed and was "canceled (idle)" without printing a group inside 30 s —
the group-dump path is unproven here, and nothing on the file path depends on it.

The remaining gap for a StreamLib broadcast, once the sharing step ships: none in tooling —
the user installs `moq` (`curl -fsSL https://moq.sh | sh`, `cargo install moq-cli`, or the
`moqdev/moq` Docker image, `rs/moq-cli/README.md:6-22`) and runs `export fmp4` against the
local relay (`http://<host>:<port>/<path>`, the self-signed fingerprint fetched for them) or a
hosted one. What the plan says must change: the sentence "ffmpeg reads the MoQ URL" — ffmpeg
reads the `moq` CLI's stdout. [I]

### 5. Does `cmaf` change it?

- [V] hang's three kinds (`rs/hang/src/catalog/container.rs:17-33`, `doc/concept/hang.md:181-187`):
  `legacy` "varint microsecond timestamp + codec payload — The default. Cheapest."; `cmaf`
  "`moof` + `mdat` — fMP4 passthrough for HLS/DASH interop; ~100 bytes per frame", carrying
  its `ftyp+moov` init in the catalog; `loc` the IETF container.
- [V] The CLI's export reads all three: `catalog/hang/container.rs:24` dispatches
  `Container::Cmaf { init }` to `fmp4::Wire::from_init`, and `container/source.rs:544-562`
  derives the sample entry from the catalog for `legacy`/`loc` while "CMAF keeps the init
  segment it was declared with". So `export fmp4 > out.mp4` works unchanged either way. [I]
- [I] Where `cmaf` would matter: a future ffmpeg demuxer or any CMSF/MSF-shaped reader
  (FFmpeg PR #23263, MediaMTX's `packaging: cmaf`) expects fMP4 fragments on the wire; a
  `legacy` track is unreadable to those without moq-mux in front. The plan's OPEN "CMAF-emitting
  node for HLS players and stock MoQ tooling" (`ARCHITECTURE.md:4030`) is about that class of
  reader, not about the file path, which `legacy` already serves.

## What remains unknown

- Whether a StreamLib-published broadcast (hang catalog written by the publisher in `tatolabd`,
  `avc1.PPCCLL` codec string) is accepted by `export fmp4` exactly as the demo's `avc3.…` one
  was — the demo proves the container and the extent-only entry, not the StreamLib publisher.
- The `cmaf` export was not run (no public `cmaf` broadcast found); the claim rests on reading
  `moq-mux`.
- FAMIUM's ffplay subscriber and the FFmpeg PR's post-2026-10-06 redesign are unverified beyond
  the pages read.
- Only the moq-lite dialect was exercised (`moq-lite-06` against `cdn.moq.dev`); the CLI's
  reading of a hang broadcast through an IETF-draft relay (Cloudflare) rests on
  `rs/moq-net/src/version.rs` alone.
- Why `moq fetch --json` printed no group in 30 s (a live-edge group that never closed is the
  likely reason [I]) was not pursued.
