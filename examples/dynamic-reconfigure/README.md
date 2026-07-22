# dynamic-reconfigure

Live camera→display graph rewiring. Starts `@tatolab/camera` → `@tatolab/display`,
then splices a `LiveVideoFrameForwarder` processor into and out of the middle of the
**running** graph N times, then auto-exits — no restart between cycles.

This is the manual, visual counterpart to the headless regression test at
`runtime/streamlib-engine/tests/dynamic_reconfigure_live_splice.rs`. That test locks
live `add_processor` / `remove_processor` against a `start()`ed runtime **only**,
without a window. The live `connect` / `disconnect` rewire this example performs
(camera → forwarder → display and back) is **not** covered headless — it is
verified visually here and via `/verify-live`.

## The model this example teaches

Reconfiguration is just the ordinary `add_processor` / `connect` / `disconnect` /
`remove_processor` calls — issued against a runtime that is **already running**.
The full splice-in step is:

```rust
runtime.disconnect(&direct_link)?;                       // camera → display, gone
let forwarder = runtime.add_processor(/* LiveVideoFrameForwarder */)?;
runtime.connect(camera.video,  forwarder.input)?;        // camera → forwarder
runtime.connect(forwarder.output, display.video)?;       // forwarder → display
```

and the splice-out step removes the forwarder and reconnects `camera → display`.
Every call lands on the started runtime; the compiler wires/unwires the links and
constructs/destroys the processor live.

## What you see

Live camera video **keeps flowing** the whole time. While the forwarder is spliced
in, frames route camera → forwarder → display, and the forwarder — a `reactive`
inline pass-through — forwards every frame unchanged, so the display keeps
delivering live video (no freeze). When it is spliced back out, `camera → display`
is restored directly. Video stays live→live across each reroute; the visible proof
the splice took effect is that playback never stalls even as the middle of the
running graph is rewired.

## Run it

```bash
./setup.sh        # one-time: link the SDK + the packages this app uses
cargo run
```

`./setup.sh` links the SDK (`streamlib link --engine <checkout>`) and symlinks
`@tatolab/camera`, `@tatolab/display`, and `@tatolab/debug-utilities` (the
forwarder) into `./streamlib_modules/`.

## Visual audit (headless / `/verify-live`)

Set the display's PNG sampling env before running so pre/mid/post reconfigure
frames land on disk without a window:

```bash
STREAMLIB_DISPLAY_PNG_SAMPLE_DIR=/tmp/reconfigure-frames \
STREAMLIB_DISPLAY_PNG_SAMPLE_EVERY=15 \
cargo run
```

## Tunables (all optional)

| env var | default | meaning |
|---|---|---|
| `STREAMLIB_RECONFIGURE_CYCLES` | `3` | splice the passthrough in/out this many times |
| `STREAMLIB_RECONFIGURE_DWELL_MS` | `2500` | monotonic dwell per phase (direct / spliced) |
| `STREAMLIB_CAMERA_DEVICE` | camera default | camera device id (e.g. a `/dev/videoN`) |
| `STREAMLIB_DISPLAY_PNG_SAMPLE_DIR` | unset | when set, the display samples frames to PNG here |
| `STREAMLIB_DISPLAY_PNG_SAMPLE_EVERY` | `30` | sample every Nth frame |
