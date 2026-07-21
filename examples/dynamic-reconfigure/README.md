# dynamic-reconfigure

Live camera→display graph rewiring. Starts `@tatolab/camera` → `@tatolab/display`,
then splices a `SimplePassthrough` processor into and out of the middle of the
**running** graph N times, then auto-exits — no restart between cycles.

This is the manual, visual counterpart to the headless regression test at
`runtime/streamlib-engine/tests/dynamic_reconfigure_live_splice.rs`, which locks
the same behavior (live `add_processor` / `remove_processor` against a `start()`ed
runtime) without a window.

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

`SimplePassthrough` is a `manual` one-shot fixture — it forwards the single frame
present when it starts, not a continuous effect. So while it is spliced in, the
display **holds** that frame; when it is spliced back out, live camera video
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
