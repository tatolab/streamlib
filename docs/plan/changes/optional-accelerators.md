# optional-accelerators

A stream that needs no GPU runs on a machine without one, and the engine never runs without a
device. After this change:
- the Linux runtime unit carries the Vulkan loader and Mesa lavapipe in
  `tatolab/runtime/_vulkan_driver/`, named to the loader additively as macOS names MoltenVK;
- the first stream to start builds the one `GpuContext` on the first device the engine can
  build on, in the loader's order with CPU-type last; a device it cannot build on is passed
  over and named, and a runtime that can build none keeps refusing stream starts by name;
- a processor interpreter imports on the runtime's device, handed its UUID and the loader
  environment at spawn;
- every stream starts on the software device; the tiers it lacks refuse by name at the node;
  DMA-BUF and the cross-process timeline are absent on it by rule, so frames cross as
  OPAQUE_FD and every copy is retired on the host before its reply;
- the Linux CI lane runs the engine on the unit's own driver, and rig-only is a marker whose
  reason comes from a closed list.

**Scale gate — this skill, plus an ADR.** New behaviour in the RHI (device selection, flavour
derivation per device), on the surface-share wire (an OPAQUE_FD pixel buffer; an optional
timeline), in the helper's import path and in the unit's contents. The ADR is
`docs/decisions/optional-accelerators.md` (owner, 2026-10-13).

**Precondition.** Every entry touched is DECIDED: §Graphics `ARCHITECTURE.md:2100-2142` (the
three entries this builds), §Product `:91`, `:173` (the `requires_gpu` clauses), §Distribution
`:3849-3888` (portability), `:3961-3980` (the Linux installer).

**Verified against the tree 2026-10-13 (HEAD 666563f95)**, four read-only sweeps; lavapipe
was run on the rig with the engine's own test binary (`VK_DRIVER_FILES=…/lvp_icd.json`,
Mesa 25.2.8). `E` = `runtime/streamlib-engine/src`, `V` = `E/vulkan/rhi`, `C` =
`E/core/context`, `SE` = `E/core/compiler/compiler_ops/subprocess_escalate`, `W` =
`sdk/streamlib-python-wheel/src/python_helper_process_pixel_exchange`, `CR` =
`runtime/streamlib-consumer-rhi/src`.
- **Startup is already lazy.** `Runner::new_with_construction_options` does no GPU work
  (`E/core/runtime/runtime.rs:514-659`); the slot `gpu_context_created_by_the_first_stream_start`
  is filled by `gpu_context_view_for_a_starting_stream` (`:171-198`) through
  `GpuContext::init_for_platform_sync` → `GpuDevice::new` → `HostVulkanDevice::new`; failure is
  `Error::EngineResourceRefusedAtAStreamsStart` (`E/core/runtime/loaded_stream.rs:422-429`),
  and the fixture `run_stream_on_tatolabd_with_no_vulkan_driver`
  (`tests/stream-on-runtime/conftest.py:645-728`) pins "No usable Vulkan driver (ICD) was found".
- **Selection is one pick with no fallthrough.** `V/vulkan_device.rs:766-786` takes the first
  `DISCRETE_GPU`, else `physical_devices[0]`; features are requested blind (`:1391-1457`) and a
  missing one fails `create_device` (`:1483`); `new()` is one 1,300-line function whose `Drop`
  (`:4798-4841`) destroys instance and device together, so a failed candidate cannot be
  dropped without the instance. The helper copies the same rule
  (`CR/consumer_vulkan_device.rs:169-185`) and requires `VK_EXT_external_memory_dma_buf` and
  `VK_EXT_image_drm_format_modifier` (`:61-67`); nothing compares its `deviceUUID` (`:194-202`)
  with a registration's `exporting_device_uuid`.
- **On lavapipe the device comes up** (`a_device_comes_up_on_this_hosts_driver_and_names_itself`
  passes, naming `llvmpipe`); all seven export-pool sentinels pass (`:2038-2160`); the first
  real tiled allocation then refuses "Driver picked modifier 0x0 … not in our candidate list"
  (`V/vulkan_texture.rs:551`) because the modifier table is the process's EGL default-display
  probe (`E/linux/drm_modifier_probe.rs:303`, called at `:1586-1599`), not the device's.
