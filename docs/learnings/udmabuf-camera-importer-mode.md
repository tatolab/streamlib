# udmabuf permissions for V4L2_MEMORY_DMABUF importer mode

## When you need this

You're running a streamlib camera path with
`STREAMLIB_CAMERA_USE_IMPORTER_DMA_BUF=1` (Path C in
`libs/streamlib/src/linux/processors/camera.rs`), or any other code
in tree that calls `udmabuf_alloc()` to produce a kernel dma_buf for
V4L2 + Vulkan dual-import. The path requires `/dev/udmabuf` to be
opened with read+write by the running process; without it, Path C's
probe fails immediately and the camera processor falls through to
Path A or B.

Default Linux distros ship `/dev/udmabuf` as `0660 root:root` (or in
some packaging configurations `root:kvm` — Ubuntu 25.10 + KVM stack
moves it there at boot). No interactive user has access to it
out-of-the-box.

## Why udmabuf instead of GPU export

uvcvideo (and every USB UVC driver in mainline) frame-fills via
`memcpy(kvaddr_of_dmabuf, urb->transfer_buffer, len)` inside its
URB completion handler. The `kvaddr_of_dmabuf` is obtained by
calling `dma_buf_vmap()` on the imported buffer at QBUF time. Any
dma_buf whose exporter does not implement `dma_buf_ops::vmap` will
fail the import, and uvcvideo never delivers DQBUF events on those
slots.

NVIDIA's proprietary GPU driver (open-gpu-kernel-modules `nv-dmabuf.c`,
through at least driver 570) does not implement `.vmap` on its
exported dma_bufs. So a Vulkan-allocated VkBuffer + DMA-BUF export
+ `VIDIOC_QBUF(memory=V4L2_MEMORY_DMABUF, m.fd=...)` syscall sequence
parses cleanly at REQBUFS+QBUF+STREAMON, but the per-slot vmap fails
silently and DQBUF never fires.

To the best of our current knowledge as of 2026-05-04, this rules
out "GPU allocates, V4L2 imports" for USB UVC on NVIDIA Linux.

