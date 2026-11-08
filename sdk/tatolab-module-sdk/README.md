# tatolab-module-sdk

The module host table a Tatolab module's native code reaches its node's ports
through, and safe Rust over it. It depends on no engine crate: a module builds
against this crate alone, and the runtime that starts its node lends the table.

## What the table carries

Version 1.0 carries, for the ports a node lends to its native code:

- open, write and state the stream facts of an output port;
- open, read and wait on input ports;
- the node's stop flag;
- log records into the node's log, at the host's levels;
- the runtime's monotonic clock;
- the host's capabilities, as JSON.

Version 1.1 adds the engine's audio device backend, so a module captures from
PipeWire, ALSA or CoreAudio without carrying an audio path of its own:

- open a capture stream on a named device or the default, optionally requiring
  a sample rate, channel count or sample format (a device that opens in another
  is refused, never converted);
- start and stop delivering blocks to a callback on the backend's device thread;
- read whether the device still serves the stream, and close it.

The stream opens under the stream's capture-prompting policy, so only a stream
that holds the node can put the platform's access prompt in front of the user,
and only when its load allows.

Version 1.2 adds surfaces, so a module hands the GPU what a device wrote
without a copy:

- import a linear DMA-BUF fd with its plane layout, as written by a foreign
  device;
- acquire a texture from one of the node's output pools;
- convert a YCbCr surface into an RGBA texture with the engine's colour
  converter;
- read the surface id a message names a surface by, and release it.

Version 1.3 adds playback through the same backend:

- open a playback stream on a named device or the default in the format the
  module plays (a rate, channel count or sample format left out takes the
  device's own); the backend converts it to the device's, and a format it
  cannot open the stream in is refused by name;
- read the format it opened in and the device's period, where the backend
  reports one;
- start and stop asking a callback on the backend's device thread to fill each
  device buffer;
- read whether the device still serves the stream, and close it.
Version 1.4 adds the macOS twin of the DMA-BUF import: an `IOSurfaceRef`, such
as a camera's or a decoder's, adopted as a surface written by a foreign device,
which the colour conversion reads where it sits.
Version 1.5 lends native code the surface a message names by id, such as an
upstream node's frame, and on macOS converts an RGBA texture into an adopted
NV12 IOSurface, the direction a VideoToolbox encoder feeds from.

Version 1.5 adds a write from host memory, for a renderer that paints into CPU
memory and cannot share a GPU buffer: `bgra32` or `rgba32` rows at any stride
replace every pixel of an output-pool texture of the same extent, with the
channel order swapped where the texture's differs. It costs one CPU copy into
the engine's host-visible staging and one GPU copy into the texture, and is
served on Linux.

Every verb is callable from any thread with no Python attached. The table carries
plain data, opaque handles and borrowed fds only. No engine or GPU object
crosses it, and
`cargo xtask check-module-host-table-surface` keeps it that way.

## Reaching it

A node's Python half lends ports in `start()` and passes the capsule it gets
back to its native half:

```python
module_host_table = ctx.open_module_host_table(
    output_ports=["encoded_video", "encoded_audio"]
)
self._thread = _native.MyReadingThread(module_host_table)
```

Once a port is lent, a write to it from Python is refused, naming the port. To
lend an input port, the node must be declared `execution="manual"`, so that the
runtime does not also drive `process()` from it.

The native half reads the capsule with the `pyo3` feature, then hands the
`ModuleHost` to whichever thread does the work:

```rust
let module_host = ModuleHost::from_python_module_host_table_capsule(&module_host_table)?;
let encoded_video = module_host.open_output_port("encoded_video")?;
std::thread::spawn(move || {
    while !module_host.stop_requested() {
        if encoded_video.write(&payload, None, true).is_err() {
            break;
        }
    }
});
```

A capture stream hands each block to a closure; the closure must not block,
so a module copies the block into its own queue and publishes from a thread:

```rust
let mut capture_stream =
    module_host.open_audio_capture_stream(&ModuleHostAudioCaptureStreamRequest::default())?;
capture_stream.start_delivering_to(Box::new(move |captured_block| {
    queue.push(captured_block.interleaved_sample_bytes.to_vec());
}))?;
```

A playback stream asks a closure for each device buffer; the closure must
not block either, so a module fills it from a queue a thread of its own keeps
topped up, and writes silence for whatever the queue does not hold:

```rust
let mut playback_stream = module_host.open_audio_playback_stream(
    &ModuleHostAudioPlaybackStreamRequest {
        sample_rate: Some(48_000),
        channels: Some(2),
        sample_format: Some(ModuleHostAudioSampleFormat::F32),
        ..ModuleHostAudioPlaybackStreamRequest::default()
    },
)?;
playback_stream.start_requesting_from(Box::new(move |requested_block| {
    queue.fill_or_write_silence(requested_block.interleaved_sample_bytes_to_fill);
}))?;
```

With the default `tracing` feature,
`module_host_tracing::forward_this_modules_tracing_records_to(&module_host)`
routes the module's own `tracing` records into the node's log.

## A function node in Rust

With the `function-node` feature, `#[function_node]` writes a function node in
Rust, for code outside Numba's subset that still needs native speed. Its grammar
is `@node.function`'s: each plain parameter is an input pin, a `Time` parameter
takes the reader's stream time, a `State<Record>` one the state its last call
returned, and a parameter named `config` the node's config record. A value is an
`f64`, `i64` or `bool`; a record is a struct of them deriving
`FunctionNodeRecord`, whose `Default` is its first state or its config's defaults.

```rust
use tatolab_module_sdk::function_node::{function_node, FunctionNodeRecord, State, Stateful};

/// `x` scaled by `gain`, then moved by `offset`.
#[function_node]
fn scale_and_offset(x: f64, #[pin_default(1.0)] gain: f64, #[pin_default(0.0)] offset: f64) -> f64 {
    x * gain + offset
}

#[derive(Clone, Copy, Default, FunctionNodeRecord)]
struct RunningTotal {
    total: f64,
}

/// `x` added into a running total.
#[function_node]
fn accumulate(x: f64, previous: State<RunningTotal>) -> Stateful<f64, RunningTotal> {
    let total = previous.total + x;
    (total, RunningTotal { total })
}
```

Each `#[function_node]` function gains a `<function>_function_node_declaration()`.
The native half hands its Python half that declaration with the `pyo3` feature,
and the Python half declares the node, whose type is its import path in the
module's `tatolab.nodes` entry points:

```rust
#[pymodule]
fn _native(module: &Bound<'_, PyModule>) -> PyResult<()> {
    let declaration = scale_and_offset_function_node_declaration();
    module.add("ScaleAndOffset", declaration.to_python_declaration(module.py())?)
}
```

```python
from tatolab.stream import node
from my_module import _native

ScaleAndOffset = node.rust_function(_native.ScaleAndOffset)
```

The node runs inside the read of each node it feeds, in that node's helper, never
in `tatolabd`. A panic refuses the call. The body is held to a function node's
contract (no I/O, no clock but `Time`, no state but `State`) by review.

## From C

`include/tatolab_module_host_table.h` declares the same table for a native half
written in C or C++. It is generated from `src/module_host_table_abi.rs` by
`cargo xtask generate-module-host-table-c-header`, and CI fails when the two
differ.

## Versioning

The table is append-only. A module built against minor N loads on any runtime
that lends minor N or later. Loading on an older minor, or on a different
major, is refused with an error that names both versions. This crate's
`major.minor` is the table's own; a patch release never changes the table.
