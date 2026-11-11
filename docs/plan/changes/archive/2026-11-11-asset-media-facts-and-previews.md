# asset-media-facts-and-previews

> **Asked for by Tato Studio, 2026-11-10** (#3498, filed from tatolab-app#247). The Library shows
> each asset as a card with a preview picture and a line of media facts ("0:42 · 1080p",
> "3:12 · 48 kHz", "2400 × 1200", "33³ LUT"), and its details pane shows a poster frame and a
> fuller kind line ("media · video, H.264, 1920×1080, 30 fps"). The app reads the runtime only
> through its local API and decodes no media itself, so the runtime has to serve both. The
> issue leaves the contract's shape to the runtime.

**Scale gate — this skill.** It adds fields to two asset tools and one new operation on the local
API. It touches no RHI, IPC wire format, processor model or Python node contract, so it carries
no ADR.

**Precondition.** It extends the DECIDED project entry under §Processor model & scheduling and
§Control plane, both IN-FLIGHT under project-and-assets. It settles no OPEN entry.

## Current state (recon, `main` `69eb6ce`)

- **What an asset reports.** `list_assets` answers each asset as `ProjectAssetOnTheWire`
  (`runtime/streamlib-runtime-client-contract/src/local_api_wire_contract.rs:544`): `asset`,
  `kind`, `path`, `content_sha256`, `content_byte_len`, `import_settings`. `describe_asset`
  (`runtime/streamlib-api-server/src/mcp_project_asset_tools.rs:316`) adds only
  `file_at_its_path` (`local_api_wire_contract.rs:785`). Nothing reads what the bytes hold.
- **Kinds.** `ProjectAssetKind` (`runtime/streamlib-project-repository/src/project_asset.rs:31`)
  is `stream`, `media` (video, audio or an image), `scene` and `data`, inferred from the file's
  extension at import (`project_asset.rs:44`). `media` does not say which of the three it is.
- **Bytes.** An asset's bytes are the one file at its path; history records a media, scene or
  data asset by its sidecar alone. `.tatolab/` is the runtime's own folder, which
  `read_project_file` refuses (`runtime/streamlib-project-repository/src/project_file_access.rs:53`)
  and an import copy skips (`runtime/streamlib-project-repository/src/project_import_copy.rs:60`).
  It already holds `.tatolab/uploads` and `.tatolab/staged-imports`.
- **Bytes over the local API.** `read_project_file`
  (`runtime/streamlib-api-server/src/mcp_project_file_tools.rs:129`) answers a file as
  `content_base64` and `byte_len`. No tool answers an image a consumer can show.
- **What the runtime can read today.** No container demuxer (MP4, MOV, MKV), no image decoder
  (`png` is a dev-dependency only, `runtime/streamlib-engine/Cargo.toml:185`), and no audio file
  reader in library code. `streamlib-video-bitstream` parses an H.264 or H.265 parameter set
  for its profile and level, not its picture size. The engine decodes H.264 and H.265 on the
  device through `OwnedDeviceVideoDecoder`
  (`runtime/streamlib-owned-device-video-codec-nodes/src/owned_device_video_decoder.rs:64`),
  Vulkan Video on Linux and VideoToolbox on macOS.
- **Scenes.** The runtime has no USD or glTF reader.

## The change

One reader per file family turns an asset's bytes into **media facts** and, where the family has
a picture, a **preview picture**. Both are derived from the bytes alone, so they are keyed by
the asset's `content_sha256` and kept as a cache the runtime can always rebuild. A
`replace_asset` gives new bytes a new hash, so a stale fact is never served.

### What a consumer reads

`describe_asset` gains `media_facts` and `preview`. `list_assets` gains the same two on each
asset, filled from the cache only, so listing a project never reads a file. A field the runtime
could not read is `null`, never an error, and an asset whose facts are not yet read reports
`media_facts_state: "reading"`.

```json
"media_facts": {
  "duration_ms": 42000,
  "video": { "pixel_width": 1920, "pixel_height": 1080, "frames_per_second": 30.0, "codec": "h264" },
  "audio": { "sample_rate_hz": 48000, "channel_count": 2, "codec": "aac" },
  "image": null,
  "lut": null,
  "scene": null
},
"media_facts_state": "read",
"preview": { "pixel_width": 512, "pixel_height": 288, "media_type": "image/png" }
```

- `media_facts_state` is `read`, `reading` or `unreadable` (no file at the asset's path, no
  reader for the file, or the reader refused it; `media_facts_unreadable_because` carries the
  reader's own words), and `null` for a stream asset.
- `video` and `audio` are both present for a video with sound. `image` carries `pixel_width`
  and `pixel_height`. `lut` carries `lut_1d_size` or `lut_3d_size` and `domain_min`/`domain_max`.
  `scene` carries `prim_count`, `null` until the runtime has a scene reader.
- `preview` is `null` for an asset with no picture (audio, a LUT, data).

A new operation answers the picture: `read_asset_preview(project_directory, asset)` answers
`media_type`, `pixel_width`, `pixel_height`, `byte_len` and `content_base64`, the shape
`read_project_file` already uses. It is refused, in the runtime's words, for an asset with no
preview, for a stream asset, and while the asset's bytes are still being read.

### Where it is kept

`.tatolab/asset-media/<content_sha256>/<extension>/facts.json` and `preview.png`, so the same
bytes under another extension are read again by the reader that extension chooses. A reader that
fails on a file leaves the asset `unreadable`, never stops the reads. History never records
them and an import copy skips them with the rest of `.tatolab/`. A duplicated or imported
project rebuilds them on first read.

### When it is read

After `import_asset` or `replace_asset` commits, the runtime reads the new bytes off the async
runtime, beside the transaction, never inside it, so an import is never slower or refused
because of a file the runtime cannot read. A cache miss on `list_assets`, `describe_asset` or
`read_asset_preview` (an asset imported before this change, a project copied in) queues the same
read; the call itself reads no file. The runtime reads one asset at a time, on one thread of its
own. A project's change feed announces the asset again once its facts are read, so a consumer
subscribed to `transactions_resource_uri` can refresh a card.

The announcement is a feed entry with `status: "media_facts_read"` naming the asset, not a
transaction (owner, 2026-11-11).
Rejected: a consumer polling `describe_asset` while `media_facts_state` is `reading` — the Library
shows many cards and polling each is wasteful (owner, 2026-11-11).

### The readers

One module per file family in a new crate, `streamlib-asset-media-facts`, which the asset tools
call and nothing else does. Each reader takes a path and answers facts and an optional picture;
the crate also keeps the cache.

- **Images** (PNG, JPEG, WebP, GIF, BMP, TIFF, HDR, EXR): the `image` crate (MIT or
  Apache-2.0) reads the size from the header and decodes the picture for the preview, scaled to
  512 px on its longest edge.
- **Video containers** (MP4, MOV, M4V, M4A): box parsing reads duration from `mvhd`, and from
  each track's `mdhd`, `hdlr`, `stsd` and `stts` its picture size, frame rate and codec, or its
  sample rate, channel count and codec. MKV and WebM through an EBML reader in the same crate. AVI and MXF answer
  `unreadable` for now.
- **Audio files** (WAV, AIFF, FLAC, MP3, AAC, Ogg, Opus, CAF): the container probed through
  `symphonia` (see below), never decoded.
- **LUTs**: `.cube` files' `LUT_1D_SIZE` or `LUT_3D_SIZE` and domain lines, read as text.
- **Scenes**: no reader. `scene` is `null` and the asset has no preview until a scene reader
  exists, which this change does not add.

**The video poster frame** comes from the engine's own decoders, run in a helper the runtime unit
carries, `tatolab-video-poster-frame`, never in the runtime's process: the runtime finds an MP4 or
QuickTime file's first sync sample, hands it to the helper as one Annex-B access unit led by its
parameter sets, and scales the RGBA picture the helper answers to the 512 px preview. On Linux the
helper decodes on an owned Vulkan Video device with the decoder's RGBA readback; on macOS on
VideoToolbox, the NV12 picture converted on the CPU. A file, codec or driver fault ends only the
helper, which the runtime stops after 20 s, and the video keeps its facts with `preview: null`
(owner, 2026-11-11). H.264 and H.265 are the codecs it decodes; MKV and WebM have no poster yet.
Rejected: a CPU software decoder added for posters alone — a second decoder in the tree and a new
codec licence to carry (owner, 2026-11-11).
Rejected: decoding the poster inside the runtime's process — a driver fault on one file would end
every stream on the machine (owner, 2026-11-11).

**Compressed audio's facts** come from `symphonia`'s probe, which reads duration, sample rate and
channels for MP3, AAC, Ogg, Opus and more, under MPL-2.0, linked only into
`streamlib-asset-media-facts`. MPL-2.0 is file-level, so it adds no obligation to StreamLib's own
files (owner, 2026-11-11).

## Delta

### MODIFIED: §Processor model & scheduling, the project entry

Add after "Data on links and runtime state are never saved.":

> An asset's media facts and preview picture are read from its bytes by the runtime and kept in
> `.tatolab/` by the bytes' SHA-256, never in history, rebuilt whenever they are missing; a
> fact the runtime cannot read is absent, never an error (owner, 2026-11-11).

### MODIFIED: §Control plane, the asset tools

`describe_asset` and `list_assets` answer `media_facts`, `media_facts_state` and `preview`;
`list_assets` answers them from what is already read and reads no file.

### ADDED: §Control plane

- `read_asset_preview` answers an asset's preview picture, in the `read_project_file` shape.
  Under control-api-contract it is one `impl ControlOperation`, group `assets`, stability
  `experimental` until Studio has shipped against it.

### ADDED: crates

- `runtime/streamlib-asset-media-facts`, the readers. Dependencies: `image` (MIT or Apache-2.0)
  and `symphonia` (MPL-2.0).
- `runtime/tatolab-video-poster-frame`, the runtime unit's poster frame helper, beside `tatolabd`
  in `bin/` and in a macOS app's `Contents/MacOS`.

### REMOVED

Nothing. The change only adds.

## Order

1. **Facts and image previews.** The crate with the image, MP4/MOV, MKV/WebM, audio and LUT
   readers; the cache; the read after import and replace and on a miss; the new fields on
   `describe_asset` and `list_assets`; `read_asset_preview`; the change-feed entry. Proven by
   the crate's tests over files each test writes (an MP4, a WebM, a WAV, a PNG, a `.cube`) and an
   api-server test that imports a PNG, a `.cube` and an FMU, waits for each read on the change
   feed, and reads facts and a preview back.
2. **Video posters** through `tatolab-video-poster-frame`, proven by its hardware tests on each
   platform's decoder (`cargo test -p tatolab-video-poster-frame --features hardware-tests`).
3. Tato Studio's Library reads them (tatolab-app, after slice 1 ships in a runtime release).

## What this does not do

- No scene reader, so no prim counts and no scene previews.
- No waveform picture for audio.
- No facts for `stream` assets; their graph is already served by `graph`.
