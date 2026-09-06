# virtual-camera-sink

`VirtualCameraSink`, a native built-in beside `DisplayWindow`: video in, a v4l2loopback
capture device out, so any Linux application opens a StreamLib graph as a camera. Implements
§Media I/O's DECIDED entry (`docs/plan/ARCHITECTURE.md:814-833`) and the built-in
criterion's clause (c) (`:106-119`); the PipeWire door stays OPEN (`:1817-1823`).
Rationale and the rig facts are `docs/decisions/virtual-camera-sink.md` (owner, 2026-09-06).

**Scale gate — this skill, plus the ADR.** New behavior with a public marker class and a
stub entry, and one new RHI primitive (a host-pointer import) that only the RHI may own:
`cargo xtask check-boundaries` keeps `vulkanalia` out of `runtime/streamlib-media-builtins`
(`xtask/src/check_boundaries.rs:275-307`). The ADR exists; this change appends its RHI
section.

**Precondition.** The three entries above are DECIDED — merged as #2194 on 2026-09-06.
§Consumers `:363-365` says a showcase authored in the current idiom is an ordinary addition.
Sections flipped to `IN-FLIGHT (→ virtual-camera-sink)`: §Packages & extension model,
§Graphics (RHI / GPU), §Media I/O, §Consumers.

**Verified against the tree 2026-09-06 (HEAD bd28d57ea)** — three read-only recon sweeps.

- The media built-ins crate already depends on `v4l = "0.14"` and drops to raw
  `libc::ioctl` with the crate's `vidioc::*` constants where the safe API stops
  (`runtime/streamlib-media-builtins/Cargo.toml:31-34`; `camera_source.rs:173`, `:265`,
  `:503`, `:529`, `:662`, `:812-902`). The crate's default feature is pure bindgen over
  `videodev2.h` with no `links` and no `rustc-link-lib`; `libv4l2` is linked only under
  its opt-in feature. So the sink adds nothing to `DT_NEEDED`
  (`sdk/streamlib-python-wheel/tests/test_wheel_portability.py:25-37`, `:121-132`).
- The crate carries the output half: `v4l::io::traits::OutputStream` (`queue`, `dequeue`,
  `next`) over `v4l::io::mmap::Stream::with_buffers(&dev, Type::VideoOutput, n)`, whose
  arena exposes the mappings as `bufs: Vec<&mut [u8]>` — page-aligned host pointers. Two
  traps in it: `queue()` polls `POLLOUT` before `VIDIOC_QBUF`, and v4l2loopback's poll
  returns 0 between `REQBUFS` and `STREAMON`, so a hand-rolled queue before start hangs
  forever; and `Metadata::default()` sends `bytesused = 0`, which the driver warns on.
- v4l2loopback 0.15.3 from its source: `QUERYCAP.driver == "v4l2 loopback"`; `S_FMT` on
  the OUTPUT type must precede `REQBUFS` and returns `EBUSY` while buffers are allocated or
  another opener holds the format; `REQBUFS` accepts `V4L2_MEMORY_MMAP` only and clamps the
  count to the module's `max_buffers` (default 2); `QBUF` publishes synchronously and
  `DQBUF` never blocks — no back-pressure from readers, the writer paces; a non-zero
  supplied timestamp is copied through under `V4L2_BUF_FLAG_TIMESTAMP_COPY`. With
  `exclusive_caps=1` the capabilities are computed per opener: OUTPUT to the writer,
  CAPTURE-only to everyone else while the writer streams.
- Chromium's V4L2 enumerator lists a node only when it reports `VIDEO_CAPTURE` and not
  `VIDEO_OUTPUT` (`media/capture/video/linux/video_capture_device_factory_v4l2.cc:184-190`),
  so Chrome needs `exclusive_caps=1`; it asks for four buffers (`kNumVideoBuffers = 4`) and
  treats an `S_PARM` failure as fatal; YUYV is on its accepted list. OBS requires only
  `VIDEO_CAPTURE` and accepts either mode. While the sink holds the format, `ENUM_FMT`
  reports exactly the set format at index 0, so a reader sees one choice.