> ~~The working shape is "userspace allocates a dma_buf via udmabuf
> (or `/dev/dma_heap/system`), V4L2 AND Vulkan both import the same
> FD."~~ — **Superseded 2026-05-04** during issue #687 E2E
> validation. Inverting the producer (userspace-allocates) unblocks
> the V4L2 side but **NVIDIA's proprietary Vulkan driver rejects
> the import on the GPU side too**. See
> [Status on NVIDIA proprietary driver](#status-on-nvidia-proprietary-driver-2026-05-04)
> below — the dual-import shape works on Mesa drivers but does NOT
> work on NVIDIA's proprietary stack on consumer/desktop RTX. On
> NVIDIA proprietary Linux, the only working shape today remains
> Path A (V4L2 MMAP + userspace memcpy into a Vulkan-allocated
> buffer). The kernel does the URB→buffer memcpy as before;
> userspace pays one memcpy per frame.

Any future driver release that adds `.vmap` to NVIDIA's dma_buf ops
would unlock the original "GPU allocates" shape — verify
empirically before relying on it. (As of the 570→595 changelog
scan documented below, no NVIDIA release through 595.71.05 has
shipped this fix.)

## Status on NVIDIA proprietary driver (2026-05-04)

The userspace-allocates shape was empirically validated against
NVIDIA proprietary driver 570.211.01 (open kernel-module variant)
on RTX 3090, kernel 6.17, Ubuntu 24.04 during issue #687. **The
GPU-side import fails identically for both producers** (`/dev/udmabuf`
and `/dev/dma_heap/system`), at the same line of code, with the
same `VK_ERROR_OUT_OF_DEVICE_MEMORY` from `vkAllocateMemory` chained
with `VkImportMemoryFdInfoKHR{handleType=DMA_BUF_EXT}`. The
producer-side smoke tests pass for both — the FDs are valid
dma_bufs that V4L2 accepts. Only the Vulkan import side trips.

### What we tested and what we found

1. **Producer-side smoke tests pass for both producers.**
   `udmabuf_alloc_smoke` and `dma_heap_alloc_smoke` (both `#[ignore]`d
   tests in `camera.rs`) confirm the kernel-side dma_buf is real
   and `lseek(SEEK_END)` reports the correct page-aligned size.
2. **V4L2 capability probe passes.** REQBUFS(DMABUF, count=4) returns
   `count=4` with `V4L2_BUF_CAP_SUPPORTS_DMABUF` set in capabilities.
   uvcvideo is happy to import these FDs.
3. **`vkGetMemoryFdPropertiesKHR(fd, DMA_BUF_EXT)` returns
   `VK_ERROR_UNKNOWN`** on the imported FD (live diagnostic
   captured in `vulkan_pixel_buffer.rs::import_single_plane`
   during #687 — driver does not recognize this FD as a valid
   external-memory import source). Buffer-side `memory_type_bits`
   was a healthy `0x1B` (types 0/1/3/4 including HOST_VISIBLE);
   the rejection is upstream of any memory-type negotiation.
4. **`vkAllocateMemory` with `VK_EXT_external_memory_host`
   (mmap-the-FD-then-import) also fails** with
   `VK_ERROR_INVALID_EXTERNAL_HANDLE`. A 5-way empirical probe
   (`/tmp/probe-host-anon.c` from the #687 research) shows NVIDIA
   accepts `posix_memalign`, `MAP_ANONYMOUS|MAP_PRIVATE`, and
   `MAP_ANONYMOUS|MAP_SHARED` cleanly through the same import path,
   but rejects any pointer whose backing VMA's `vm_file` is a
   dma_buf inode. `mlock` does not change the verdict — it's a
   VMA-source filter, not a pinnability check. The spec
   (`VK_EXT_external_memory_host` issue #1) explicitly permits
   this implementation discretion, so it isn't a bug per the spec.

### Why this happens

NVIDIA's import path through `nvidia-drm` PRIME calls into
`nvkms->getSystemMemoryHandleFromDmaBuf` (closed-source NVKMS blob
referenced from `nvidia-drm/nvidia-drm-gem-dma-buf.c::nv_drm_gem_prime_import_sg_table`).
That function does the producer-recognition decision and rejects
anything other than a small set of supported foreign producers.
cubanismo (NVIDIA / Khronos) on
[`open-gpu-kernel-modules` discussion #243](https://github.com/NVIDIA/open-gpu-kernel-modules/discussions/243)
(Oct 2024): *"we've supported importing 'foreign' dma-bufs for
several releases now via EGL and Vulkan… through the kernel's
PRIME helpers by way of nvidia-drm"* — but the supported set is
*FDs originated by another DRM driver*, not userspace producers
like udmabuf or dma_heap/system. An earlier (May 2022) statement
from the same NVIDIA engineer calls it *"a software limitation,
not a hardware one."*

### What we ruled out as workarounds

- **Producer swap** (`/dev/udmabuf` ↔ `/dev/dma_heap/system`): both
  fail identically. Producer choice doesn't change the verdict.
- **Memory-type-selection fix** (`vkGetMemoryFdPropertiesKHR` +
  intersect with buffer's `memoryTypeBits`): probe call itself
  fails on these FDs; the spec-correct approach can't even start.
  Also breaks an existing Vulkan-export round trip on NVIDIA where
  the original code worked accidentally (NVIDIA's FD probe returns
  DEVICE_LOCAL-only bits while its allocator accepts HOST_VISIBLE
  in practice — a documented NVIDIA spec deviation).
- **`VK_EXT_external_memory_host` (mmap-then-import)**: empirically
  rejected by NVIDIA's VMA-source filter (probe results above).
- **`/dev/dma_heap/cma`** (contiguous-allocator backing): not
  available on this kernel without `cma=128M` boot parameter +
  permanent kernel memory tax. Even if tested, the rejection
  lives inside the closed NVKMS blob; CMA changes the *backing*
  but not the FD's `dma_buf_ops` lineage that NVKMS recognizes.
- **Driver upgrade (570 → 595)**: no relevant changelog entries
  across 570.211.01 → 595.71.05. The only DMA-BUF additions are
  on the *export* side (DRM modifiers for YCbCr); foreign-import
  producer set is unchanged. Worse, 595 regressed an adjacent
  foreign-import path (PipeWire/KWin portal screencast — bazzite
  issue #4345).

### What does work, today

- **Path A (V4L2 MMAP + userspace memcpy)**: the existing default.
  ~3 MB × 60 fps = ~180 MB/s memcpy at 1080p60. Works on every
  driver. This is what the camera processor falls back to when
  Path C is not opted in.
- **Mesa NVK on the same NVIDIA hardware**: Mesa's open-source
  Nouveau-based Vulkan driver is conformant Vulkan 1.3/1.4 on
  Turing/Ampere/Ada and accepts foreign dma_bufs uniformly per
  Mesa convention. A user-side ICD swap
  (`VK_ICD_FILENAMES=/usr/share/vulkan/icd.d/nouveau_icd.x86_64.json`,
  install `mesa-vulkan-drivers`) would route streamlib's Vulkan
  through NVK instead of NVIDIA proprietary, and Path C should
  work — *not yet empirically validated by streamlib's E2E*, but
  architecturally supported. Filed as the unblocking validation
  step for #687.
- **Mesa drivers on Intel/AMD hardware** (anv, radv): same
  architectural reasoning, same status — not yet validated by
  streamlib's E2E.

### Adjacent precedent

- [Looking Glass](https://looking-glass.io/) — a VM-shared-memory
  framework — wrote *a custom kernel module* (KVMFR) rather than
  use udmabuf, because no userspace dma_buf producer worked with
  NVIDIA proprietary on the host side. That's an extreme effort
  and a strong signal about the constraint's durability.
- [obs-kmsgrab](https://github.com/w23/obs-kmsgrab) (OBS Studio
  DMA-BUF screen capture) explicitly disclaims NVIDIA support for
  the same root cause.
- [NVIDIA forum thread "Problem with udmabuf imported EGLImages"
  (Aug 2022)](https://forums.developer.nvidia.com/t/problem-with-udmabuf-imported-eglimages-and-zwp-linux-dmabuf-v1/225687)
  — user attempted the udmabuf path, got `Failed to lookup NVKMS
  gem object for export: 0x00000001`. Zero NVIDIA reply.
  Unresolved 4 years later.

### Unblocking criteria

Before promoting Path C from "wired but opt-in" to a default-on
shipping path, one of:

1. **NVIDIA driver release** that broadens
   `getSystemMemoryHandleFromDmaBuf` to accept system-memory
   dma_buf producers (udmabuf, dma_heap/system). Watch
   `open-gpu-kernel-modules` releases and NVIDIA Linux driver
   release notes. The reversal is a one-line removal of the gate
   at the call site (capability check on `vendor_id == 0x10DE`).
2. **Mesa empirical validation** — Path C E2E on Intel iris, AMD
   radeonsi, OR Mesa NVK on RTX, with the
   [`docs/testing.md` standardized E2E template](../testing.md)
   filled in (Cam Link content visible in PNG samples).
   This unblocks Path C as a Mesa-side feature with NVIDIA gated
   off.
3. **streamlib pivots to NVIDIA-native APIs** — CUDA's
   `cudaImportExternalMemory(OPAQUE_FD)` from a Vulkan-allocated
   `VkBuffer` (the path streamlib's #588 already established for
   the cuda surface adapter), or NVIDIA-allocated GBM buffers
   used as PRIME source. Different architecture from #687; would
   be filed as its own issue.

## Permanent host setup (workstation)

1. **Install the udev rule.** Drop the file below in
   `/etc/udev/rules.d/`:

   ```
   # /etc/udev/rules.d/99-streamlib-udmabuf.rules
   #
   # Grant the `render` group read+write on /dev/udmabuf so the streamlib
   # V4L2_MEMORY_DMABUF importer-mode camera path can allocate kernel
   # dma_bufs without root privileges.
   KERNEL=="udmabuf", GROUP="render", MODE="0660"
   ```

   `render` is the canonical Linux GPU-render-node group; if a user
   already has `/dev/dri/render*` access for Mesa, adding udmabuf
   here is a natural fit. `video` works equivalently — pick whichever
   matches the rest of the deployment's GPU group conventions.

2. **Reload udev and re-trigger** so the new rule takes effect
   without a reboot:

   ```bash
   sudo udevadm control --reload-rules
   sudo udevadm trigger --subsystem-match=misc --attr-match=KERNEL=udmabuf
   # or simpler: sudo udevadm trigger
   ```

3. **Add the running user to the `render` group** (if not already):

   ```bash
   sudo usermod -aG render $USER
   ```

   The new group membership applies on next login. To activate it in
   the current shell: `newgrp render`.

4. **Verify.**

   ```bash
   ls -l /dev/udmabuf
   # crw-rw---- 1 root render ... /dev/udmabuf

   # In a render-group shell:
   test -r /dev/udmabuf && test -w /dev/udmabuf && echo OK || echo NOT_OK
   ```

5. **Run the streamlib unit test that locks the wire shape:**

   ```bash
   cargo test -p streamlib udmabuf_alloc_smoke -- --ignored --nocapture
   ```

## Container setup

`/dev/udmabuf` is a kernel-side device node — it exists only on the
host kernel. Containers see it only through an explicit device
bind-mount.

### Docker / Podman

```bash
docker run \
  --device /dev/udmabuf:/dev/udmabuf:rw \
  --device /dev/video0:/dev/video0:rw \
  --device /dev/dri:/dev/dri:rw \
  --group-add $(getent group render | cut -d: -f3) \
  -e STREAMLIB_CAMERA_USE_IMPORTER_DMA_BUF=1 \
  -e STREAMLIB_CAMERA_DEVICE=/dev/video0 \
  <image> ...
```

Notes:

- `--device /dev/udmabuf:/dev/udmabuf:rw` binds the host kernel's
  udmabuf device into the container's `/dev` tree. Without this,
  `open(/dev/udmabuf)` returns `ENOENT` inside the container even
  if the container's runtime is privileged.
- `--group-add` makes the container's PID-1 process a member of the
  host `render` gid — required for the udev rule's `GROUP="render"`
  permission to apply. Without it, root-inside-container still works,
  but a non-root in-container user does not. To the best of our
  current knowledge, Docker resolves the gid against the host's
  `/etc/group` at startup, not against the container's image — `getent
  group render` on the host is the right source.
- `--device /dev/dri:/dev/dri:rw` is required for any GPU work
  (Vulkan device + import path); not specific to udmabuf, but the
  V4L2_MEMORY_DMABUF flow needs both halves available.
- `--device /dev/video0:/dev/video0:rw` is the camera; replace with
  whichever V4L2 node the test targets.

### Kubernetes / podman-compose

Wrap the same flags. With Kubernetes, expose `/dev/udmabuf` via a
`HostPath` volume + `securityContext.runAsGroup` matching the host
render gid; pair with the `device-plugins` operator if running on a
managed cluster.

### Privileged containers

`--privileged` makes all host devices available, which sidesteps the
explicit `--device` flags. **Don't use** for production camera-capture
workloads — it's strictly broader than needed and makes the trust
boundary impossible to audit. Test rigs that already have a
privileged trust model can use it as a shortcut, but the explicit
`--device` recipe should be the default in any committed compose /
manifest.

### CI runners

GitHub Actions hosted runners do not expose `/dev/udmabuf` to user
workflows (nor do they advertise USB devices), so the
`udmabuf_alloc_smoke` and `v4l2_dma_buf_importer_capability_probe`
tests cannot run in hosted CI. Self-hosted runners with appropriate
udev rules can. Mark CI gating accordingly when wiring this up under
a future *GPU CI Runners* milestone.

## Verification commands

The cheapest authoritative probe is one ioctl:

```c
/* probe-udmabuf.c — compiles to ~30 lines */
#include <fcntl.h>
#include <stdio.h>
#include <sys/ioctl.h>
#include <sys/syscall.h>
#include <sys/mman.h>
#include <unistd.h>

struct udmabuf_create { unsigned memfd, flags; unsigned long offset, size; };
#define UDMABUF_CREATE_IOCTL 0x40187542UL

int main() {
    int memfd = syscall(SYS_memfd_create, "probe", MFD_CLOEXEC | MFD_ALLOW_SEALING);
    if (memfd < 0) { perror("memfd_create"); return 1; }
    if (ftruncate(memfd, 4096) != 0) { perror("ftruncate"); return 1; }
    if (fcntl(memfd, F_ADD_SEALS, F_SEAL_SHRINK) != 0) { perror("F_SEAL_SHRINK"); return 1; }
    int udma = open("/dev/udmabuf", O_RDWR | O_CLOEXEC);
    if (udma < 0) { perror("open(/dev/udmabuf)"); return 2; }
    struct udmabuf_create c = { .memfd = memfd, .flags = 0, .offset = 0, .size = 4096 };
    int dmabuf = ioctl(udma, UDMABUF_CREATE_IOCTL, &c);
    if (dmabuf < 0) { perror("UDMABUF_CREATE"); return 3; }
    printf("udmabuf OK — fd=%d\n", dmabuf);
    return 0;
}
```

`cc -O2 probe-udmabuf.c -o /tmp/probe-udmabuf && /tmp/probe-udmabuf`
should print `udmabuf OK` on a correctly-configured host or container.

## Reference

- udev rule shape: standard Linux device-node access pattern (see
  `udev(7)`).
- udmabuf UAPI: `include/uapi/linux/udmabuf.h` in the kernel tree;
  driver source at `drivers/dma-buf/udmabuf.c`.
- Why udmabuf works where Vulkan-export doesn't: the
  `videobuf2-vmalloc.c::vb2_vmalloc_map_dmabuf` →
  `dma_buf_vmap_unlocked` chain, vs NVIDIA's `nv_dma_buf_ops` not
  implementing `.vmap`. See @docs/learnings/cross-process-vkimage-layout.md
  for the broader Vulkan-side export-vs-import model.
- Implementation: `udmabuf_alloc` and `try_setup_dma_buf_importer` in
  `libs/streamlib/src/linux/processors/camera.rs`.
- E2E gating: the `udmabuf_alloc_smoke` and
  `v4l2_dma_buf_importer_capability_probe` `#[ignore]`d unit tests
  in the same file.
- Issue: #687 (V4L2_MEMORY_DMABUF importer mode for direct
  camera→GPU memory).
