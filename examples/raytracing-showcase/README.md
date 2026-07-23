# raytracing-showcase

Orbits a single cube through the SDK's `VulkanRayTracingKernel` RHI and writes
each frame as a PNG. A pure engine/RHI demo — it uses no processor graph, so
there is no `add_processor`, no `processor_type_ref!`, and no processor package
to load.

The demo skips with a clear message when the device does not expose the
`VK_KHR_ray_tracing_pipeline` extension chain (needs an RTX-class or RDNA2+ GPU
with a recent driver).

## Run it

```bash
./setup.sh        # one-time: link the in-repo SDK
cargo run
```

PNG frames land in `$RT_SHOWCASE_OUT_DIR` (defaults to a `rt-showcase/`
directory under the system temp dir). Assemble them into an mp4 with the
`ffmpeg` command the program prints on completion.

`./setup.sh` runs `streamlib link --engine <checkout>` so the app's
`streamlib = "0.6"` dependency resolves against the in-repo SDK (a transient
`[patch.crates-io]`; there is no hosted registry — the linked checkout is the
SDK source). Because this
demo loads no processor packages, that is the only setup step. The `streamlib`
CLI must be on your `PATH` (build it with `cargo build -p streamlib-cli` from
the checkout); `setup.sh` falls back to the checkout's built binary otherwise.

`Cargo.lock` is not committed (it pins linked-checkout versions).