- The engine's own camera list filters on `VIDEO_CAPTURE` (`camera_source.rs:79-110`), so a
  loopback node the sink is streaming into would qualify for the camera source's
  first-device pick: a graph `camera → effect → virtual camera` could capture itself.
- The RHI has no host-pointer import. `VK_EXT_external_memory_host` appears only in the
  vendored bindings (`vendor/tatolab-vulkanalia/src/vk/extensions.rs:2952-2977`,
  `builders.rs:50066-50097`). The optional-extension pattern is probe → push → record a
  `bool` → accessor (`runtime/streamlib-engine/src/vulkan/rhi/vulkan_device.rs:809-822`,
  `:845-849`, `:1178-1180`, `:2788`); the properties-chain precedent for the alignment
  query is `:1069-1072`. The rig reports the extension with a 4 KiB alignment.
- A compute kernel binds a `VkBuffer` as a storage-buffer output today:
  `ComputeBindingSpec::storage_buffer` (`core/rhi/compute_kernel.rs:145`) maps to
  `STORAGE_BUFFER` (`vulkan/rhi/vulkan_compute_kernel.rs:773-784`), set through
  `set_storage_buffer_storage` (`:1013`). The mirror shader exists — YUYV buffer in, RGBA
  image out — at `vulkan/rhi/shaders/color_convert_yuyv_buffer_to_rgba.comp:24-28`, built
  by `vulkan_color_converter.rs:183-220` behind `RhiColorConverter`
  (`core/rhi/color_converter.rs:219-283`); no image-to-YUYV direction exists. `dispatch()`
  records no host-stage barrier (`vulkan_compute_kernel.rs:531-596`); `record()` on a
  caller-owned command buffer (`:600-614`) is how `RgbToNv12Converter` surrounds a kernel
  with its own barriers (`vulkan/video/rgb_to_nv12.rs:65-86`).
- The readback staging is `HOST_VISIBLE | HOST_COHERENT` with no `HOST_CACHED` preference
  (`vulkan/rhi/vulkan_texture_readback.rs:118-126`), and `acquire_storage_buffer` lands on
  a sequential-write, DMA-BUF-exportable allocation (`vulkan/rhi/vulkan_buffer.rs:168-200`,
  `:260-272`). The host-cached precedent is the decode path
  (`vulkan/video/decode/mod.rs:867-883`, `:911-921`: 37 ms median for one 1080p frame off
  write-combined memory) and `new_opaque_fd_export_host_cached` (`vulkan_buffer.rs:487-508`).
- Any published `VideoFrame` becomes a GPU image through
  `GpuContext::resolve_texture_registration_by_surface_id` (`core/context/gpu_context.rs:1317-1414`):
  same-process texture, cross-process DMA-BUF import, or the buffer-backed fallback that
  uploads a camera frame into a per-slot RGBA8 canvas (`:1461-1526`). The encoder built-in
  is the sink-shaped consumer of it
  (`runtime/streamlib-media-builtins/src/published_surface_to_encoded_frame_encoder.rs:223-233`).
- A built-in's registration and five wheel touchpoints: `register_media_builtin_processor_types`
  (`runtime/streamlib-media-builtins/src/lib.rs:107-127`); the constructor-less marker,
  the `is()` arm with the Linux-only refusal, `add_class`, the re-export and the stub
  (`sdk/streamlib-python-wheel/src/python_native_builtin_blocks.rs:86-90`, `:120-130`,
  `:193-197`; `src/lib.rs:42-53`; `python/streamlib/__init__.py:39-45`, `:84-110`;
  `python/streamlib/_engine.pyi:28-80`, `:250-292` as the docstring model). stubtest gates
  the stub with no allowlist (`.github/workflows/python-wheel.yml:155-157`).
- Tests reach CI by name only: the media-builtins entry at `.github/workflows/test.yml:129`
  mirrored at `xtask/src/main.rs:284-294`; the engine-lib slice at `test.yml:214` mirrored
  at `:300-313`. Hardware-bound wheel tests carry `requires_gpu`
  (`sdk/streamlib-python-wheel/pyproject.toml:61-67`) and run on the rig only.
