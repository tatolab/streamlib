# streamlib-moq

Media over QUIC publish and subscribe for [StreamLib](https://github.com/tato123/streamlib), as a
capability extension wheel: Rust inside, two ordinary `@processor` classes as the binding.

```bash
pip install streamlib-moq --index-url https://tatolab.github.io/streamlib/simple/
```

The same index serves the `streamlib` wheel this depends on, so one `--index-url`
installs both. PyPI publication waits for the project rename; the artifact is
identical either way.

```python
from streamlib_moq import MoqBroadcastPublisher, MoqBroadcastSubscriber
```

`MoqBroadcastPublisher` takes one fan-in input, `tracks` — each inbound link becomes one MoQ
track, named by the container: `{track_id}.m4s` on `cmaf`, the link's own channel name on
`streamlib_bag` unless `track_names` says otherwise. `MoqBroadcastSubscriber` emits
`encoded_video` and `encoded_audio`, filling every key of the wire contract from the stream
itself.

A link's first bag settles what its track carries. A bag with a `bitstream` key is encoded
media — `H264Encoder` or `OpusEncoder` output, by its `codec`. A bag without one is **data**:
any bag at all — a map of numbers, strings, binary, lists and nested maps — published as a
track beside the video and audio, on the same time-synced transport. Data rides
`streamlib_bag` only; a `cmaf` broadcast refuses a data bag by name at its first one. Each
data object carries the bag whole under `bag`, beside a per-track `sequence_index` the
publisher mints and the bag's own `timestamp_ns`, so nothing in the user's bag is renamed or
reserved. A data object never cuts a MoQ group; a broadcast with no video is cut by two
backstops instead — 128 objects, or an open group about a second old — so a late joiner replays
at most a second of history.

```python
publisher = rt.add(
    MoqBroadcastPublisher,
    config={
        "relay_url": relay_url,
        "container_format": "streamlib_bag",
        "track_names": ["video", "audio", "telemetry"],
    },
)
rt.connect(video_encoder.output("encoded_video"), publisher.input("tracks"))
rt.connect(audio_encoder.output("encoded_audio"), publisher.input("tracks"))
rt.connect(telemetry_probe.output("telemetry"), publisher.input("tracks"))
```

`track_names` names the tracks positionally, in the order the links were wired, so a
subscriber in another node can ask for `telemetry` rather than a channel name minted at
`add`. One name per link, refused by name at `setup()` when the counts differ; refused under
`cmaf`, whose track names are the interop contract a subscriber not asked to fetch a catalog
hardcodes.

Both speak two container formats, chosen with `container_format`:

- `"cmaf"` (the default) lays the broadcast out the way `moq-pub` does — a `.catalog` track, an
  init track, and media objects that are self-contained `moof` + `mdat` fragments — so `moq-js`
  and `moq-sub` can play it.
- `"streamlib_bag"` carries the bag's own keys as msgpack, which is the only way the producer's
  ordering pair and stamp survive a hop byte-exact.

Both sit on the encoded side of the codec blocks: `H264Encoder` / `OpusEncoder` upstream of the
publisher, `H264Decoder` / `OpusDecoder` downstream of the subscriber.

`relay_url` is required and has no default: Cloudflare's draft-16 relays are provisioned per
account and carry their token in the URL path, so it reads
`https://draft-16.cloudflare.mediaoverquic.com/<token>`.
