# Separately-built `.slpkg` GPU plugins must not hand-roll RHI on the raw `HostVulkanDevice`

## Symptom

A GPU streamlib package runs **end-to-end clean in-process / as a
workspace plugin**, but when loaded as a **separately-built registry
`.slpkg`** crashes the GPU driver during resource construction. On
NVIDIA Linux the crash is a heap double-free deep inside the driver's
shader compiler:

```
... -> VulkanComputeKernel::new -> create_pipeline_layout
    -> vkCreatePipelineLayout
    -> "double free or corruption (!prev)"  in libnvidia-gpucomp.so   (SIGABRT)
```

No Rust panic, no validation error — a native abort inside the driver.
The identical code path (build the same compute kernel on the same GPU
+ driver) passes when run **in-process** (e.g. a GPU integration test
in the package's own crate). Aligning the plugin's `streamlib-engine`
version to the host's does **not** fix it — the crash reproduces
identically at a version split and at a matched version.

## Trigger condition

A package's GPU code obtains the host device via
`GpuContextFullAccess::host_vulkan_device_arc()` and then builds RHI
objects **locally inside the plugin** off that device —
`VulkanComputeKernel::new(device, …)`, `HostVulkanBuffer::new_*(device, …)`,
`RhiCommandRecorder::new(device, …)`, etc. — AND the package is
distributed as a source-only `.slpkg` built at load time by the build
orchestrator (a *separate* cargo invocation), rather than as a
workspace plugin compiled in the host's own build.

## Root cause

`host_vulkan_device_arc()` transits an `Arc<HostVulkanDevice>` across
the plugin ABI by raw pointer (`Arc::into_raw` host-side,
`Arc::from_raw` plugin-side). `HostVulkanDevice` is **not
`#[repr(C)]`**, so its field layout is `repr(Rust)` — which the
compiler is free to choose and which is **not stable across separate
compilations**. The transit is sound only when the plugin's
`streamlib-engine` is byte-identical to the host's (same rustc, same
resolved dep graph, same features) — the "workspace plugin cdylib"
contract.

A separately-built registry `.slpkg` does not satisfy that. The
orchestrator resolves the plugin's deps independently, so even at the
**same `streamlib-engine` version** the binary can differ (different
transitive patch versions, different feature unification arising from a
different surrounding dep graph). When it does, the plugin reads the
host's `HostVulkanDevice` through a mismatched layout — every field
access lands at the wrong offset. The garbage flows into descriptor-set
/ pipeline-layout creation, and the driver corrupts its own heap
building the pipeline.

The deeper invariant: a non-`#[repr(C)]` type **cannot** safely cross a
plugin boundary by raw pointer between two independently-compiled
binaries, regardless of version. The fix is not to make the transit
work — it is to **not transit the device at all**.

## Fix

Build every GPU resource through the cdylib-safe `GpuContextFullAccess`
primitives instead of the raw device. These dispatch through the
`#[repr(C)]` FullAccess vtable: the **host** builds the resource on the
**host's** device and returns a `#[repr(C)]` PluginAbiObject handle
whose methods route through a per-type `methods_vtable` — immune to
host-layout drift.

| Don't (raw device, build-fragile) | Do (FullAccess primitive, plugin-safe) |
|---|---|
| `VulkanComputeKernel::new(device, desc)` | `full.create_compute_kernel(desc)` |
| `HostVulkanBuffer::new_storage_buffer_*(device, n)` | `full.acquire_storage_buffer(n)` |
| hand-rolled `Vec<Texture>` decode ring | `full.create_texture_ring(…)` |

The descriptor data is the same; only the construction site changes. A
package's GPU code should never name `HostVulkanDevice`,
`VulkanComputeKernel::new`, or `HostVulkanBuffer::new*` — those are
host-internal. If a primitive a package needs has no cdylib-safe
FullAccess form yet (some OPAQUE_FD / cross-API export paths may not),
that is an engine gap to close (add the primitive or escalate the op),
not a license to reach for the raw device.

## Why it hides until a registry `.slpkg`

In-process consumers and workspace-plugin cdylibs share the host's
exact compilation, so the transited `HostVulkanDevice` layout happens
to match and the raw-device path "works." The bug manifests only once
the plugin is a separately-built artifact whose layout can diverge —
which is precisely the cross-repo distribution the `.slpkg` model
targets. A GPU package that passes every in-repo test can still corrupt
in a customer's host. Treat "works in-process" as **no evidence** that
a GPU package is plugin-safe; the only real test is a separate-build
`.slpkg` run.

## Reference

- The sound primitives live on `GpuContextFullAccess` in
  `libs/streamlib-engine/src/core/context/gpu_context.rs`
  (`create_compute_kernel`, `acquire_storage_buffer`,
  `create_texture_ring`); the compute-kernel PluginAbiObject and its
  `methods_vtable` live in
  `libs/streamlib-engine/src/vulkan/rhi/vulkan_compute_kernel.rs`.
- The reference "done right" call is `create_texture_ring` — GPU
  packages that already use it for their output ring show the shape the
  kernel/buffer construction should follow.
- Sibling learning:
  [`cdylib-make-borrow-cached-fields.md`](cdylib-make-borrow-cached-fields.md)
  — the other end of the same non-`#[repr(C)]` transit hazard (zeroed
  cached fields on a reconstructed borrow).
- Architecture: [`../architecture/compute-kernel.md`](../architecture/compute-kernel.md)
  (the single compute abstraction),
  [`../architecture/subprocess-rhi-parity.md`](../architecture/subprocess-rhi-parity.md)
  (the escalate model for privileged GPU work across a process boundary),
  [`../architecture/texture-ring.md`](../architecture/texture-ring.md).
