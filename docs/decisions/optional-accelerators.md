# Optional accelerators: a device always exists, the software one last

Rationale for the `[optional-accelerators]` entries in `docs/plan/ARCHITECTURE.md` §Graphics
and the clauses they amend in §Product and §Distribution, decided by the owner 2026-10-13.
It settles pivot step 5 of `one-runtime-per-machine.md`.

## Trigger

Read this before giving `GpuContext` an `Option`, before adding a CPU path to a built-in
because a machine has no GPU, before making a stream declare that it needs a GPU, before
bundling or swapping the software Vulkan driver, before marking a test rig-only, and when
someone asks why a runtime with no GPU still builds a Vulkan device.

## The decision

1. **The engine never runs without a device.** The Linux runtime unit carries the Vulkan
   loader and Mesa's lavapipe, built with LLVM linked in, beside `tatolabd`, named to the
   loader additively exactly as the macOS unit names MoltenVK. macOS needs nothing: every
   Apple Silicon machine has a GPU behind MoltenVK.
2. **The runtime starts with no device built; the first stream to start builds the one
   `GpuContext`.** This was already the tree's shape after runtime hosting. What changes is
   which device: the first the engine can build on in the loader's order (discrete,
   integrated, virtual, CPU-type), so hardware always wins and the software device is taken
   only when nothing else builds. A device that cannot be built on is passed over and named.
   The loader's own selection variables are the override; the engine adds no dial of its own.
3. **A stream never declares that it needs a GPU.** Every stream starts; a node that asks for
   a tier the device lacks is refused by name at that node, as on any device.
4. **The software device gets no DMA-BUF flavour, by rule.** Its frames cross to processor
   interpreters as OPAQUE_FD; the host-pointer import tier serves the virtual camera.
5. **A processor interpreter imports on the runtime's device**, handed the device and the
   loader environment at spawn.
6. **The Linux CI lane runs the engine on the software device.** Rig-only narrows to what a
   runner cannot give: Vulkan Video, DMA-BUF modifiers, CUDA, a real camera, display or audio
   device.
7. **No window on the software device** (owner, 2026-10-13, resolving the change proposal's
   fork). The bundled lavapipe is built without window-system code so it links only what any
   Linux has and loads in a bare container; `DisplayWindow` refuses by name on the software
   device, offscreen rendering is untouched, and a stream there is viewed from outside the
   runtime — by its URL once the sharing step lands, or by a node reading its frames into a
   window of its own. The owner's framing: streams are pipelines; a window is the viewer's,
   not the stream's.
8. **The adapter crates stay where they are.** The earlier "do they move" item had no driver
   behind it: nothing about optional accelerators touches where an in-process Rust crate
   lives, and only the CUDA adapter ships at all, compiled into the runtime portion.

## Why

Checked against the tree on 2026-10-13:

- The runtime already serves its API with no Vulkan driver present, and refuses each stream
  start by name when the device cannot be built. What stood between "a stream that needs no
  GPU" and running was not startup but the data path: a pixel buffer is always a Vulkan
  allocation, the node context carries the GPU capability unconditionally, and a processor
  interpreter reads a frame through a Vulkan import of its own. Audio, Opus, the MP4 sink
  and pure-Python nodes need no GPU themselves and still could not start.
- Making a device always exist keeps one path through all of that. The alternative, a
  device-less mode, is CPU pixel buffers plus a CPU arm in capture, display, the virtual
  camera, the codecs and every exchange door, each a second path beside the GPU one.
- Lavapipe over SwiftShader, on what each lacks for us: SwiftShader caps its heap at 2 GiB
  and an allocation at 1 GiB, has no memory-budget extension (the budget gate would run on
  its estimate), cannot export a timeline semaphore (the Linux cross-process ordering rides
  one), has no ray tracing, no tagged releases, and Chromium is winding down its own use of
  it. Lavapipe reports system memory as its heap with the budget extension, exports opaque
  fds, imports host pointers, exports semaphores, and is the driver Mesa and wgpu run their
  own GPU-less CI on. Both are permissively licensed (MIT; Apache-2.0 with the LLVM
  exception).
- The cost is download size, which the owner accepted: the Linux unit is about 19 MB
  gzipped today; a static-LLVM lavapipe is estimated at 12 to 15 MB gzipped more (one
  measured datapoint, a 45 MB static Windows build; distro builds are smaller because they
  lean on a 137 MB shared LLVM). The ticket that bundles it measures the real number.
