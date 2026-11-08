# tatolab-browser

A web page as a stream source, built the way a third party builds a node: a
standalone maturin wheel that depends on `tatolab-stream`, `pyo3` and
`tatolab-module-sdk`, and on no engine crate.

- `BrowserSource` renders `url` offscreen with Chromium, through CEF, as a tab
  in the machine's browser host: one CEF the runtime starts at the first
  `BrowserSource`'s lease and stops 30 s after the last, shared by every stream
  on the machine, each stream with a CEF profile (cookies, storage) of its own.
  The node's helper runs no CEF. On `start()` it lends its ports to this
  wheel's Rust, which leases the host through the module host table (1.7),
  opens the tab over the versioned tab protocol (`tab-protocol/`), and writes
  each frame the host paints, handed over in shared memory, into a texture
  from the node's output pool (`surface_write_from_host_memory`), published as
  an RGBA Image frame in sRGB. Python never runs at frame or sample cadence.
- `data` input: each Records map reaches the page as
  `window.dispatchEvent(new CustomEvent("tatolab:data", { detail }))`.
- `events` output: each `window.tatolab.publish(object)` call is one Records map.
- `audio` output: what the page plays, as planar `f32` Channels blocks at
  `audio_sample_rate`. A silent page publishes nothing.
- A page's renderer that crashes is reloaded by the host. When the host
  itself ends, the runtime starts it again under its restart backoff and each
  open page opens again in the new host. The node fails, shown in error in
  the graph while the rest of the stream runs on, when the renderer crashes
  more than 5 times in a minute, when the runtime gives up on the host, or
  when a running host refuses to open the page again for a minute.
- `BrowserSourceOutputProbe` logs a source's frame rate, its audio's
  continuity and how long a `data` message takes to reach the page's paint.

Config: `url`, `width`, `height`, `frame_rate` (1 to 60), `transparent_background`,
`audio_sample_rate`, `output_pool_rotation_depth`. A `url` with no scheme is a page
file inside the project, relative to the project directory, and opens as its
`file://` URL.

Linux only for now. Frames cross the CPU once per paint; zero-copy is next.

## Build it

The wheel itself carries no CEF. The browser host, its CEF subprocess and the
CEF runtime ship in the runtime unit, under `lib/tatolab/browser-host/`: on
Linux side by side, on macOS as `tatolab-browser-host.app`, with CEF's
framework and the subprocess as the helper apps CEF launches inside it.
`python3 -I scripts/fetch_cef_binary_distribution.py <directory>` lays out the
distribution for this machine, licence included, for `CEF_PATH`.
Building them needs CEF's binary distribution, the one `cef-dll-sys` links
against. `export-cef-dir`, which fetches it for `cef-dll-sys`, leaves out the
distribution's `LICENSE.txt`; copy it into `$CEF_PATH` from the official
`cef_binary_*_linux64_minimal` archive:

```bash
export CEF_PATH=/path/to/cef   # holding libcef.so (or the macOS framework) and LICENSE.txt
cargo xtask build-runtime      # stages the browser host when CEF_PATH is set
uvx maturin@1.9.6 build --release   # in packages/tatolab-browser
```

`cargo xtask stage-browser-host` stages the host alone into the unit.

## Run it

With this machine's runtime running:

```bash
cd streams/browser-source
tatolab dev
```

A file page loads classic scripts only: Chromium refuses module scripts
from a file URL, so a library shipped only as ES modules (three.js r160 and
later) needs a classic build, inlined modules, or the page served over http.

The probe page animates every frame, plays a 440 Hz tone and answers each
`data` message from its next animation frame. Watch the browser's `video` in
Tato Studio's viewer; a
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
