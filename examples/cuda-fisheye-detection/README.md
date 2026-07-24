# cuda-fisheye-detection

A Python subprocess processor imports a host-allocated OPAQUE_FD `VkImage` as a
CUDA texture: the host warps a demo image with a pure-Rust fisheye (barrel)
distortion, uploads it into a DEVICE_LOCAL OPAQUE_FD `VkImage`, and registers it
via surface-share; the Python processor imports it through
`cudaExternalMemoryGetMappedMipmappedArray`, undistorts with a `cupy.RawKernel`
(hardware-bilinear sampling), runs YOLOv8n detection, and writes an annotated
PNG. The tiled-image (texture) half of streamlib's CUDA surface-adapter story,
sibling to `polyglot-cuda-inference`'s flat-tensor DLPack path.

## The model this example teaches

Like every streamlib app, the wiring is `add_processor` + `connect` + `start`
with **no module-loading call** and **no version at the reference site**:

```rust
let source = runtime.add_processor(ProcessorSpec::new(
    processor_type_ref!("tatolab", "debug-utilities", "BgraFileSource"),
    /* config */,
))?;
let undistort_ident =
    processor_type_ref!("tatolab", "cuda-fisheye-python", "CudaFisheyeUndistortion");
let undistort = runtime.add_processor(ProcessorSpec::new(undistort_ident, undistort_config))?;
```

The referenced packages live in this app's **`streamlib_modules/`** folder and
the runtime lazily discovers + loads each on first reference. This app uses two:
the in-repo `@tatolab/debug-utilities` trigger source, plus its own `./python`
package (`@tatolab/cuda-fisheye-python`).

(The host-side setup hook that pre-allocates the OPAQUE_FD `VkImage` + timeline,
uploads the warped pixels, and registers them with surface-share is separate
application wiring — it is the adapter integration this example exists to
demonstrate, not module loading.)

## Run it

```bash
./setup.sh                                  # one-time local link
cargo run -- --output=/tmp/cuda-fisheye-detected.png
```

`./setup.sh` does the full local setup in one shot:

1. **SDK** — `streamlib link --engine <checkout>` points the Rust and Python
   streamlib SDK surfaces at the in-repo checkout (crates.io patch + uv source).
   The linked checkout is the SDK package source; there is no central
   package registry.
2. **Packages** — `streamlib link` symlinks `@tatolab/debug-utilities` and this
   example's `./python` package into `./streamlib_modules/`.

The `streamlib` CLI must be on your `PATH` (`cargo build -p streamlib-cli`, or
`cargo install --path tools/streamlib-cli`); `setup.sh` falls back to the
checkout's built binary.

## What's committed vs generated

`streamlib_modules/`, `streamlib.lock`, `Cargo.lock`, and the
`streamlib link --engine` override are **not committed** — they are regenerated
by `./setup.sh`.