- **Lavapipe exports no semaphore.** `HostVulkanTimelineSemaphore::new_exportable`
  (`V/vulkan_sync.rs:184`) fails with "An external handle is not a valid handle of the
  specified type"; 25 of 26 escalate and staging test failures are this one error. The engine
  mints an exportable timeline per helper texture and image (`SE/acquisition/linux.rs:152-231`
  via `new_exportable_timeline_edge`, `SE/acquisition/mod.rs:361-371`) and per staging
  (`C/surface_export_staging.rs:417`), with no capability query anywhere; the staging
  registration takes it as mandatory (`C/surface_store.rs:840-844`) and the helper refuses a
  staging check-out without `[staging_fd, copy_done_fd]` (`W/linux/export_staging.rs:566-597`),
  waiting on it at `:380-395`. Texture edges are already `Option` on both sides
  (`C/surface_store.rs:924-929`; `W/linux/texture.rs:42-64`).
- **DMA-BUF is platform-gated, not device-gated.** `HostVulkanTexture::new` chains a DMA-BUF
  external create-info whenever `CROSS_PROCESS_EXPORT_BY_FILE_DESCRIPTOR_EXISTS_ON_THIS_PLATFORM`
  (`V/vulkan_device.rs:65`); DMA-BUF pools are built on `VK_KHR_external_memory_fd` alone
  (`:1086-1087`); the pixel-buffer slot is always `HostVulkanBuffer::new`, DMA-BUF
  (`V/vulkan_pixel_buffer_pool.rs:14-38`); the helper refuses any `pixel_buffer` whose
  `handle_type` is not `dma_buf` (`W/linux/mod.rs:365-376`) and imports with
  `from_dma_buf_fds` (`:405-415`); `SurfaceStore::lookup_buffer` refuses OPAQUE_FD
  (`C/surface_store.rs:1155-1170`). The OPAQUE_FD shapes that exist: the CPU-readback staging
  registration (`:814-867`, `resource_type pixel_buffer` + `handle_type opaque_fd` +
  `vk_memory_type_index`) and the helper import
  `ConsumerVulkanBuffer::from_opaque_fd_at_stated_memory_type_index`
  (`W/linux/export_staging.rs:454`, one plane). Lavapipe's DMA-BUF export depends on opening
  `/dev/udmabuf` (`root:render 660` on the rig; the `kvm` group on Ubuntu's default rule).
- **CUDA is checked late.** `as_device_tensor().__enter__` first escalates the engine into a
  DEVICE_LOCAL OPAQUE_FD staging plus a timeline, then `import_opaque_fd_into_cuda` refuses
  through `cuda_unavailable_reason` (`sdk/streamlib-python-wheel/src/python_cuda_pixel_exchange.rs:143-213`),
  naming no Vulkan device. `GpuCapabilitiesSnapshot` (`C/gpu_context.rs:1136-1147`) carries
  no device type. The CUDA Array Interface has no implementation (`_cross_floor_check.py:80` only).
- **The spawn inherits tatolabd's environment** and sets no Vulkan variable
  (`E/core/compiler/compiler_ops/processor_interpreter_spawn_host.rs:152-311`); `tatolabd`
  sets `VK_ADD_DRIVER_FILES` once at `runtime/tatolabd/src/main.rs:43-57` through
  `bundled_vulkan_driver_environment.rs:32-66`, and the lent `_bundled_vulkan_driver.py:17-55`
  re-applies the rule; both hard-code `MoltenVK_icd.json`
  (`CR/runtime_unit_layout.rs:30-44`). On Linux the loader is found by bare soname only
  (`CR/vulkan_loader_library.rs:32-36`); the lend-relative search is `cfg(target_os = "macos")`.
- **The unit.** `xtask/src/build_runtime.rs:150-161`, `:182-184` stage and verify the driver
  under `cfg!(target_os = "macos")`; `scripts/stage_macos_bundled_vulkan_driver.sh:22-24` pins
  MoltenVK by tag and SHA-256 and builds the loader from `vulkan-sdk-1.4.357.0`;
  `scripts/build_linux_runtime_release_tarball.sh:60` installs only
  `clang-devel cmake ninja-build pkgconfig` into `manylinux_2_28` (AlmaLinux 8.10, gcc 14,
  no meson, no LLVM; `llvm-static` 21.1.8 and `libdrm-devel` are in its repos). The 0.46.2
  tarball is 19,620,571 bytes. The portability gate checks `NEEDED` only for `_engine`
  (`test_wheel_portability.py:480-497`) and the glibc floor for every ELF (`:561-590`);
  `test_bundled_vulkan_driver.py:99-141` asserts `_vulkan_driver` is absent off macOS.
- **CI.** `python-wheel.yml:211-223`, `:243-282` and `test.yml:886-891` run `-m "not requires_gpu"`
  with no ICD installed; `requires_gpu` is bare at 72 sites in `tests/stream-on-runtime`
  (162 of 301 tests) and defined as "needs a Vulkan device that can allocate exportable DMA-BUF
  memory" (`sdk/streamlib-python-wheel/pyproject.toml:61-66`, `tests/stream-on-runtime/pyproject.toml:50-55`);
  `linux_only_capability(reason=…)` is held to a closed list by
  `sdk/tatolab-stream/tests/test_platform_markers.py:28-77`; `hardware-tests` gates about 276
  `cfg_attr(…, ignore = "<free text>")` sites, 204 of them one boilerplate string.
  `macos-wheel.yml:145-252` already runs `tatolabd` on the lend's driver and asserts the loader
  opened `_vulkan_driver/libvulkan.1.dylib` — the precedent for a lane on the bundled driver.

## MODIFIED: §Graphics `ARCHITECTURE.md:2117-2136` — what a stream gets on the software device

The entry stands, with two clauses replaced:
- "Camera capture takes its MMAP path, display presents through the device's own swapchain,
  …" → "Camera capture takes its MMAP path; a window needs a hardware device — the bundled
  driver carries no window-system code, so `DisplayWindow` refuses by name on the software
  device, and a stream there is viewed from outside the runtime, by its URL or by a node that
  reads its frames; …".
- After "the virtual camera's loopback door takes the host-pointer import tier." add: "A device
  that exports no semaphore — the software device does not — crosses no timeline: a texture
  or staging registration carries none, a processor interpreter waits on none, and every copy
  an escalate op makes on that device is retired on the host before the reply, as on macOS."

## MODIFIED: §Product `ARCHITECTURE.md:91`, `:173` — the test division

> … the three suites run on both floors; on Linux their `requires_gpu` halves run on the lane
> on the unit's own software device, and a test stays on the rig only when it carries
> `rig_only_capability(reason=…)`, whose reason `test_platform_markers.py` holds to a closed
> list — Vulkan Video, DMA-BUF modifiers, CUDA, a camera, a display, an audio device; on macOS
> the `requires_gpu` half runs on that floor's rig until the parity ticket that first makes
> it green …

Replaces "their `requires_gpu` halves on each floor's rig" (`:91`) and "`requires_gpu` halves
stay rig-only" (`:173`); the amendment pointers on both fold.

## MODIFIED: §Distribution `ARCHITECTURE.md:3849-3856` — the portability model

> … On Linux, system libraries (window system, libcuda) are dlopen'd at runtime, never linked.
> A stock machine on either floor may have no Vulkan driver, so the lend carries one in
> `tatolab/runtime/_vulkan_driver/`: on macOS the Vulkan loader, MoltenVK and its unedited ICD
> manifest; on Linux the Vulkan loader and Mesa lavapipe built with LLVM linked in, from one
> Mesa tag pinned by SHA-256, and its unedited ICD manifest — each linking only what the host
> may supply, proven for every native binary the lend carries, not only `_engine`. Engine and
> helper alike dlopen that loader by absolute path, found beside the `_engine` image through
> `dladdr`, after the bare names … and `tatolabd` names the manifest to the loader additively
> through `VK_ADD_DRIVER_FILES`, deferring to a user's `VK_DRIVER_FILES` …

The Linux installer entry (`:3961-3980`) gains the sentence "The tarball carries the Vulkan
loader and the software device in `tatolab/runtime/_vulkan_driver/`, and `install.sh` refuses
a tarball without the driver's manifest"; both pointers fold.

## Tree consequences, by slice

**Slice 1 — the Linux bundle.**
- `scripts/stage_linux_bundled_vulkan_driver.sh`, the sibling of the macOS script: clone Mesa
  at a pinned tag with a pinned SHA-256 of the tarball, build with
  `-Dvulkan-drivers=swrast -Dgallium-drivers=llvmpipe -Dshared-llvm=disabled`, zstd and
  xmlconfig disabled, no libdrm in the build environment (so no DMA-BUF, no sync file, no
  `libdrm` `NEEDED`), `-Dplatforms=` (no window system); build the loader from the same
  `VULKAN_LOADER_TAG` the macOS script uses; write `libvulkan.so.1`, `libvulkan_lvp.so` and
  `lvp_icd.x86_64.json` with `library_path` relative. Runs inside the tarball script's
  manylinux step (meson from pip, `llvm-static` and `ninja-build` from dnf) so the driver
  meets the glibc 2.28 floor; the PR records the measured tarball growth.
- `CR/runtime_unit_layout.rs`: the three file-name constants become per-platform
  (`cfg(target_os)`), `BUNDLED_ICD_MANIFEST_FILE_NAME` = `lvp_icd.x86_64.json` on Linux;
  `_bundled_vulkan_driver.py` reads the manifest name from the platform too.
  `xtask/src/build_runtime.rs:150-184`: one `stage_bundled_vulkan_driver` per platform and one
  `ensure_lend_carries_the_bundled_vulkan_driver`, the `MACOS_` constant and macOS-named
  function deleted. `installer/install.sh:131-137` adds the manifest path to its list.
- `CR/vulkan_loader_library.rs`: a Linux tail after the bare soname — `_vulkan_driver/libvulkan.so.1`
  beside the `_engine` image (`dladdr`) and beside `bin/` for `tatolabd` — logged as the macOS
  tail is.
- Tests: `test_wheel_portability.py` gains
  `test_every_elf_the_lend_carries_links_only_what_the_host_may_supply` (closing the gap that
  checks `_engine` alone); `test_bundled_vulkan_driver.py` gets Linux arms (the three files
  present, `VK_ADD_DRIVER_FILES` naming the lend's manifest); the tarball script's
  `tests_against_the_runtime_unit_lend` run covers both. `linux-runtime-tarball.yml` asserts
  the unit's `tatolabd` starts a stream naming `llvmpipe`, as `macos-wheel.yml:232` asserts
  the MoltenVK loader.

**Slice 2 — the device the engine builds on, and what it offers.**
- Split `HostVulkanDevice::new` (`V/vulkan_device.rs:593-1883`): instance creation once;
  `build_on_physical_device(candidate)` per candidate, returning `Result` with device-only
  teardown (an RAII guard over `VkDevice` so an early `?` leaks nothing); candidates in the
  loader's order re-sorted so `PhysicalDeviceType::CPU` is last; the first `Ok` wins. A
  pass-over is a `VulkanDevicePassedOver { device_name, device_type, reason }`, every one
  logged with the chosen device at `:848-859`; a candidate fails on a missing graphics queue,
  `create_device` refusing a required feature, or a sentinel allocation failing in a flavour
  the device advertises (`allocate_one_sentinel_per_export_pool` keeps its `?`). No
  candidate → the existing `Error::GpuError` naming the loader's guidance.
- `GpuCapabilitiesSnapshot` gains `device_is_software` (type CPU), `supports_dma_buf_export`
  (`VK_EXT_external_memory_dma_buf` + modifiers, and never on a software device) and
  `supports_semaphore_export` (`vkGetPhysicalDeviceExternalSemaphoreProperties`, OPAQUE_FD,
  timeline); its `supports_external_memory` doc stops claiming DMA-BUF. The modifier table
  (`drm_modifier_probe.rs:303`) is taken only when the device exports DMA-BUF.
- DMA-BUF becomes device-gated: `create_dma_buf_pools` and the DMA-BUF external create-infos
  in `HostVulkanTexture::new` and `HostVulkanBuffer` run only on `supports_dma_buf_export`;
  `CROSS_PROCESS_EXPORT_BY_FILE_DESCRIPTOR_EXISTS_ON_THIS_PLATFORM` is deleted. The
  pixel-buffer slot (`V/vulkan_pixel_buffer_pool.rs:14-38`) takes the OPAQUE_FD host-visible
  pool where DMA-BUF is absent; `register_buffer` / `check_in` then carry
  `vk_memory_type_index` and `exporting_device_uuid` as the staging registration does
  (`C/surface_store.rs:814-867`), `handle_check_in` stops hard-coding them `None`
  (`unix_socket_service.rs:936-937`), and `lookup_buffer` admits OPAQUE_FD. The V4L2 arm skips
  its `VIDIOC_EXPBUF` probe on such a device; the PipeWire virtual-camera door refuses by
  name there, the loopback door unchanged.
- The timeline becomes optional: `assign_texture_handle_id`, `assign_image_handle_id` and
  the staging's `refill_done` mint an edge only on `supports_semaphore_export`;
  `register_surface_export_staging` takes `Option`; `refill_device_export_staging`,
  `run_cpu_readback_copy` and `copy_device_export_staging_back_to_surface` wait on the host
  before replying when no edge exists, replying `timeline_value: None`.
- `as_device_tensor` and the tensor capsule refuse before any escalate on a software device,
  naming it (the snapshot reaches the helper through the existing `gpu_capabilities` op);
  `cpu()` and `as_numpy` are untouched.
- Tests: `the_first_device_the_engine_can_build_on_wins_and_a_cpu_type_device_is_last`,
  `a_device_passed_over_is_named_with_its_reason`, `a_software_device_builds_no_dma_buf_pool_and_mints_no_timeline`,
  `a_pixel_buffer_registers_as_opaque_fd_with_its_memory_type_index_where_dma_buf_is_absent`,
  `a_staging_copy_without_a_timeline_is_retired_before_the_reply`,
  `as_device_tensor_refuses_on_a_software_device_naming_it_before_any_staging_opens`; the
  flavour-derivation tests in `SE/acquisition/tests.rs:107-188` gain the software arm;
  `every_exportable_flavour_reacquires_after_its_pool_is_freed_with_a_swapchain_live` asserts
  only the flavours the device offers. Rig: a camera stream on `VK_DRIVER_FILES=lvp_icd.json`
  reaches a Python node's `cpu()`.

**Slice 3 — the processor interpreter on the runtime's device.**
- The spawn host sets `STREAMLIB_RUNTIME_VULKAN_DEVICE_UUID` from the device the runtime built
  on; `ConsumerVulkanDevice::new` becomes `new_on_the_device_the_runtime_named(uuid)` and
  refuses by name a UUID no enumerated device carries, or none given. The loader environment
  needs no new variable: `VK_ADD_DRIVER_FILES` is inherited and re-applied by the lend.
- `REQUIRED_DEVICE_EXTENSIONS` (`CR/consumer_vulkan_device.rs:61-67`) keeps
  `VK_KHR_external_memory_fd`; DMA-BUF, modifiers and `VK_KHR_external_semaphore_fd` become
  probed, and the arms that need one refuse by name without it.
- `W/linux/mod.rs:365-376`: the `dma_buf`-only refusal goes; an `opaque_fd` arm imports a
  single-plane pixel buffer through `from_opaque_fd_at_stated_memory_type_index`, a
  multi-plane one refused by name. `export_staging.rs:566-597` accepts `[staging_fd]` alone and
  skips the wait when the reply's `timeline_value` is `None`.
- Tests: `a_helper_on_a_machine_with_two_devices_imports_on_the_one_the_runtime_named`,
  `an_opaque_fd_pixel_buffer_reads_through_the_cpu_door`,
  `a_staging_without_a_timeline_reads_after_the_reply`; the helper refusal tests
  (`test_video_frame_claim.py:128` and its sibling) stand.

**Slice 4 — CI on the unit's driver.**
- The Linux lanes take the driver from the unit they just built (`python-wheel.yml`'s tarball
  artifact), never from apt: the integration suite runs the whole of
  `tests/stream-on-runtime` minus `rig_only_capability`, and a job runs
  `cargo test --features hardware-tests` for the engine, media-builtins and `tatolabd` with
  `VK_ADD_DRIVER_FILES` naming the staged manifest; the engine-lib allowlist
  (`test.yml:370-670`) stops being the only way an engine test reaches CI.
- `rig_only_capability(reason=…)` joins `linux_only_capability` in `test_platform_markers.py`
  with its closed list; the 72 `requires_gpu` sites are re-read test by test, those needing a
  real camera, display, audio device, Vulkan Video, modifiers or CUDA gaining the marker; the
  `requires_gpu` definitions stop naming DMA-BUF. Rust: one helper,
  `skip_unless_the_device_has(…)` over the snapshot, whose reason is the same closed list, and
  the lane fails on a skip line it cannot name — the shape `test.yml:1081-1095` already uses
  for the macOS adapter tests. The 204 boilerplate `ignore` reasons are left as they are; a
  test that needs a tier says so through the helper, the rest simply run.
- The no-driver fixture keeps working (its variables replace the bundle by the existing rule);
  the lavapipe comments in `python-wheel.yml:211-219` and the two `pyproject.toml` markers go.

## Assumptions stated, not asked

- **CPU-type last, otherwise the loader's order.** The engine re-sorts only to pin the
  software device last, so a loader older than 1.3.204 cannot change the outcome; it never
  prefers discrete over integrated itself. Bites if a user wants the integrated GPU first —
  the loader's `VK_LOADER_DEVICE_SELECT` is the dial.
- **A sentinel failing in an advertised flavour passes the device over.** It is the NVIDIA
  fake-out-of-memory tell today; on a hardware device that fails it, the next device — the
  software one — is taken and the pass-over is named. Bites as a silently slow runtime; the
  log names it, and `VK_DRIVER_FILES` pins a device for diagnosis.
- **OPAQUE_FD pixel buffers are single-plane.** Every pooled frame today is RGBA; a planar
  OPAQUE_FD pixel buffer is refused by name until a producer needs one.
- **The timeline rule keys on the device, not the driver name**: any device whose
  `vkGetPhysicalDeviceExternalSemaphoreProperties` offers no OPAQUE_FD timeline takes the
  host-retired path. Lavapipe is the first; MoltenVK's shared-event path is unchanged.
- **lavapipe's OPAQUE_FD export needs no libdrm** (memfd-backed) — a build fact the slice-1
  ticket proves with the OPAQUE_FD sentinels on the bundled driver; if false, libdrm is built
  in statically, never a `NEEDED`.
- **The size cost is measured, not estimated**: the slice-1 PR records the tarball's growth
  against 19,620,571 bytes.

## Decided in review — a window on the software device (owner, 2026-10-13)

A lavapipe built with window-system support links libxcb, its six extension libraries,
libX11-xcb, libxshmfence and libwayland-client as `NEEDED`. A minimal container or a headless
server has none of them, and then the loader cannot open the bundled driver at all: no device,
every stream refused — in exactly the environments the software device is for. The owner chose
the headless build: the driver links only libc, libm, libstdc++, libgcc_s, libdl and
libpthread and loads anywhere; `DisplayWindow` refuses by name on the software device, pointing
at the vendor driver; offscreen rendering is untouched. Streams are pipelines, and viewing is
the viewer's job — by URL once the sharing step lands, or by a node reading the frames into a
window of its own. The rationale is in the ADR.
Rejected: a desktop-linked build — a window on a driverless desktop, no device in a bare
container (owner, 2026-10-13). Rejected: both builds chosen at start — roughly doubles the
driver bytes (owner, 2026-10-13). A user who wants a window on a GPU-less box points
`VK_DRIVER_FILES` at their distro's own lavapipe, which carries the window-system code.

## Tickets

Milestone #65, *Streams run on a machine without a GPU*. Slice 1: #2753 (the Linux bundle).
Slice 2: #2754 (device selection and the snapshot fields), #2755 (DMA-BUF as a device
capability, the OPAQUE_FD pixel buffer — including the interpreter's import arm and its
probed extensions), #2757 (no timeline on a device that exports none, including the
interpreter's staging check-out; `as_device_tensor` refused on a software device).
Slice 3: #2756 (the interpreter selecting the device the runtime names — selection only,
blocked by #2754 alone). Slice 4: #2758 (the integration lane and `rig_only_capability`),
and #2759 (the engine's hardware tests on the lane). The first two tickets (#2753 and
also #2754) start in parallel; the blocked-by edges are on the issues.

## REMOVED

- REMOVED: MACOS_BUNDLED_VULKAN_DRIVER_STAGING_SCRIPT_RELATIVE_TO_WORKSPACE
- REMOVED: ensure_lend_carries_macos_bundled_vulkan_driver
- REMOVED: test_the_lend_carries_the_driver_beside_the_engine_on_macos_and_none_elsewhere
- REMOVED: CROSS_PROCESS_EXPORT_BY_FILE_DESCRIPTOR_EXISTS_ON_THIS_PLATFORM
- REMOVED: needs a Vulkan device that can allocate exportable DMA-BUF memory
- REMOVED: a software rasterizer (lavapipe) cannot
- REMOVED: path and imports through CUDA, not a CPU mapping
- REMOVED: it carries exactly the staging's memory and its copy timeline
