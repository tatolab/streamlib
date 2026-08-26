# control-plane-surface-pixel-exchange

The control plane gains one composable door: hand it a published surface id, get image
bytes back — out of process, no window in the graph, no display server in the path. Tap is
untouched in every spelling. A consumer taps a channel, decodes the bag itself, notices a
surface id in it, and exchanges that id for the image — the rxjs-pipe shape: tap is the
observable, exchange is an operator the consumer applies. Ticket #1966. Delta against
`../ARCHITECTURE.md` §Control plane & observability only.

**Scale gate — new behavior plus a changed contract (the pinned control vocabulary), so
this skill.** No RHI primitive, no IPC wire field, no processor-model rule, no Python
authoring API — the door composes shipped machinery. The ADR trigger list does not fire;
an ADR rides anyway because the shape had a real alternative (resolving inside tap) that
the owner rejected for a reason worth recording:
`docs/decisions/control-plane-pixel-exchange.md`.

**Precondition note.** §Control plane & observability is IN-FLIGHT and carries one OPEN
entry — auth and remote-access posture (`ARCHITECTURE.md:681`). Every entry this change
touches is DECIDED, and the OPEN one is not on its path: the opt-in bearer gate already
covers the whole `POST /mcp` dispatch and the tap WebSocket
(`runtime/streamlib-api-server/src/auth.rs:4-19`), and the exchange joins that gated set
(below), so no new exposure question opens. If the owner reads the precondition
section-wide rather than entry-wide, this routes to `/align` first.

---

## ADDED: §Control plane & observability — the surface-id exchange

- **DECIDED** — The control plane exposes an `exchange` operation: a published surface id
  in, image bytes out, reachable from outside the runtime process. It is its own verb,
  peer to `graph` / `tap` / `logs` — never a mode of any of them. Tap keeps exactly its
  shipped contract: bags forwarded verbatim as bytes, no decode, no field named, no new
  argument. Composition happens entirely at the consumer: it decodes the bag itself, reads
  whatever field it knows carries a surface id, and calls `exchange` with the id. The
  engine therefore still inspects no bag content anywhere — the clause at
  `ARCHITECTURE.md:147` stands untouched, and so does the port/type-free rendering rule.
  This is how verification sees pixels; it is also how any API consumer sees them, because
  the door knows nothing about verification.

- **DECIDED** — The exchange is a pool claim, bounded to the copy. Inside one operation
  call: resolve the id, claim the frame through the pool's own claim seam (the refcount
  in-process, the checkout lease cross-process — the shipped seam, never a new one), run
  the GPU conversion and the GPU→CPU copy under the claim, release, then encode and
  return. Encoding happens after release, so the claim window is the copy alone —
  milliseconds — and an encoder's cost can never extend it. The producer never waits
  regardless: the pool skips claimed slots and grows to its cap, so an exchange costs the
  node memory at worst, never another processor's cadence (`ARCHITECTURE.md:147-149`).
  Without the claim, a producer could recycle the slot mid-copy and the caller would
  receive a torn frame — half one frame, half the next — which is precisely the silent
  wrongness the lifetime contract exists to kill.

- **DECIDED** — Tap's non-interference guarantee is untouched, because the two doors touch
  different systems. Tap's guarantee is about the channel: verbatim bags, completeness
  traded away and reported as `dropped_bags`, one reserved subscriber slot
  (`core/runtime/operations.rs:67-88`). The exchange never attaches to a channel; it is a
  pool consumer on the same terms as any typed cast in a downstream processor, one frame
  at a time.

- **DECIDED** — Staleness fails loud and composes as a retry, never as wrong pixels. A
  surface id is per-frame (`<slot>#<generation>`), and resolving a retired one refuses
  with the recycled-frame error before any bytes move
  (`ARCHITECTURE.md:132-137`; `linux/surface_share/unix_socket_service.rs:759-772`). So
  when an exchange succeeds, the bytes are exactly the tapped bag's frame — the
  generation grammar is what proves the pairing — and when it is too slow, the caller
  taps a newer bag and exchanges that. The publish-to-claim window is the one every pool
  consumer already obeys: it rides pool depth, and outwaiting it is an error
  (`ARCHITECTURE.md:141-146`). Sample-and-exchange-as-you-go is therefore the intended
  loop, and temporal sampling — tap N bags, exchange each as it arrives — falls out of
  composition rather than needing a batched verb. On the measured numbers: the 60 ms
  round-trip is CLI process-spawn plus connect, not the operation; a warm client (an MCP
  host holding its connection, or one script holding one HTTP connection) round-trips on
  localhost in low single-digit milliseconds, inside even the 60 fps / pool-depth-4
  window of ~66 ms. The cold-CLI spelling stays honest at camera cadences and degrades
  to a loud retry above them.

