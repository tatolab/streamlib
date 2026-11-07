# tatolab-moq

MoQ gateway nodes for [Tatolab](https://tatolab.com) streams, as an extension wheel: the MoQ
client in Rust inside, four ordinary `@node` classes as the binding. Nothing leaves a stream
unless its author adds one of these nodes.

> **New to Tatolab? Install the Tatolab app first.** This package adds four nodes to a stream
> project; the Tatolab runtime, which comes with the app, is what runs them. See
> [Install Tatolab](https://tatolab.com/docs/install) and the
> [Quickstart](https://tatolab.com/docs/quickstart).

In a stream project:

```bash
uv add tatolab-moq
```

```python
from tatolab.moq import MoqGateway, MoqTrack, MoqTrackReceive, MoqTrackSend
```

- `MoqGateway` holds one connection to a relay and publishes one broadcast there, served as
  `<broadcast>.hang`. Its config names the relay: `kind` (`moq-relay`, `cloudflare` or
  `moqpro`), `broadcast`, and the kind's own keys (`relay_url`, `token`,
  `certificate_fingerprint`; `relay_url`; `project_id`, `signing_key`). A Records message on its
  `settings` input replaces the config's values, which is how a node reading a secret store hands
  over a credential.
- `MoqTrack` declares one track of a gateway's broadcast: `gateway` (the reference `add`
  returned), `track`, and an optional `catalog` entry replacing its send's upstream entry. It has
  no ports; hold it in a `SetVariable` and read it anywhere with a `GetVariable`.
- `MoqTrackSend` writes each message of `content` (Packets, Records, Channels or CPU-resident
  Array) to the `track` it names by reference. One send per track.
- `MoqTrackReceive` reads `track` of `broadcast` at the relay its `gateway` dials, each name
  exactly as its publisher wrote it, and writes it on `content` as `family` (`packets` with a
  `codec`, or `records`), stamped on this machine's clock. `max_age` is how late a group may
  arrive, in milliseconds of the track's own time; 100 by default.

The send, track and receive nodes reach their gateway over iceoryx2 shared memory; only the
gateway's process touches the network.
