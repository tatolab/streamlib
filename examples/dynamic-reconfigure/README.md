# dynamic-reconfigure

Live camera→display graph rewiring. Starts `@tatolab/camera` → `@tatolab/display`,
then splices a `SimplePassthrough` processor into and out of the middle of the
**running** graph N times, then auto-exits — no restart between cycles.

This is the manual, visual counterpart to the headless regression test at
`runtime/streamlib-engine/tests/dynamic_reconfigure_live_splice.rs`. That test locks
live `add_processor` / `remove_processor` against a `start()`ed runtime **only**,
without a window. The live `connect` / `disconnect` rewire this example performs
(camera → passthrough → display and back) is **not** covered headless — it is
verified visually here and via `/verify-live`.

## The model this example teaches

Reconfiguration is just the ordinary `add_processor` / `connect` / `disconnect` /
`remove_processor` calls — issued against a runtime that is **already running**.
The full splice-in step is:

```rust
runtime.disconnect(&direct_link)?;                       // camera → display, gone
let passthrough = runtime.add_processor(/* SimplePassthrough */)?;
runtime.connect(camera.video,  passthrough.input)?;      // camera → passthrough
runtime.connect(passthrough.output, display.video)?;     // passthrough → display
```

and the splice-out step removes the passthrough and reconnects `camera → display`.
Every call lands on the started runtime; the compiler wires/unwires the links and
constructs/destroys the processor live.

## What you see

While the passthrough is spliced in, live frames stop arriving at the display, so
the display **retains its last frame** — `SimplePassthrough` is a `manual` one-shot
fixture, not a continuous effect, and does not pump frames through on its own. When
it is spliced back out and `camera → display` is restored, live camera video
resumes. The **live → held → live** transition each cycle is the visible proof the
reroute took effect on the running graph.

## Run it

```bash
./setup.sh        # one-time: link the SDK + the packages this app uses
cargo run
```

`./setup.sh` links the SDK (`streamlib link --engine <checkout>`) and symlinks
`@tatolab/camera`, `@tatolab/display`, and `@tatolab/debug-utilities` (the
passthrough) into `./streamlib_modules/`.

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