- **DECIDED** — The engine converts, in the RHI, or the caller gets nothing viewable. A
  camera frame is NV12 or YUYV; converting it is the RHI's existing job —
  `core/rhi/color_converter.rs`, `vulkan/rhi/vulkan_color_converter.rs`,
  `vulkan/rhi/shaders/color_convert_nv12_buffer_to_rgba.comp`,
  `color_convert_yuyv_buffer_to_rgba.comp` — and readback is an always-present
  `GpuContext` capability (`core/context/gpu_context.rs:2478`,
  `create_texture_readback`; plan §Graphics, `ARCHITECTURE.md:416-421`). No pixel
  conversion happens outside the RHI and no second converter is built.

- **DECIDED** — The operation reaches the engine through `RuntimeOperations` and nothing
  else. The api-server is a processor whose HTTP task deliberately holds only
  `Arc<dyn RuntimeOperations>` (`runtime/streamlib-api-server/processors/api_server.rs:167-171`);
  the trait gains one operation (`exchange_surface_id_for_image_bytes_async`), `Runner`
  implements it (`core/runtime/operations_runtime.rs:336`) over the shipped doors —
  `SurfaceStore::check_out` (`core/context/surface_store.rs:510`) for a pooled
  pixel-buffer backing, the host-visible export staging
  (`core/context/surface_export_staging.rs:425`) for a texture backing, the same doors
  the cast object's `cpu()` rides (`ARCHITECTURE.md:365-394`). No new surface-resolution
  path exists, and the caller needs no Vulkan device, no surface socket, and no runtime
  link.

- **DECIDED** — One operation, every surface: MCP tool and REST route serve the same
  `exchange` with the same arguments and result shape; the CLI verb is a pure JSON-RPC
  client of it, per the vocabulary rule. The REST spelling joins the bearer-gated set
  beside the tap WebSocket — same mechanism-parity reasoning the tap gate states
  (`auth.rs:18-19`) — and MCP inherits the gate the whole dispatch already has. What the
  auth OPEN entry decides later, it decides for this verb the same as the rest.

- **DECIDED** — The observer effect is the problem being removed, and its absence is the
  proof. Reading a channel no longer requires terminating it in a window, so a mid-graph
  channel is observable in the topology that ships. Window capture survives only where
  the window is genuinely the subject — the present and swapchain path.

## MODIFIED: §Control plane & observability — the vocabulary sentence

The entry at `ARCHITECTURE.md:643-654` pins the vocabulary as "graph, tap, logs, health,
nodes". Two edits, one factual and one decided here:

- Factual: the served MCP tool set is `graph`, `tap`, `logs`, `shutdown` — asserted
  equal, not superset (`runtime/streamlib-api-server/src/mcp.rs:599`); `health` and
  `nodes` are REST and registry surfaces, not tools. The sentence is corrected to match.
- **DECIDED** — `exchange` joins the vocabulary as an observation verb. The pivot's rule
  is unchanged in substance: the control plane never mutates the graph — submit /
  replace / connect / remove stay gone, code stays the source of truth, the edit loop
  stays `dev`. A read that costs the node a bounded copy is still a read.

The CLI sentence (`ARCHITECTURE.md:664-669`) adds `exchange` to the observation verbs:
`nodes` / `graph` / `tap` / `logs` / `exchange`. The verb takes a surface id, or a
channel: the channel form composes tap → decode → exchange **client-side in one warm
process** — one connection, the exchange fired the moment the bag lands, `--count` and
every-Nth sampling as client flags. It is the cold-spawn latency fix and the throttling
surface in one, and it adds nothing to the engine: the CLI stays a pure JSON-RPC client
composing the same two operations any consumer composes.

---

## ADDED: §Control plane & observability — what the exchange hands back

