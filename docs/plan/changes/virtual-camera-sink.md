# virtual-camera-sink

`VirtualCameraSink`, a native built-in beside `DisplayWindow`: video in, a camera any Linux
application can select out, as many instances as the graph adds, each with its own `name`.
Two doors, one open per instance: a v4l2loopback device when a free one exists, else a
PipeWire camera-role node. Implements §Media I/O's DECIDED entry
(`docs/plan/ARCHITECTURE.md:814-846`) and the built-in criterion's clause (c) (`:106-119`).
Rationale and the rig facts are `docs/decisions/virtual-camera-sink.md` (owner, 2026-09-06;
the two-door widening and the N-instance rule ruled later the same day and recorded in the
entry by this change).

**Scale gate — this skill, plus the ADR.** New behavior with a public marker class and a
stub entry, one new RHI primitive (a host-pointer import) that only the RHI may own
(`cargo xtask check-boundaries`, `xtask/src/check_boundaries.rs:275-307`), and the video
half of the engine's PipeWire shim. The ADR exists; this change appends its sections.

**Precondition.** The entries are DECIDED (#2194, merged 2026-09-06, widened here on the
owner's ruling). §Consumers `:363-365`: a showcase in the current idiom is an ordinary
addition. Sections flipped to `IN-FLIGHT (→ virtual-camera-sink)`: §Packages & extension
model, §Graphics (RHI / GPU), §Media I/O, §Consumers.

**Verified against the tree 2026-09-06 (HEAD bd28d57ea)** — three read-only recon sweeps
and one live probe.

- The media built-ins crate already depends on `v4l = "0.14"` and drops to raw
  `libc::ioctl` with the crate's `vidioc::*` constants where the safe API stops
  (`runtime/streamlib-media-builtins/Cargo.toml:31-34`; `camera_source.rs:173`, `:265`,
  `:503`, `:529`, `:662`, `:812-902`). The crate's default feature is pure bindgen with no
  `links` and no `rustc-link-lib`; `libv4l2` is linked only under its opt-in feature. So
  the loopback door adds nothing to `DT_NEEDED`
  (`sdk/streamlib-python-wheel/tests/test_wheel_portability.py:25-37`, `:121-132`).
- The crate carries the output half: `v4l::io::traits::OutputStream` (`queue`, `dequeue`,
  `next`) over `v4l::io::mmap::Stream::with_buffers(&dev, Type::VideoOutput, n)`, whose
  arena exposes the mappings as `bufs: Vec<&mut [u8]>`, page-aligned host pointers. Two
  traps: `queue()` polls `POLLOUT` before `VIDIOC_QBUF`, and v4l2loopback's poll returns 0
  between `REQBUFS` and `STREAMON`, so a hand-rolled queue before start hangs forever; and
  `Metadata::default()` sends `bytesused = 0`, which the driver warns on.
- v4l2loopback 0.15.3 from its source: `QUERYCAP.driver == "v4l2 loopback"`, `QUERYCAP.card`
  is the module's `card_label`; `S_FMT` on the OUTPUT type must precede `REQBUFS` and
  returns `EBUSY` while buffers are allocated or another opener holds the format; `REQBUFS`
  accepts `V4L2_MEMORY_MMAP` only and clamps the count to `max_buffers` (default 2);
  `QBUF` publishes synchronously and `DQBUF` never blocks — no back-pressure, the writer
  paces; a non-zero supplied timestamp is copied through under `TIMESTAMP_COPY`. With
  `exclusive_caps=1` capabilities are per opener: OUTPUT to the writer, CAPTURE-only to
  everyone else while it streams. The label is fixed at module load; nothing renames a
  device at runtime.
- Chromium's V4L2 enumerator lists a node only when it reports `VIDEO_CAPTURE` and not
  `VIDEO_OUTPUT` (`media/capture/video/linux/video_capture_device_factory_v4l2.cc:184-190`),
  so Chrome needs `exclusive_caps=1`; it asks for four buffers and treats an `S_PARM`
  failure as fatal; YUYV is accepted. OBS requires only `VIDEO_CAPTURE`. While the sink
  holds the format, `ENUM_FMT` reports the set format alone.
- **Live probe on the rig:** a `Video/Source` node with `media.role = Camera` registered
  from GStreamer appeared in the portal-exposed set (WirePlumber's `access-portal.lua`
  grants camera clients every node with that class and role) beside `v4l2_input.*` nodes
  for the vivid and Cam Link devices — the session manager's V4L2 monitor mirrors capture
  devices into that set with `device.api = v4l2`. Whether a loopback node under
  `exclusive_caps=1` is mirrored the same way is the ticket's first check beside the
  import experiment; the SPA monitor keys on the capture capability, which that mode
  reports to every opener but the writer. Chrome 152 here ships the PipeWire camera flag
  off; Firefox uses PipeWire cameras by default only where its pref was flipped.
- The engine reaches PipeWire through a `dlsym`'d C shim over vendored headers that adds
  no `DT_NEEDED` entry (`runtime/streamlib-engine/src/linux/pipewire_audio_shim.c`, the
  X-macro list in `pipewire_audio_shim.h:24-66`, arm `pipewire_audio_device_backend.rs`).
  It resolves the stream, loop, context, core, properties and time entry points, and
  compiles SPA's audio pod builders; it has no video format pods, no buffer-parameter
  negotiation, and no `add_buffer` handling.
- PipeWire's DMA-BUF procedure (its own documentation): a producer announces one format
  with a mandatory, unfixated modifier list and one shared-memory sibling, fixates on the
  modifier that allocates, sets the buffer data type to DMA-BUF when modifiers negotiated,
  allocates in `add_buffer`, and the consumer imports or takes the fallback.
- The engine's own camera list filters on `VIDEO_CAPTURE` (`camera_source.rs:79-110`), so a
  loopback node the sink feeds qualifies for the camera source's first-device pick: a graph
  `camera → effect → virtual camera` could capture itself.
- The RHI has no host-pointer import; `VK_EXT_external_memory_host` appears only in the
  vendored bindings (`vendor/tatolab-vulkanalia/src/vk/extensions.rs:2952-2977`,
  `builders.rs:50066-50097`). The optional-extension pattern is probe → push → record a
  `bool` → accessor (`runtime/streamlib-engine/src/vulkan/rhi/vulkan_device.rs:809-822`,
  `:845-849`, `:1178-1180`, `:2788`); the properties-chain precedent is `:1069-1072`. The
  rig reports the extension with a 4 KiB alignment.
- A compute kernel binds a `VkBuffer` as a storage-buffer output today
  (`core/rhi/compute_kernel.rs:145`; `vulkan/rhi/vulkan_compute_kernel.rs:773-784`,
  `:1013`). The mirror shader exists, YUYV buffer in, RGBA image out
  (`vulkan/rhi/shaders/color_convert_yuyv_buffer_to_rgba.comp:24-28`, built by
  `vulkan_color_converter.rs:183-220` behind `RhiColorConverter`,
  `core/rhi/color_converter.rs:219-283`); no image-to-YUYV direction exists. `dispatch()`
  records no host-stage barrier (`:531-596`); `record()` on a caller-owned command buffer
  (`:600-614`) is how `RgbToNv12Converter` wraps a kernel in its own barriers
  (`vulkan/video/rgb_to_nv12.rs:65-86`).
- The readback staging is write-combined (`vulkan_texture_readback.rs:118-126`) and
  `acquire_storage_buffer` lands on a sequential-write allocation
  (`vulkan_buffer.rs:168-200`, `:260-272`); the host-cached precedent is the decode path
  (`vulkan/video/decode/mod.rs:867-883`, `:911-921`: 37 ms a frame off write-combined
  memory) and `new_opaque_fd_export_host_cached` (`vulkan_buffer.rs:487-508`).
- Any published `VideoFrame` becomes a GPU image through
  `resolve_texture_registration_by_surface_id` (`core/context/gpu_context.rs:1317-1414`,
  buffer-backed fallback `:1461-1526`); the encoder built-in is the sink-shaped consumer
  (`published_surface_to_encoded_frame_encoder.rs:223-233`). Every cross-process texture
  carries its DRM modifier and plane layout (`python_helper_process_pixel_exchange.rs:385-434`).
- Registration and the five wheel touchpoints: `register_media_builtin_processor_types`
  (`runtime/streamlib-media-builtins/src/lib.rs:107-127`); marker, `is()` arm with the
  Linux-only refusal, `add_class`, re-export, stub
  (`python_native_builtin_blocks.rs:86-90`, `:120-130`, `:193-197`; `src/lib.rs:42-53`;
  `__init__.py:39-45`, `:84-110`; `_engine.pyi:28-80`, `:250-292` as the docstring model).
  stubtest gates the stub with no allowlist (`python-wheel.yml:155-157`).
- Tests reach CI by name only: media-builtins at `test.yml:129` / `xtask/src/main.rs:284-294`,
  the engine-lib slice at `test.yml:214` / `:300-313`. Hardware-bound wheel tests carry
  `requires_gpu` (`sdk/streamlib-python-wheel/pyproject.toml:61-67`), rig only.
- Colorimetry translation is capture-direction only (`v4l2_color.rs`).

## ADDED

### §Media I/O — the sink, its config, and the door rule

- `VirtualCameraSink` in `runtime/streamlib-media-builtins/src/virtual_camera_sink.rs`,
  Linux-gated in `lib.rs` beside `display_window`, registered in
  `register_media_builtin_processor_types`. Macro: `execution = reactive, scheduling = high,
  config = crate::virtual_camera_sink::VirtualCameraSinkConfig, input("video",
  delivery_profile = "newest", ...)`, no output. Reactive: neither door applies
  back-pressure, the frame's arrival is the pace, and `process()` blocks only on the GPU.
- `VirtualCameraSinkConfig { name: String = "StreamLib Camera", device: Option<String> }`,
  serde defaults. `name` is what every picker shows. Instances are unlimited; each is one
  camera.
- Door choice at `setup()`, per instance, logged once with the door and why:
  1. `device` set: open it, `QUERYCAP`; not `v4l2 loopback` → refuse by name (`... is
     driver "uvcvideo", not "v4l2 loopback"`); format held by another producer or
     instance → refuse by name (`... is already fed by another producer; one writer per
     loopback device`). Loopback door.
  2. `device` unset: enumerate `/dev/video*`, keep loopback nodes whose format token is
     free; prefer the one whose `QUERYCAP.card` equals `name`, else the first. Loopback door.
  3. No free loopback node: PipeWire door. The log line carries the module line for the
     machines that want the loopback door for every camera: `sudo modprobe v4l2loopback
     devices=N exclusive_caps=1 max_buffers=4 card_label="<name1>,<name2>,..."`, with the
     graph's instance names filled in, and the `modules-load.d` / `modprobe.d` note.
  Only a named device that cannot serve refuses; an unnamed instance always comes up on
  one door. The engine never loads a module and never asks for elevation.

### §Media I/O — the loopback door

- First frame: `S_FMT` on `V4L2_BUF_TYPE_VIDEO_OUTPUT` (YUYV, `bytesperline = 2·width`,
  `sizeimage = 2·width·height`, colorimetry through the new inverse map), `S_PARM` from
  the frame's `fps` when present, `REQBUFS(4, MMAP)` accepting the clamped count,
  `QUERYBUF` + `mmap`, `STREAMON` — in that order, before any queue. Each mapping is handed
  to the RHI primitive below once. An extent change runs `REQBUFS(0)` → `S_FMT` → `REQBUFS`
  → `STREAMON` again and re-imports; readers reopen as for a camera re-plugged.
- Per frame: resolve the frame to a texture registration, take the next free output
  buffer, run the RGBA→YUYV pass with that buffer's mapping as the kernel's storage-buffer
  output, `QBUF` with `bytesused = sizeimage` and `timestamp` from the frame's
  `timestamp_ns` (the driver marks it `TIMESTAMP_COPY`, in the monotonic epoch every
  StreamLib stamp already lives in). Raw `QBUF`/`DQBUF` after `STREAMON`, never the crate's
  `queue()` before `start()`. A frame arriving with every buffer queued is dropped and
  counted at the edge, logged at the built-ins' cadence.
- `teardown()`: `STREAMOFF`, `REQBUFS(0)`, close; the format token releases with the fd.

### §Media I/O — the PipeWire door

- The engine's shim gains its video half: SPA video-format pod builders
  (`spa_format_video_raw_build` and the modifier property with the mandatory and
  don't-fixate flags), the buffers parameter with `dataType`, and the `add_buffer` /
  `remove_buffer` / `param_changed` stream events, as inline C compiled into
  `pipewire_audio_shim.c`'s sibling `pipewire_video_source_shim.c` under the same rule: it
  calls only the pointers Rust filled, and every `pw_*` it needs is already in the X-macro
  list or is added there and resolved by name. No new `DT_NEEDED` entry; the portability
  test is the pass/fail.
- A Rust arm `runtime/streamlib-engine/src/linux/pipewire_video_source.rs` beside the
  audio backend: `PipeWireCameraNode::open(name, extent, modifiers)` registers a stream as
  `Video/Source`, `media.role = Camera`, `node.description = name`, `node.name` derived from
  it, connects with `ALLOC_BUFFERS | DRIVER`, and drives negotiation: BGRx/RGBA offered with
  the engine's tiled DMA-BUF modifier (unfixated) beside the shared-memory sibling; on
  fixation the arm allocates the negotiated count from the sink's own texture ring —
  DMA-BUF-flavoured textures whose fds and plane layout the engine already holds — and
  hands each fd to `add_buffer`. The consumer imports zero-copy. When the consumer takes the
  shared-memory sibling, the arm readbacks each frame into host-cached staging and copies
  into the shared-memory buffer, the fallback tier again. Reached from the built-in through
  a `GpuContextFullAccess` door, never through raw PipeWire in media-builtins.
- Per frame: copy the resolved texture into the ring's next slot on the GPU (a blit; the
  frame's own surface is another processor's to recycle), publish the slot's buffer with
  the frame's stamp in `pw_buffer`'s time. Dispatch returns retired, so implicit sync holds.
- Extent change re-negotiates the format; consumers reconnect.

### §Media I/O — the camera source's first-device rule

- `list_camera_capture_devices` (`camera_source.rs:79-110`) skips a node whose
  `QUERYCAP.driver` is `v4l2 loopback` when picking the first device for an unset
  `device_id`; a `device_id` naming one is still opened. Otherwise the showcase captures
  itself.

### §Graphics (RHI / GPU) — one primitive, two tiers

- `VK_EXT_external_memory_host` enabled by the optional-extension pattern
  (`vulkan_device.rs:845-849`), recorded as `supports_host_pointer_import()`, with
  `min_imported_host_pointer_alignment` snapshotted at device create (`:1069-1072` shape).
- `GpuContextFullAccess::import_host_mapping_for_gpu_writes(host_ptr, byte_len) ->
  HostMappingWrittenByGpu`, in `core/context/gpu_context.rs` over a new
  `vulkan/rhi/vulkan_host_mapping_imported_as_buffer.rs`. One abstraction with the tier
  inside, the shape `opaque_fd_buffer_pool_host_cached` takes (`vulkan_device.rs:1307-1335`):
  - **Imported tier.** `get_memory_host_pointer_properties_ext` on the page-aligned range;
    `vkAllocateMemory` with `ImportMemoryHostPointerInfoEXT`; a `VkBuffer` with
    `STORAGE_BUFFER` usage bound to it; `publish_to_host()` is a buffer memory barrier to
    the `HOST` stage. The kernel's writes land in the loopback's pages.
  - **Staged tier.** Extension absent or the import refused for that range (the driver may
    decline a mapping of a character device — the open question on the platform floor):
    a `HOST_CACHED` staging buffer of the same length, `publish_to_host()` one `memcpy`
    into the range. Never the write-combined `acquire_storage_buffer` allocation.
  - `tier() -> HostMappingTier::{ImportedHostPointer, HostCachedStagingCopy}`, logged once
    per sink at first frame with the reason for a fallback.
- `RhiColorConverter::convert_image_to_yuyv_buffer(texture, layout, target:
  &HostMappingWrittenByGpu, width, height)` in `core/rhi/color_converter.rs`, the reverse of
  `convert_buffer_to_image`, over `vulkan/rhi/shaders/color_convert_rgba_image_to_yuyv_buffer.comp`
  registered in `build.rs:88-145`: `sampled_image(0)` in, `storage_buffer(1)` out, push
  constants `resolution`; recorded through `record()` on the converter's own command buffer
  between an image barrier and the primitive's host barrier, `RgbToNv12Converter`'s wrapping
  (`rgb_to_nv12.rs:65-86`) on a buffer target; BT.601 limited range from the frame's
  `ColorInfo`, the mirror shader's table.
- `v4l2_color.rs` gains the inverse mapping — `ColorInfo` → V4L2 `colorspace`, `ycbcr_enc`,
  `quantization`, `xfer_func` — so `S_FMT` tells readers the colorimetry.

### §Media I/O — the five wheel touchpoints

- `PythonVirtualCameraSinkBlock` marker, the `is()` arm with `VirtualCameraSink is
  Linux-only today; this platform is not supported by the streamlib wheel yet`,
  `add_class`, re-export and `__all__`, and the stub entry (model `Mp4Sink`, `:250-292`):

  ```
  @final
  class VirtualCameraSink:
      """Native built-in block: video frames to a virtual camera any Linux
      application can select — a v4l2loopback device when one is free, else a
      PipeWire camera node.

      A marker type — pass the class itself to `Runtime.add`
      (`rt.add(VirtualCameraSink, config={"name": "Desk cam"})`); it is never
      instantiated and its per-frame path never enters the interpreter.

      One input, `video` (`newest`), and no output. Add as many instances as the
      graph needs; each is its own camera.

      Config keys: `name`, the camera's name in every picker, defaulting to
      "StreamLib Camera" — on the loopback door it also picks the device whose
      label matches when several are loaded; `device`, optional, naming one
      v4l2loopback node. The door is chosen at `setup()` and logged with the
      `modprobe` line for machines that want the loopback door: a free loopback
      device takes it (every application sees that one), otherwise the PipeWire
      door, which needs no module and no root. A named device of another driver,
      or one another producer feeds, refuses by name at `setup()`; the runtime
      keeps running. The engine never loads a module or asks for elevation.

      Frames are stamped with their monotonic timestamp on both doors; the format
      follows the first frame's extent, and an extent change re-negotiates it.
      """
  ```

### Tests

- `cargo test -p streamlib-media-builtins --lib`:
  `the_only_port_is_one_newest_input_and_there_is_no_output`,
  `the_config_names_the_camera_and_the_device_and_nothing_else`,
  `a_named_node_of_another_driver_is_refused_by_name`,
  `a_free_loopback_device_wins_the_door_and_none_means_pipewire` (over a fake `QUERYCAP`
  answer set), `the_modprobe_line_lists_every_instance_name`,
  `the_camera_list_skips_loopback_nodes_unless_named`. Mirrored at `test.yml:129` /
  `xtask/src/main.rs:284-294`.
- Engine-lib, named into `test.yml:214` and `xtask/src/main.rs:300-313`:
  `a_host_mapping_takes_the_imported_tier_when_the_device_allows_it`,
  `a_refused_import_falls_back_to_host_cached_staging_and_says_why`,
  `the_yuyv_pass_writes_every_pixel_of_the_target_range`,
  `the_video_shim_names_every_entry_point_it_expects_rust_to_resolve` (the audio shim's
  own test, applied to the video half),
  `a_pipewire_camera_node_offers_a_modifier_and_a_shared_memory_sibling` (pod contents).
- Wheel `tests/test_virtual_camera_sink.py`: `test_the_marker_class_cannot_be_instantiated`,
  `test_display_name_defaults_to_the_type_name`, and under `requires_gpu`
  `test_frames_reach_a_loopback_device_and_read_back_as_yuyv` (opens the device as a
  capture reader, checks extent, fourcc, and the stamps) and
  `test_without_a_loopback_device_a_pipewire_camera_node_appears` (`pw-dump` shows the
  node with the configured name, class and role).
- Rig proof, recorded in the ticket with the driver version and the tier: with the module
  loaded as the line prescribes, two sinks in one graph appear as two named cameras in
  Chrome, OBS and `v4l2-ctl`; with the module unloaded, the PipeWire node appears in
  Firefox's picker and in `pw-dump`, and whether a loopback node is mirrored into the
  portal set under `exclusive_caps=1` is written down.

### §Consumers — the showcase

- `examples/camera-virtual-camera`: the scaffold's shape, `CameraSource` → the scaffolded
  Python effect → `VirtualCameraSink` named from an environment variable, and a second
  sink on the raw camera to show two cameras from one graph. README as
  `camera-codec-roundtrip`: "Run it" is `streamlib dev`, with the module line as the
  optional step for `/dev/video*`-only applications; "Observing it" opens the cameras in
  another application and taps the effect's output. It links nothing locally.

## MODIFIED

- §Media I/O `:814-846` — the DECIDED entry, widened by the owner's ruling to two doors,
  unlimited instances and the `name` key; its PipeWire OPEN folded in and removed. At fold
  time it gains the ship tag and the tier the rig chose.
- §Consumers `:362` — fourteen converted beside two held; `camera-virtual-camera` joins as
  the virtual camera's showcase under the convention at `:363-365`.
- `docs/decisions/virtual-camera-sink.md` — three sections appended: the RHI primitive's
  tier, the module line the message prescribes, and the two-door rule with N cameras.

## REMOVED

None. This change adds a capability and deletes nothing; the ship gate has nothing to
verify.

## Known limits, recorded for the tickets

- A reader's `S_PARM` overwrites the loopback's device-wide frame interval; the sink's
  cadence is the frame's arrival regardless. Logged once when observed.
- The loopback label is the module's: `name` selects among loaded devices and fills the
  prescribed line, it never renames one.
- Three tracer bullets, not two: the loopback door with the import experiment first, the
  PipeWire door with the shim's video half, then the showcase — each demoable alone.
