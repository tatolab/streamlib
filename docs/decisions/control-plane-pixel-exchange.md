# Exchange is its own verb, not a mode of tap

Rationale for the `exchange` entries in `docs/plan/ARCHITECTURE.md` §Control plane &
observability. Change file: `docs/plan/changes/control-plane-surface-pixel-exchange.md`.

## Trigger

Read this before adding an argument to `tap` that would make it return pixels, before
adding a second surface-resolution path for "just this one caller", before putting a
window in a graph so that something can see a channel, and when someone asks why
reading a frame costs a claim on a pool slot.

## Decision

The control plane exposes one more observation verb: **`exchange` — a published surface
id in, image bytes out, reachable from outside the runtime process.** It is peer to
`graph` / `tap` / `logs`, never a mode of any of them.

Tap keeps exactly its shipped contract: bags forwarded verbatim as bytes, no decode, no
field named, no new argument. Composition happens entirely at the consumer — it decodes
its own bag, reads whatever field it knows carries a surface id, and calls `exchange`
with the id. The engine therefore still inspects no bag content anywhere.

Inside one call the order is the contract: resolve the id, claim the frame through the
pool's own claim seam, run the conversion and the GPU→CPU copy under that claim,
release, **then** encode and return. Encoding after the release is what bounds the claim
to the copy — milliseconds — so an encoder's cost can never extend the window a producer
is kept out of its own slot.

Conversion happens in the RHI or not at all. A camera frame is NV12 or YUYV and the
engine's own colour converter is what turns it into RGBA; a frame already published as
RGBA takes a buffer→image copy; a texture backing takes the RHI's existing display blit,
which is also the one place the optional long-edge downscale is spent. No second
converter, no second scaler, and no pixel walked on the CPU.

Staleness fails loud and composes as a retry. A retired `<slot>#<generation>` id is
refused as the recycled-frame error before any bytes move, so a successful exchange
returns exactly the tapped bag's frame, and a slow caller taps a newer bag and exchanges
that.

## Rejected alternatives

- **Resolve inside tap** — a flag that makes the tap decode each bag, notice a surface
  id and substitute pixels. Rejected: it puts bag interpretation in the engine, which
  the port/type-free rendering rule and the "engine inspects no bag content" clause both
  forbid; it couples the tap's non-interference guarantee (about a channel) to a pool
  claim (about a frame); and it makes every tap consumer pay for a capability most of
  them do not want.
- **A batched `exchange N frames` verb.** Temporal sampling falls out of composition —
  tap N bags, exchange each as it arrives — so a batching verb would add engine state
  (which frames, held how long) to buy what a client loop already has, while pinning N
  pool slots at once instead of one.
- **Window capture: put a `DisplayWindow` in the graph and screenshot it.** The observer
  effect *is* the problem being removed. It terminates the channel being observed, so a
  mid-graph channel is unobservable in the topology that ships; it needs a display
  server, so it cannot run headless; and it reads what a compositor drew rather than
  what the producer published. Window capture survives only where the window is
  genuinely the subject — the present and swapchain path.
- **Hand back raw unconverted planes.** Honest but unviewable: every caller would need
  the same YUV maths, in whatever language it is written in, and the RHI already has the
  shaders. Deferred until something needs planes specifically rather than pixels.
- **Copy without claiming.** A producer could recycle the slot mid-copy and the caller
  would receive a torn frame — half one frame, half the next. That is precisely the
  silent wrongness the surface-id lifetime contract exists to kill, so the copy is worth
  nothing without the claim around it.
- **A dedicated surface-resolution path for the control plane.** The doors the typed
  cast's CPU reach already rides resolve both backings and already refuse retired ids;
  a second resolver would be a parallel abstraction that drifts from the first exactly
  where correctness lives.

## Consequences

- An exchange costs the node a bounded copy and, at worst, memory: the pool skips
  claimed slots and grows to its cap, so a control-plane read can never set another
  processor's cadence. This is the same bargain every pool consumer already makes.
- Reading pixels no longer requires a window or a display server anywhere in the path,
  which is what makes headless verification and remote API consumption the same code
  path.
- Two spellings of one operation. The REST route serves the exact frame as a binary
  `image/png` body — lossless, full resolution, no base64 inflation, remote-capable —
  and is the evidence and PSNR path. The MCP tool returns an image content block
  downscaled by default to a declared cap, with the result stating the true extent and
  pointing at the exact-bytes route. Both are the same operation with the same
  arguments; the CLI verb is a pure JSON-RPC client of it.
- A PNG encoder enters library code. It is pure Rust end to end, so the wheel's
  portability contract — system libraries dlopen'd, our code compiled in — is untouched.
- The verb joins the bearer-gated set beside the tap WebSocket. That is mechanism
  parity, not a trust boundary the exchange imposes: whatever the open auth and
  remote-access question decides later, it decides for this verb the same as the rest.
- Latency is a client-shape question, not an operation cost. A warm client holding one
  connection round-trips on localhost in low single-digit milliseconds, inside the
  publish-to-claim window a 60 fps source with pool depth 4 allows. A cold process spawn
  per exchange stays honest at camera cadences and degrades to a loud retry above them,
  which is why the CLI's channel form composes tap → decode → exchange client-side in
  one warm process.
