# tatolab-browser

A web page as a stream source, built the way a third party builds a node: a
standalone maturin wheel that depends on `tatolab-stream`, `pyo3` and
`tatolab-module-sdk`, and on no engine crate.

- `BrowserSource` renders `url` offscreen with Chromium, through CEF, inside
  its own helper process. On `start()` it lends its ports to this wheel's Rust,
  which runs CEF's browser process on one thread of that helper. Each frame CEF
  paints is written from host memory into a texture from the node's output
  pool (`surface_write_from_host_memory`, module host table 1.4) and published
  as an RGBA `VideoFrame`. Python never runs at frame or sample cadence.
- `data` input: each msgpack map reaches the page as
  `window.dispatchEvent(new CustomEvent("tatolab:data", { detail }))`.
- `events` output: each `window.tatolab.publish(object)` call is one msgpack map.
- `audio` output: what the page plays, as the house audio block (interleaved
  `f32`, stereo, at `audio_sample_rate`). A silent page publishes nothing.
- CEF's child processes run `tatolab-browser-subprocess`, shipped in the wheel
  beside CEF, and die with the helper, so `kill -9` of the helper takes them
  with it.
- `BrowserSourceOutputProbe` logs a source's frame rate, its audio's
  continuity and how long a `data` message takes to reach the page's paint.

Config: `url`, `width`, `height`, `frame_rate` (1 to 60), `transparent_background`,
`audio_sample_rate`, `output_pool_rotation_depth`.

Linux only for now. Frames cross the CPU once per paint; zero-copy is next.

## Build it

The wheel ships the CEF runtime, so building it needs CEF's binary
distribution, the one `cef-dll-sys` links against. `export-cef-dir`, which
fetches it for `cef-dll-sys`, leaves out the distribution's `LICENSE.txt`; copy
it into `$CEF_PATH` from the official `cef_binary_*_linux64_minimal` archive,
because the wheel and its notices both reproduce it:

```bash
export CEF_PATH=/path/to/cef_binary_linux64   # the extracted distribution holding libcef.so
cargo build --release -p tatolab-browser-page-bridge --bin tatolab-browser-subprocess
python3 stage_cef_runtime.py      # copies CEF and the subprocess into python/tatolab/browser/cef/
uvx maturin@1.9.6 build --release
```

## Run it

With this machine's runtime running:

```bash
cd streams/browser-source
tatolab dev
```

The probe page animates every frame, plays a 440 Hz tone and answers each
`data` message from its next animation frame. Watch it at
`http://127.0.0.1:4446/watch/<machine>/main`; a
`BrowserSourceOutputProbe: ...` report lands in the project's log every 600
frames.

## Check it

Without CEF, `cef/dox` compiles the bindings without linking them:

```bash
cargo fmt --all --check
cargo test --locked --workspace --features cef/dox
cargo clippy --locked --workspace --all-targets --features cef/dox -- -D warnings
uvx pyright@1.1.411
```

`THIRD-PARTY-NOTICES.md` is regenerated, with CEF's licence read from
`$CEF_PATH`, by
`cargo xtask generate-third-party-notices --extension-package-directory packages/tatolab-browser`.
