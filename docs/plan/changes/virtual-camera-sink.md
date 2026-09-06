# virtual-camera-sink

`VirtualCameraSink`, a native built-in beside `DisplayWindow`: video in, a camera any Linux
application can select out. Each instance is one camera that exists only while its processor
runs — created at `setup()`, removed at `teardown()`, a USB camera plugged in and pulled out
from every other application's point of view, showing whatever the graph writes. As many
instances as the graph adds, each with its own `name`. Two doors, one per instance: a
v4l2loopback device the sink creates through the module's control node, else a PipeWire
camera-role node. Implements §Media I/O's DECIDED entry
(`docs/plan/ARCHITECTURE.md:814-851`) and the built-in criterion's clause (c) (`:106-119`).
Rationale and the rig facts are `docs/decisions/virtual-camera-sink.md` (owner rulings,
2026-09-06, recorded in the entry by this change).

**Scale gate — this skill, plus the ADR.** New behavior with a public marker class and a
stub entry, one new RHI primitive (a host-pointer import) that only the RHI may own
(`cargo xtask check-boundaries`, `xtask/src/check_boundaries.rs:275-307`), and the video
half of the engine's PipeWire shim. The ADR exists; this change appends its sections.

**Precondition.** The entries are DECIDED (#2194, merged 2026-09-06, widened here on the
owner's rulings). §Consumers `:363-365`: a showcase in the current idiom is an ordinary
addition. Sections flipped to `IN-FLIGHT (→ virtual-camera-sink)`: §Packages & extension
model, §Graphics (RHI / GPU), §Media I/O, §Consumers.

**Out of scope, by the owner's word.** Nothing here touches the engine's camera source, the
test-pattern source, or the rig's capture devices and their verification fixtures. The sink
is a producer of a new device; it reads no camera and shares no code with one.

**Verified against the tree 2026-09-06 (HEAD bd28d57ea)** — three read-only recon sweeps
and one live probe.

- **The module's control node** (v4l2loopback 0.15.3, from its source): `/dev/v4l2loopback`
  is a misc device registered with no mode, so its default is root-only. `CTL_ADD` takes a
  `v4l2_loopback_config` — `output_nr`, `card_label`, `announce_all_caps`, `max_buffers`,
  `max_openers`, extents — and answers with the device number; `CTL_REMOVE` takes a number
  and returns `EBUSY` while any opener holds the device; `CTL_QUERY` reads a device's
  config back. No capability check sits behind the ioctls beyond opening the node. The
  module loads with `devices=0`. A device persists until removed or the module unloads;
  nothing ties it to the creating process.
- **Granting the node without root:** `TAG+="uaccess"` on a udev rule hands the active
  seat user an ACL through logind (`/usr/lib/udev/rules.d/70-uaccess.rules:16`); the rig
  runs a seat with that rule present. A one-time step, never the engine's.
- The device itself, once created: `QUERYCAP.driver == "v4l2 loopback"`, `card` is the
  label; `S_FMT` on the OUTPUT type must precede `REQBUFS` and returns `EBUSY` while
  buffers are allocated or another opener holds the format; `REQBUFS` accepts
  `V4L2_MEMORY_MMAP` only and clamps to the device's buffer count; `QBUF` publishes
  synchronously and `DQBUF` never blocks; a non-zero supplied timestamp is copied through
  under `TIMESTAMP_COPY`. With capture-only capabilities announced, the writer sees OUTPUT
  and every other opener sees CAPTURE while it streams; Chromium's V4L2 enumerator lists a
  node only in that mode (`video_capture_device_factory_v4l2.cc:184-190`), asks for four
  buffers and accepts YUYV; OBS requires only capture. `ENUM_FMT` reports the set format
  alone while the sink holds it.
- The media built-ins crate already depends on `v4l = "0.14"` and drops to raw
  `libc::ioctl` with the crate's constants where its safe API stops
  (`runtime/streamlib-media-builtins/Cargo.toml:31-34`; `camera_source.rs:503`, `:812-902`
  as the ioctl precedent only). Default features are pure bindgen with no `links`, so the
  loopback door adds nothing to `DT_NEEDED`
  (`sdk/streamlib-python-wheel/tests/test_wheel_portability.py:25-37`, `:121-132`). The
  crate's `OutputStream` over `mmap::Stream::with_buffers(&dev, Type::VideoOutput, n)`
  exposes the mappings as `bufs: Vec<&mut [u8]>`, page-aligned; its `queue()` polls
  `POLLOUT` first, and the loopback's poll returns 0 between `REQBUFS` and `STREAMON`, so a
  queue before start hangs; `Metadata::default()` sends `bytesused = 0`. The control ioctls
  are not in the crate; the `v4l2loopback` crate (MIT, 0.1.0) binds them but is a one-off —
  the two ioctls and the config struct are declared in-tree as ABI facts instead.
- **Live probe on the rig:** a `Video/Source` node with `media.role = Camera` registered
  from GStreamer appeared in the portal-exposed set (WirePlumber's `access-portal.lua`
  grants camera clients every node with that class and role), and the session manager's
  V4L2 monitor mirrors capture devices into the same set with `device.api = v4l2`. So a
  loopback device is visible to portal-based applications too. Chrome 152 here ships the
  PipeWire camera flag off; Firefox uses PipeWire cameras by default only where its pref
  was flipped.
- The engine reaches PipeWire through a `dlsym`'d C shim over vendored headers that adds
  no `DT_NEEDED` entry (`runtime/streamlib-engine/src/linux/pipewire_audio_shim.c`, the
  X-macro list in `pipewire_audio_shim.h:24-66`, arm `pipewire_audio_device_backend.rs`).
  It resolves the stream, loop, context, core, properties and time entry points and
  compiles SPA's audio pods; it has no video format pods, no buffer-parameter negotiation,
  and no `add_buffer` handling. PipeWire's DMA-BUF procedure (its own documentation): one
  format with a mandatory, unfixated modifier list beside a shared-memory sibling, fixate
  on the modifier that allocates, buffer data type DMA-BUF when modifiers negotiated,
  allocate in `add_buffer`, the consumer imports or takes the fallback.
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
- Colorimetry translation exists in the capture direction only (`v4l2_color.rs`); the sink
  needs the inverse for `S_FMT`.

## ADDED

### §Media I/O — the sink, its config, and the door rule

- `VirtualCameraSink` in `runtime/streamlib-media-builtins/src/virtual_camera_sink.rs`,
  Linux-gated in `lib.rs` beside `display_window`, registered in
  `register_media_builtin_processor_types`. Macro: `execution = reactive, scheduling = high,
  config = crate::virtual_camera_sink::VirtualCameraSinkConfig, input("video",
  delivery_profile = "newest", ...)`, no output. Reactive: neither door applies
  back-pressure, the frame's arrival is the pace, and `process()` blocks only on the GPU.
- `VirtualCameraSinkConfig { name: String = "StreamLib Camera", door: VirtualCameraDoor =
  Auto }` with `VirtualCameraDoor::{Auto, V4l2Loopback, PipeWire}`, serde
  `snake_case`. `name` is what every picker shows on either door. Instances are unlimited;
  each is one camera.
- Door choice at `setup()`, per instance, logged once with the door and the reason:
  - `auto`: the loopback door when `/dev/v4l2loopback` opens read-write; else the PipeWire
    door, with the log line carrying the one-time step for machines that want the
    loopback door: `sudo modprobe v4l2loopback devices=0` (persisted in `modules-load.d`
    and `modprobe.d`) and a rule `KERNEL=="v4l2loopback", SUBSYSTEM=="misc",
    TAG+="uaccess"` in `/etc/udev/rules.d`.
  - `v4l2loopback`: the loopback door or a refusal by name at `setup()` carrying those two
    lines — the processor never reaches Running, the runtime keeps running.
  - `pipewire`: that door, refusing by name only when no session daemon answers.
  The engine never loads a module, never writes a rule, never asks for elevation.

### §Media I/O — the loopback door

- `setup()`: `CTL_QUERY` over the module's devices for one whose label is `name` — a device
  this sink left behind, at a crash or behind a reader that held it at teardown — and reclaim
  it; else `CTL_ADD` with `card_label = name`, `announce_all_caps = 0` (capture-only to
  readers, the mode Chromium lists), `max_buffers = 4`, `output_nr = -1` (the module picks
  the number). Open the returned `/dev/videoN`. The two ioctls and `v4l2_loopback_config`
  are declared in-tree beside the sink as the module's ABI, with a layout test.
- First frame: `S_FMT` on `V4L2_BUF_TYPE_VIDEO_OUTPUT` (YUYV, `bytesperline = 2·width`,
  `sizeimage = 2·width·height`, colorimetry through the new inverse map), `S_PARM` from
  the frame's `fps` when present, `REQBUFS(4, MMAP)`, `QUERYBUF` + `mmap`, `STREAMON` — in
  that order, before any queue. Each mapping is handed to the RHI primitive below once. An
  extent change runs `REQBUFS(0)` → `S_FMT` → `REQBUFS` → `STREAMON` again and re-imports;
  readers reopen as for a camera re-plugged.
- Per frame: resolve the frame to a texture registration, take the next free output
  buffer, run the RGBA→YUYV pass with that buffer's mapping as the kernel's storage-buffer
  output, `QBUF` with `bytesused = sizeimage` and `timestamp` from the frame's
  `timestamp_ns` (the driver marks it `TIMESTAMP_COPY`, in the monotonic epoch every
  StreamLib stamp already lives in). Raw `QBUF`/`DQBUF` after `STREAMON`, never the crate's
  `queue()` before `start()`. A frame arriving with every buffer queued is dropped and
  counted at the edge, logged at the built-ins' cadence.
- `teardown()`: `STREAMOFF`, `REQBUFS(0)`, close, then `CTL_REMOVE`. `EBUSY` — a reader
  still holds the camera — is logged by name and the device is left for the next
  `setup()` to reclaim; nothing waits on another application.

### §Media I/O — the PipeWire door

- The engine's shim gains its video half: SPA video-format pod builders
  (`spa_format_video_raw_build` and the modifier property with the mandatory and
  don't-fixate flags), the buffers parameter with `dataType`, and the `add_buffer` /
  `remove_buffer` / `param_changed` stream events, as inline C compiled into a sibling
  `pipewire_video_source_shim.c` under the same rule: it calls only the pointers Rust
  filled, and every `pw_*` it needs is in the X-macro list or added there and resolved by
  name. No new `DT_NEEDED` entry; the portability test is the pass/fail.
- A Rust arm `runtime/streamlib-engine/src/linux/pipewire_video_source.rs` beside the
  audio backend: `PipeWireCameraNode::open(name, extent, modifiers)` registers a stream as
  `Video/Source`, `media.role = Camera`, `node.description = name`, `node.name` derived from
  it, connects with `ALLOC_BUFFERS | DRIVER`, and drives negotiation: BGRx/RGBA offered with
  the engine's tiled DMA-BUF modifier (unfixated) beside the shared-memory sibling; on
  fixation the arm allocates the negotiated count from the sink's own texture ring —
  DMA-BUF-flavoured textures whose fds and plane layout the engine already holds — and
  hands each fd to `add_buffer`. The consumer imports zero-copy. When it takes the
  shared-memory sibling, the arm readbacks each frame into host-cached staging and copies
  into the shared-memory buffer, the fallback tier again. Reached from the built-in through
  a `GpuContextFullAccess` door, never through raw PipeWire in media-builtins. `close()`
  disconnects and destroys the stream, and the node is gone with it.
- Per frame: copy the resolved texture into the ring's next slot on the GPU (a blit; the
  frame's own surface is another processor's to recycle), publish the slot's buffer with
  the frame's stamp in `pw_buffer`'s time. Dispatch returns retired, so implicit sync holds.
- Extent change re-negotiates the format; consumers reconnect.

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
      application can select. Each instance is one camera that exists while its
      processor runs — created at setup, removed at teardown, like a USB camera
      plugged in and pulled out — showing whatever the graph writes into it.

      A marker type — pass the class itself to `Runtime.add`
      (`rt.add(VirtualCameraSink, config={"name": "Desk cam"})`); it is never
      instantiated and its per-frame path never enters the interpreter.

      One input, `video` (`newest`), and no output. Add as many instances as the
      graph needs; each is its own camera.

      Config keys: `name`, the camera's name in every picker, defaulting to
      "StreamLib Camera"; `door`, "auto" (default), "v4l2loopback", or "pipewire".
      Under "auto" the sink creates a v4l2loopback device when the module's
      control node is writable — the door every application sees — and otherwise
      registers a PipeWire camera node, which needs no module and no root. The
      door is logged at setup with the one-time lines that enable the loopback
      door on this machine; "v4l2loopback" refuses by name at `setup()` with
      those lines when they are missing, and the runtime keeps running. The
      engine never loads a module or asks for elevation.

      A loopback device a reader still holds at teardown is left in place and
      reclaimed by name at the next setup. Frames are stamped with their
      monotonic timestamp on both doors; the format follows the first frame's
      extent, and an extent change re-negotiates it.
      """
  ```

### Tests

- `cargo test -p streamlib-media-builtins --lib`:
  `the_only_port_is_one_newest_input_and_there_is_no_output`,
  `the_config_names_the_camera_and_the_door_and_nothing_else`,
  `auto_takes_the_loopback_door_when_the_control_node_opens_and_pipewire_otherwise`,
  `a_forced_loopback_door_without_the_control_node_refuses_with_both_lines`,
  `a_device_carrying_this_sinks_label_is_reclaimed_rather_than_duplicated`,
  `the_control_config_struct_matches_the_modules_layout` (layout test), over a fake
  control-node answer set. Mirrored at `test.yml:129` / `xtask/src/main.rs:284-294`.
- Engine-lib, named into `test.yml:214` and `xtask/src/main.rs:300-313`:
  `a_host_mapping_takes_the_imported_tier_when_the_device_allows_it`,
  `a_refused_import_falls_back_to_host_cached_staging_and_says_why`,
  `the_yuyv_pass_writes_every_pixel_of_the_target_range`,
  `the_video_shim_names_every_entry_point_it_expects_rust_to_resolve`,
  `a_pipewire_camera_node_offers_a_modifier_and_a_shared_memory_sibling`.
- Wheel `tests/test_virtual_camera_sink.py`: `test_the_marker_class_cannot_be_instantiated`,
  `test_display_name_defaults_to_the_type_name`, and under `requires_gpu`
  `test_a_camera_appears_while_the_graph_runs_and_is_gone_after_shutdown` (the device or
  node with the configured name exists during the run and not after),
  `test_frames_reach_the_loopback_device_and_read_back_as_yuyv` (a capture reader checks
  extent, fourcc, and the stamps), `test_without_the_control_node_a_pipewire_camera_node_appears`.
- Rig proof, recorded in the ticket with the driver version and the tier: two sinks in one
  graph appear as two named cameras in Chrome, OBS and `v4l2-ctl` while it runs and vanish
  at shutdown; with the control node withheld, the PipeWire node appears in a portal-based
  picker and in `pw-dump`.

### §Consumers — the showcase

- `examples/camera-virtual-camera`: the scaffold's shape, the scaffolded source → the
  scaffolded Python effect → `VirtualCameraSink` named from an environment variable, and a
  second sink on the effect's other output to show two cameras from one graph. README as
  `camera-codec-roundtrip`: "Run it" is `streamlib dev`, with the two one-time lines as the
  optional step for `/dev/video*`-only applications; "Observing it" opens the cameras in
  another application. It links nothing locally.

## MODIFIED

- §Media I/O `:814-851` — the DECIDED entry, widened by the owner's rulings to two doors, a
  device per processor, unlimited instances and the `name` key; its PipeWire OPEN folded in
  and removed. At fold time it gains the ship tag and the tier the rig chose.
- §Consumers `:362` — fourteen converted beside two held; `camera-virtual-camera` joins as
  the virtual camera's showcase under the convention at `:363-365`.
- `docs/decisions/virtual-camera-sink.md` — sections appended for the RHI primitive's
  tier, the two-door rule, and the device-per-processor ruling, with the earlier
  module-line and pre-loaded-device reasoning annotated superseded.

## REMOVED

None. This change adds a capability and deletes nothing; the ship gate has nothing to
verify.

## Known limits, recorded for the tickets

- A device outlives a crash and `CTL_REMOVE` returns `EBUSY` while a reader holds it, so
  "gone at shutdown" is best-effort: the next `setup()` reclaims by label.
- A reader's `S_PARM` overwrites the loopback's device-wide frame interval; the sink's
  cadence is the frame's arrival regardless. Logged once when observed.
- Three tracer bullets: the loopback door with the control node and the import experiment
  first, the PipeWire door with the shim's video half, then the showcase — each demoable
  alone.