- Colorimetry translation is capture-direction only (`v4l2_color.rs`, V4L2 enum →
  `ColorInfo`); the sink needs the inverse for `S_FMT`.

## ADDED

### §Media I/O — the sink

- `VirtualCameraSink` in `runtime/streamlib-media-builtins/src/virtual_camera_sink.rs`,
  Linux-gated in `lib.rs` beside `display_window` and registered in
  `register_media_builtin_processor_types`. Declared with the processor macro:
  `execution = reactive, scheduling = high, config = crate::virtual_camera_sink::VirtualCameraSinkConfig,
  input("video", delivery_profile = "newest", ...)`, no output. Reactive rather than a
  thread of its own: the loopback never applies back-pressure, so the frame's arrival is
  the pace, and `process()` blocks only on the GPU pass, the encoder's shape.
- `VirtualCameraSinkConfig { device: Option<String> }`, serde default. Unset: enumerate
  `/dev/video*`, open each, keep the first whose `QUERYCAP.driver` is `v4l2 loopback` and
  whose OUTPUT format token is free. Set: that path, checked the same way. The card label
  is the module's, read back from `QUERYCAP.card` for the log line.
- `setup()`: `S_FMT` on `V4L2_BUF_TYPE_VIDEO_OUTPUT` with YUYV at the first frame's extent
  is deferred to the first frame — the extent is not known at setup, the encoder's lazy
  mint (`:1496`). Setup opens the device, runs `QUERYCAP`, and refuses by name:
  - no loopback node found: `VirtualCameraSink: no v4l2loopback device is present. Load the
    module once — sudo modprobe v4l2loopback exclusive_caps=1 max_buffers=4
    card_label="StreamLib Virtual Camera" — and re-run; persist it in
    /etc/modules-load.d and /etc/modprobe.d to skip this next boot.`
  - the named device is not a loopback node: `... is driver "uvcvideo", not "v4l2 loopback"`.
  - another producer holds the device: `... is already fed by another producer (S_FMT
    returned EBUSY); one writer per loopback device.`
  Each raises at `setup()`; the processor never reaches Running; the runtime keeps running
  (`H264Decoder`'s shape, `_engine.pyi:133-136`). The engine never loads a module and never
  asks for elevation.
- First frame: `S_FMT` (YUYV, `bytesperline = 2·width`, `sizeimage = 2·width·height`,
  colorimetry from the frame's `ColorInfo` through the new inverse mapping), `S_PARM`
  from the frame's `fps` when the bag carries one, `REQBUFS(4, MMAP)` accepting the
  clamped count, `QUERYBUF` + `mmap`, `STREAMON` — in that order, before any queue, because
  of the poll trap above. Every mapping is handed to the RHI primitive below at this point,
  once. An extent change later runs `REQBUFS(0)` → `S_FMT` → `REQBUFS` → `STREAMON` again
  and re-imports; attached readers see the stream end and reopen, as they do for a camera
  re-plugged.
- Per frame: resolve the frame to a texture registration
  (`resolve_texture_registration_by_surface_id`, the encoder's call), take the next
  free output buffer, run the RGBA→YUYV pass with that buffer's mapping as the kernel's
  storage-buffer output, then `QBUF` with `bytesused = sizeimage` and `timestamp` set from
  the frame's `timestamp_ns` (so the driver marks it `TIMESTAMP_COPY` in the monotonic
  epoch every StreamLib stamp already lives in). `DQBUF` recycles; the loop is the crate's
  `OutputStream::next()` shape or raw `QBUF`/`DQBUF` after `STREAMON`, never the crate's
  `queue()` before `start()`. Loss is counted at the edge: a frame arriving while every
  buffer is queued and none has been dequeued is dropped and counted, logged at the
  built-ins' cadence.
- `teardown()`: `STREAMOFF`, `REQBUFS(0)`, close. The format token releases with the fd.

### §Media I/O — the camera source's first-device rule

- `list_camera_capture_devices` (`camera_source.rs:79-110`) skips a node whose
  `QUERYCAP.driver` is `v4l2 loopback` when picking the first device for an unset
  `device_id`; a `device_id` naming one is still opened. Without it the showcase graph
  can capture its own output.

### §Graphics (RHI / GPU) — one primitive, two tiers

- `VK_EXT_external_memory_host` enabled by the optional-extension pattern
  (`vulkan_device.rs:845-849`), recorded as `supports_host_pointer_import()`, with
  `min_imported_host_pointer_alignment` snapshotted through the properties chain at device
  create (`:1069-1072` shape).
- `GpuContextFullAccess::import_host_mapping_for_gpu_writes(host_ptr, byte_len) ->
  HostMappingWrittenByGpu`, in `core/context/gpu_context.rs` over a new
  `vulkan/rhi/vulkan_host_mapping_imported_as_buffer.rs`. One abstraction with the tier
  inside, the shape `opaque_fd_buffer_pool_host_cached` already takes
  (`vulkan_device.rs:1307-1335`):
  - **Imported tier.** `get_memory_host_pointer_properties_ext` on the page-aligned
    range; `vkAllocateMemory` with `ImportMemoryHostPointerInfoEXT`; a `VkBuffer` with
    `STORAGE_BUFFER` usage bound to it. `publish_to_host()` after the pass is a buffer
    memory barrier to the `HOST` stage. The kernel's writes land in the loopback's pages.
  - **Staged tier.** When the extension is absent or the import is refused for that range
    (the driver may decline a mapping of a character device — the one open question on the
    platform floor), the primitive allocates a `HOST_CACHED` staging buffer of the same
    length and `publish_to_host()` is one `memcpy` from staging into the range. Never the
    write-combined `acquire_storage_buffer` allocation — the 37 ms trap.
  - `tier() -> HostMappingTier::{ImportedHostPointer, HostCachedStagingCopy}`, logged once
    per sink at first frame with the reason for a fallback, `tracing::warn!` for a refused
    import on a device that advertised the extension.
- `RhiColorConverter::convert_image_to_yuyv_buffer(texture, layout, target:
  &HostMappingWrittenByGpu, width, height)` in `core/rhi/color_converter.rs`, the reverse
  of `convert_buffer_to_image`, over a new `vulkan/rhi/shaders/color_convert_rgba_image_to_yuyv_buffer.comp`
  registered in `build.rs:88-145`. Bindings: `sampled_image(0)` in, `storage_buffer(1)`
  out, push constants `resolution`. Recorded through `record()` on the converter's own
  command buffer between an image barrier and the primitive's host barrier, submitted
  through `submit_to_queue`, waited on a fence — `RgbToNv12Converter`'s wrapping
  (`rgb_to_nv12.rs:65-86`) applied to a buffer target. BT.601 limited-range matrix
  selected from the frame's `ColorInfo`, the same table the mirror shader reads.
- `v4l2_color.rs` gains the inverse mapping — `ColorInfo` → V4L2 `colorspace`,
  `ycbcr_enc`, `quantization`, `xfer_func` — beside the existing capture-direction table,
  so `S_FMT` tells readers the colorimetry the encoder would have.

### §Media I/O — the five wheel touchpoints

- `PythonVirtualCameraSinkBlock` marker (`python_native_builtin_blocks.rs`), the `is()`
  arm resolving to the media built-in's import path with the Linux-only refusal `VirtualCameraSink
  is Linux-only today; this platform is not supported by the streamlib wheel yet`,
  `add_class` (`src/lib.rs`), re-export and `__all__` (`__init__.py`), and the stub entry.
- Stub entry, spelled (the model is `Mp4Sink`, `_engine.pyi:250-292`):

  ```
  @final
  class VirtualCameraSink:
      """Native built-in block: video frames to a virtual camera any Linux
      application opens (v4l2loopback).

      A marker type — pass the class itself to `Runtime.add`
      (`rt.add(VirtualCameraSink)`); it is never instantiated and its per-frame path
      never enters the interpreter.

      One input, `video` (`newest`), and no output. Each frame is converted to YUYV
      on the GPU and written into the loopback device's own buffers, stamped with
      the frame's monotonic timestamp; readers see a camera at the frame's extent.

      `device` is the one config key and it is optional: unset picks the first
      v4l2loopback device found, set names one. The device must exist and be idle
      before `setup()`: a missing module, a device of another driver, or a device
      another producer feeds refuses by name at `setup()` with the command to run
      — the processor never reaches Running and the runtime keeps running.
      Loading the module is the user's one-time step, never the engine's.

      The device format follows the first frame's extent; an extent change
      re-negotiates it and attached readers reopen. Several instances may be added,
      one device each.
      """
  ```

### Tests

- `cargo test -p streamlib-media-builtins --lib` —
  `the_only_port_is_one_newest_input_and_there_is_no_output`,
  `the_config_names_the_device_and_nothing_else`,
  `a_missing_loopback_device_is_refused_by_name_with_the_modprobe_line`,
  `a_node_of_another_driver_is_refused_by_name`, and the enumeration filter
  `the_camera_list_skips_loopback_nodes_unless_named`, over a fake `QUERYCAP` answer.
  Mirrored at `test.yml:129` / `xtask/src/main.rs:284-294`.
- Engine-lib, named into `test.yml:214` and `xtask/src/main.rs:300-313`:
  `a_host_mapping_takes_the_imported_tier_when_the_device_allows_it`,
  `a_refused_import_falls_back_to_host_cached_staging_and_says_why`,
  `the_yuyv_pass_writes_every_pixel_of_the_target_range` (a synthetic RGBA image whose
  YUYV bytes are checked against a CPU conversion, both tiers).
- Wheel: `tests/test_virtual_camera_sink.py` — `test_the_marker_class_cannot_be_instantiated`,
  `test_display_name_defaults_to_the_type_name`, and under `requires_gpu`
  `test_frames_reach_a_loopback_device_and_read_back_as_yuyv`, which opens the device as
  a V4L2 capture reader (`v4l2-ctl --stream-mmap`), checks the extent and the fourcc, and
  checks that the buffer timestamps carry the frames' stamps. stubtest covers the entry.
- Rig proof, recorded in the ticket: with the module loaded as the message prescribes,
  the showcase running, and the node opened in Chrome (`chrome://webrtc-internals` or a
  `getUserMedia` page), OBS, and `v4l2-ctl`, each shows the effect. The tier the sink
  reports is written down with the driver version.

### §Consumers — the showcase

- `examples/camera-virtual-camera`: the scaffold's shape (`app.py` with `setup(rt)`,
  `pyproject.toml` on the explicit index, `.python-version`), `CameraSource` → the
  scaffolded Python effect → `VirtualCameraSink`. README sections as
  `camera-codec-roundtrip` has them; "Run it" is the one-time `modprobe` line and
  `streamlib dev`; "Observing it" opens the device in another application and taps the
  effect's output. It links nothing locally: the sink is in the wheel.

