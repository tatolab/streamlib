# virtual-camera-pipewire-door

> **Asked for by the owner, 2026-11-04.** The owner picked "Loopback door now, PipeWire later" on
> `media-built-ins-become-modules` and asked that the second door not be left undone ("I worry
> if that means it will never get down"). Issue #3124. Where the door lives is the owner's call,
> in [NEEDS DECISION] D1 below.

`tatolab.virtualcamera:VirtualCameraSink` has one door, v4l2loopback. A machine without the
v4l2loopback module loaded or permitted gets no virtual camera at all, and `door = "pipewire"` is
refused by name. Before the media built-ins left the runtime, the Rust sink had a second door: a
PipeWire `Video/Source` node that portal-based readers (browsers, OBS through the camera portal)
select directly, with no kernel module and no permission grant.

**After this change:**
- **`door = "pipewire"` publishes a PipeWire camera.** The node registers a `Video/Source` node
  named `name` at `setup()` and removes it at `teardown()`. A reader that opens it gets the
  graph's frames. No kernel module, no `enable-virtual-camera` grant.
- **`auto` falls back to PipeWire.** `auto` takes v4l2loopback where its control node is
  writable, PipeWire otherwise, and refuses naming both doors only when neither answers.
- **One PipeWire loader in the process.** The camera reaches libpipewire through the engine's
  one dlopen chain, the audio arm's, exported to the module through the module host table, the
  way `tatolab-microphone` and `tatolab-speaker` reach the audio device.
- **What a stream author sees:** the `door` key takes all three values again. No port, no other
  key, no Protocol changes.

**Scale gate — this skill, no ADR.** It adds host table verbs (an append-only minor bump, the
contract the host table already states) and turns `tatolab-virtualcamera` from a pure-Python
wheel into an extension wheel. It touches no RHI type, no IPC wire format and no processor-model
rule.

**Precondition.** §Media I/O and §Packages are DECIDED and SHIPPED. This change flips the
virtual camera entry in §Media I/O to IN-FLIGHT.

## Current state (recon, `main` `1ba4f7f`)

- The module: `packages/tatolab-virtualcamera/python/tatolab/virtualcamera/__init__.py`. Pure
  Python. `loopback_door_refusal_or_none` refuses `door == "pipewire"` by name. The per-frame
  path is `acquire_pixel_buffer(w, h, "yuyv")`, `convert_surface_into`, then
  `copy_rows_between` into the loopback device's mmap buffer.
- The engine still carries the Rust sink's PipeWire door, with no caller:
  `runtime/streamlib-engine/src/linux/pipewire_video_source.rs` (`PipeWireCameraNode`, 973
  lines), its C shim `pipewire_video_source_shim.c`/`.h`, `GpuContextFullAccess::open_pipewire_camera_node`
  (`core/context/gpu_context.rs:5022`), and `host_rhi.rs:57`'s re-export.
- `PipeWireCameraNode` has two buffer arms. DMA-BUF: it allocates exportable render targets
  through `GpuContextFullAccess` (`acquire_render_target_dma_buf_image`, line ~384) and
  composes each frame into one with `VulkanOffscreenCompositor`. Shared memory: the shim's
  `MemFd` slots, which the GPU writes through `HostMappingWrittenByGpu`
  (`vulkan_host_mapping_imported_as_buffer.rs`, reached by `import_host_mapping_for_gpu_writes`
  at `gpu_context.rs:2862` and `:5385`). The shim offers `SPA_VIDEO_FORMAT_RGBA`
  (`pipewire_video_source_shim.c:38`).
- A module's helper process holds no `GpuContextFullAccess`: its GPU work escalates to the
  parent through the pixel exchange (`sdk/streamlib-python-wheel/src/python_helper_process_pixel_exchange/`).
  So neither arm runs in a helper as it stands.
- The host table is at minor 6 (`sdk/tatolab-module-sdk/src/module_host_table_abi.rs:28`). Its
  audio playback verbs (`audio_playback_stream_open/start/stop/liveness/close`) are the shape
  for an exported device: a request struct, an opened-stream out struct, a plain handle,
  implemented in `sdk/streamlib-python-wheel/src/python_module_host_table/audio_playback_streams.rs`
  over the engine's `AudioDeviceBackend`.
- The PipeWire loader: `linux/pipewire_entry_points.c` plus `PipeWireLibraryEntryPoints::loaded_once_per_process`,
  shared by the audio arm and the camera shim; `build.rs:93`'s `compile_pipewire_shims` compiles
  all three shims.

## Decisions

### [NEEDS DECISION] D1 — where the PipeWire camera lives

- **A (recommended) — in the node's own process, over shared memory.** The engine's camera
  door keeps only its shared-memory arm, retargeted to `SPA_VIDEO_FORMAT_YUY2`, and is exported
  through the host table as a virtual camera output stream. The module gains a native half that
  opens it and copies each frame's YUYV rows into the slot PipeWire hands it: the same one CPU
  copy the loopback door makes, from the same `convert_surface_into` output. The DMA-BUF arm,
  the compositor use and `HostMappingWrittenByGpu` are deleted. Matches §Media I/O's "a device
  is opened by the node that uses it, in its own process" and the audio device's precedent.
- **B — held by the runtime, zero-copy.** `tatolabd` keeps `PipeWireCameraNode` whole; the
  helper escalates "open a camera" once and "present this surface" per frame. Keeps the DMA-BUF
  arm, but puts a per-node device in the app process, against the same rule, and adds a
  per-frame escalation hop.

The rest of this proposal is written for A.

### D2 — the module becomes an extension wheel

The native half is a standalone maturin project under `packages/tatolab-virtualcamera`, built
against `tatolab-module-sdk` like `tatolab-speaker`, Linux only (`extension_builds_for_macos:
false`; off Linux the node still refuses at `setup()` naming the platform). Its loopback door
stays in Python, unchanged. Recommended because the host table is reached only from native code.

## ADDED

- **Host table minor 7 — the virtual camera output stream.** Appended after
  `surface_lend_by_surface_id`:
  `virtual_camera_stream_open(node_host, request, opened, error) -> handle`,
  `virtual_camera_stream_offer_extent(node_host, handle, width, height, error)`,
  `virtual_camera_stream_write_yuyv_frame(node_host, handle, rows, stride_bytes, timestamp_ns, presented, error)`,
  `virtual_camera_stream_liveness(node_host, handle, error)`,
  `virtual_camera_stream_close(node_host, handle)`.
  `ModuleHostTableVirtualCameraStreamRequest { camera_name: ModuleHostTableUtf8, reserved }`;
  `presented` reports published, no consumer watching, or every buffer held by the consumer.
  Layout regression tests beside the audio ones; the generated C header regenerates.
  `host_capabilities_json` names `virtual_camera_output` where the PipeWire camera door
  answers. Implemented in `python_module_host_table/virtual_camera_streams.rs`.
- **`tatolab-module-sdk`'s safe wrapper** `ModuleHostVirtualCameraStream`, beside
  `module_host_audio_playback.rs`.
- **The module's native half** — `tatolab.virtualcamera._native.PipeWireCameraDoor(camera_name)`
  with `offer_extent(width, height)`, `write_yuyv_frame(rows, stride_bytes, timestamp_ns) ->
  str` and `close()`, each a host table call, and its `.pyi`.
- **`door = "pipewire"` and `auto`'s fallback** in `VirtualCameraSink.setup()`. A PipeWire
  door that will not open is refused naming why (no libpipewire, no daemon answering). `auto`
  with neither door refuses naming both reasons and the verb.
- **Tests.** Host table layout tests; the wheel's verbs against the null-consumer case (no
  daemon in CI: the open refuses by name, which the suite asserts); the module's door choice
  in its GPU-free tests; the suite's existing PipeWire-door test turned from "refused by name"
  into "opens where a daemon answers, else refuses naming the daemon".

