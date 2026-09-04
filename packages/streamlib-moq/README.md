# streamlib-moq

Media over QUIC publish and subscribe for [StreamLib](https://github.com/tato123/streamlib), as a
capability extension wheel: Rust inside, two ordinary `@processor` classes as the binding.

```bash
pip install streamlib-moq
```

```python
from streamlib_moq import MoqBroadcastPublisher, MoqBroadcastSubscriber
```

`MoqBroadcastPublisher` takes one fan-in input, `tracks` — each inbound link becomes one MoQ
track named by its channel. `MoqBroadcastSubscriber` emits `encoded_video` and `encoded_audio`,
filling every key of the wire contract from the stream itself.

Both speak two container formats, chosen with `container_format`:

- `"cmaf"` (the default) lays the broadcast out the way `moq-pub` does — a `.catalog` track, an
  init track, and media objects that are self-contained `moof` + `mdat` fragments — so `moq-js`
  and `moq-sub` can play it.
- `"streamlib_bag"` carries the bag's own keys as msgpack, which is the only way the producer's
  ordering pair and stamp survive a hop byte-exact.

Both sit on the encoded side of the codec blocks: `H264Encoder` / `OpusEncoder` upstream of the
publisher, `H264Decoder` / `OpusDecoder` downstream of the subscriber.

Cloudflare's draft-16 relays are provisioned per account and carry their token in the URL path,
so `relay_url` is `https://draft-16.cloudflare.mediaoverquic.com/<token>`.