- DMA-BUF export on lavapipe exists only when the process can open `/dev/udmabuf`, which
  the default udev rule gives to the `kvm` group. A flavour whose presence depends on a
  container's device permissions is a flavour that breaks in exactly the environments the
  software device is for, so it is ruled absent rather than probed.
- ~~Lavapipe cannot start the engine today because the device-wide pre-warm allocates one
  sentinel per export pool and fails the device when any sentinel fails~~ — Superseded
  2026-10-13 by a measurement with the engine's own test binary on the rig: the device comes
  up and every sentinel passes; the first real tiled allocation then refuses, because the
  tiled DMA-BUF pool's modifier list comes from an EGL probe the Vulkan driver does not
  honour. Under decision 2 a sentinel failing in a flavour the device advertises passes that
  device over; under decision 4 the software device offers no DMA-BUF pool to pre-warm.

## Rejected alternatives

- **A device-less engine mode.** Two paths forever through every surface door and every
  built-in; the owner preferred keeping the code the same on 2026-09-26 and reaffirmed it.
- **SwiftShader as the software device.** Four megabytes, but the gaps above are structural
  for this engine, not cosmetic.
- **Lavapipe against the system's shared LLVM.** Nothing guarantees a GPU-less machine or a
  container has one, and a 137 MB dependency the installer does not control is not a bundle.
- **A stream declaring that it needs a GPU.** A declaration the graph would carry for the
  runtime to refuse at load; the per-node refusal already exists and is finer.
- **A warning when GPU-heavy work runs on the software device.** The device is named once
  at the first stream start; a per-node speed warning would be a judgement the engine cannot
  make for a stream it does not know.
- **An engine dial for device selection.** The Vulkan loader and Mesa already ship one.
- **A lavapipe built with window-system support.** It links libxcb, its extension libraries
  and the Wayland client, which a minimal container lacks, so the driver would fail to load
  exactly where the software device is needed; a window on a driverless desktop is rare and
  has a named remedy. **Both builds, chosen at start** was rejected with it as roughly
  doubling the driver bytes.
- **A separate accelerated distribution.** Already rejected by `one-runtime-per-machine.md`:
  an extra cannot change the runtime's native module.

## Consequences

- The Linux unit roughly doubles in download size. The real figure is measured when the
  bundle lands, and a static-LLVM Mesa build joins the manylinux build.
- The device-wide pre-warm becomes per device: a sentinel failure passes the device over
  instead of failing the start, and the runtime's log carries the device it built on and
  every device it passed over, with reasons.
- The pixel-buffer pool gains the OPAQUE_FD flavour on Linux where DMA-BUF is absent, and
  the processor interpreter's pixel-buffer import gains the OPAQUE_FD arm it already has for
  textures and storage buffers.
- ~~Whether lavapipe exports a timeline semaphore as an opaque fd is unverified.~~ —
  Settled 2026-10-13, measured: lavapipe exports no semaphore as an opaque fd, binary or
  timeline (its sync type is a userspace mutex and condvar; only a pre-signalled sync file
  exists, and only with `/dev/udmabuf`). So the device-write ordering on the software device
  takes the macOS shape, retiring the copy on the host before the reply, rather than a second
  ordering mechanism.
- A lavapipe built with window-system support links the X11 and Wayland client libraries,
  which a minimal container lacks; the change proposal carries that fork.
- The headless and container cases fall out: lavapipe needs no window system and no
  `/dev/dri`, so a stream with no display window runs on a server or in a plain container.
  A display window still needs X11 or Wayland to put it on.
- The Linux CI lane can start the engine. Which tests move off the rig is settled by the
  rule in decision 6, test by test, as the bundle lands.
- A Raspberry Pi is one step closer, not there: the Linux unit is x86_64-only, and an
  arm64 unit is a distribution change of its own. On a Pi the hardware Mesa driver would be
  taken first under decision 2 and lavapipe sit behind it.
- The memory-budget entry already applies to any device; on the software device it reads
  the system-memory heap through the extension, and a small machine is refused frames by
  name before it swaps.
- §Product's sentence keeps "with an NVIDIA GPU" as the first-minute experience it
  describes; the amendment pointer records that the GPU is optional.