## MODIFIED

- **§Media I/O, the virtual camera entry.** "The one door is v4l2loopback … `door = "pipewire"`
  is refused" becomes two doors: v4l2loopback as written, and a PipeWire `Video/Source` node
  over shared memory, YUY2, four buffers, the frame's monotonic timestamp on each, reached
  through the host table's virtual camera output stream. `auto` prefers v4l2loopback.
- **§Packages, the host table entry.** "host capabilities, among them the audio device
  interface" gains the virtual camera output stream.
- **`pipewire_video_source.rs`** keeps its loader, its name and property plumbing and its
  shared-memory slot cycle, takes the frame as YUYV host rows instead of a texture, and no
  longer needs a GPU context. The shim offers YUY2 and `MemFd` only.
- **The release workflow.** `build-tatolab-virtualcamera-distribution` becomes a
  `reusable-extension-wheel.yml` call, manylinux only.

## REMOVED

- REMOVED: PipeWireCameraBufferKind
  Only one buffer kind remains, so the presentation result carries no kind.
- REMOVED: open_pipewire_camera_node
- REMOVED: HostMappingWrittenByGpu
  Its one producer was the shared-memory arm's GPU write; the frame now arrives as host rows.
- REMOVED: import_host_mapping_for_gpu_writes
- REMOVED: runtime/streamlib-engine/src/vulkan/rhi/vulkan_host_mapping_imported_as_buffer.rs
- REMOVED: no PipeWire camera door
  The module's refusal text for `door = "pipewire"`.