Resolved by the owner, 2026-08-25 (was this change's one `[NEEDS DECISION]`).

- **DECIDED** — Two spellings of one operation. REST serves the exact frame as a binary
  `image/png` response body: lossless, full resolution, no base64 inflation,
  remote-capable — the evidence and PSNR path, and what the CLI writes into a
  caller-named directory. The MCP tool returns an image content block, downscaled by
  default to a declared cap (~1568 px long edge, the resolution ceiling vision models
  actually use) with the result stating the true extent and the exact-bytes route — the
  agent's in-session view, remote included, always under the per-image payload ceiling.
  A PNG encoder enters library code (`png` is a dev-dependency today, fixture decoding
  only — `runtime/streamlib-engine/Cargo.toml:180`); the downscale rides the RHI's
  existing blit path, never a second scaler. Raw unconverted planes stay deferred until
  something needs them. This replaces the window-capture shape outright: no screenshot
  tool, no window, no display server anywhere in the read path.

---

## REMOVED:

- REMOVED: STREAMLIB_DISPLAY_PNG_SAMPLE_DIR
  Retired with the in-process display sampler; the engine honours it nowhere. Live text
  in `.claude/agents/evidence-verifier.md:21`, `.claude/skills/verify-live/SKILL.md:28`,
  and as a dead export in the three PSNR fixture scripts.
- REMOVED: STREAMLIB_DISPLAY_PNG_SAMPLE_EVERY
  Same retirement. Live text in `.claude/agents/evidence-verifier.md:21`,
  `.claude/skills/verify-live/SKILL.md:29,84`, and the same three fixture scripts.
- REMOVED: STREAMLIB_DISPLAY_FRAME_LIMIT
  Same retirement. Live text in `.claude/agents/evidence-verifier.md:21`,
  `.claude/skills/verify-live/SKILL.md:30,42,64`, and the same three fixture scripts.

**Blast radius, proved by running the gate's own sweep rather than assumed.**
`runtime/streamlib-engine/tests/fixtures/**` is inside the content sweep —
`.claude/scripts/ship-change-removed-gate.sh:49-56` excludes only `vendor`, `docs/plan`,
`docs/decisions`, `docs/learnings`, `examples`, `CHANGELOG.md` and the consumer entries
under `packages/`. So these bullets require the dead exports to come out of
`e2e_fixture_psnr.sh:154-156`, `e2e_fixture_psnr_jpeg.sh:182-184` and
`e2e_fixture_psnr_vivid.sh:152-154`. That is a mechanical scrub of variables nothing
reads — no assertion changes, none of the codec work they are blocked on is touched. It
must land with this change or the ship gate cannot go green.

**Bullets deliberately not written, each because the sweep proved them wrong.**
`camera-display` matches live engine comments at `vulkan/rhi/vulkan_texture.rs:2077` and
`vulkan/rhi/vulkan_swapchain_dma_buf_allocation_fix_validation_test.rs:52`, plus the ship
gate's own self-test fixture at
`.claude/scripts/tests/ship-change-removed-gate.test.sh:194`. `xdotool` is genuinely
alive in `runtime/streamlib-media-builtins/tests/two_display_windows_live.rs:45` and
`sdk/streamlib-python-wheel/tests/test_processor_owned_window.py:270,290` — window-gesture
tests where the window is the subject. Both stay grep checks in the validation below
rather than gate bullets.

---

## Not in scope

- **The three PSNR fixtures stay deferred** (owner call, #1966). They build
  `examples/vulkan-video-psnr` and `examples/jpeg-psnr`, pre-pivot consumer crates, and
  converting them needs codec blocks in Python — a named engine gap. This change scrubs
  their dead env-var exports and touches nothing else in them. They become
  straightforward once this door exists, because exact pixels are what PSNR needs.
- **The `.claude/` harness half is its own PR**, per `.claude/rules/flow.md`: the
  verify-live skill text, `evidence-verifier.md`, the two `agent-knowledge` index files,
  and the `rig-brake.sh` branch keying on `cargo run -p camera-display`, a crate that no
  longer compiles. A session never edits the skills it is itself running — why this was
  split out of PR #1967. Sequenced after the capability lands, so the skill documents a
  door that exists.
- **Zero-copy per-frame consumption by a foreign GPU stack** stays OPEN
  (`ARCHITECTURE.md:124-128`) and is untouched.

## Validation

- A live run that taps a channel with **no `DisplayWindow` anywhere in the graph**,
  decodes a bag, exchanges its surface id, and gets pixels. The absent window is the
  assertion — the proof the observer effect is gone; no log line stands in for it.
- A headless run — no `$DISPLAY`, no window server — returning the same pixels.
- A retired id refused by name at the exchange, loud, never resolving to newer pixels.
- The claim released before the operation returns, asserted against the pool's own
  accounting (`core/context/surface_check_out_lease_registry.rs:247,289`), so exchanging
  N frames in sequence never pins N slots.
- Tap's tool result is byte-identical to today under this change — no new argument, no
  new field, nothing removed.
- Mechanical: zero hits for the three env vars and for `-p camera-display` under
  `.claude/`, and no `xdotool` or ImageMagick invocation on the pixel-read path.
- Gates are held to the bar the failure being repaired sets: the retired camera-display
  fixture asserted on three tracing strings that exist nowhere in the engine — it would
  have reported FAIL on a healthy run, and nobody noticed. No new gate asserts on our own
  `tracing` prose.