## MODIFIED

- §Consumers `:362` — `examples/` now stands at fourteen converted beside two held;
  `camera-virtual-camera` joins as the virtual camera's showcase, an ordinary addition
  under the convention at `:363-365`.
- §Media I/O `:814-833` — the DECIDED entry gains the ship tag and the tier the rig chose.
- `docs/decisions/virtual-camera-sink.md` — appends "The RHI primitive is one
  abstraction with a tier inside" and "The modprobe line the message prescribes":
  `exclusive_caps=1` because Chromium's enumerator rejects a node that also reports
  OUTPUT, and in 0.15.x the capabilities are per opener so the writer is unaffected;
  `max_buffers=4` because Chrome requests four and the module clamps to the parameter.

## REMOVED

None. This change adds a capability and deletes nothing; the ship gate has nothing to
verify.

## Not decided here, and known limits recorded for the ticket

- A reader's `S_PARM` overwrites the device-wide frame interval the sink set; the sink's
  cadence is the frame's arrival regardless, so this changes what a reader is told, not
  what it gets. Logged once when observed.
- A second producer is refused at `S_FMT`, not at `open()`, so the refusal is by name at
  `setup()` and never silent.
- The import experiment's result on the platform floor is the first ticket's first step;
  it selects the default tier and is written into the plan entry at fold time.
