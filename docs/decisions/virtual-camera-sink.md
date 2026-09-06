# The virtual camera is a built-in, on v4l2loopback

Rationale for the `[virtual-camera-sink]` entries in `docs/plan/ARCHITECTURE.md`
§Media I/O and the clause it added to the built-in criterion in §Packages & extension
model, decided by the owner 2026-09-06.

## Trigger

Read this before proposing the virtual camera as an extension wheel, before proposing a
PipeWire backend for it, before adding a second write path beside the GPU pass, or when
someone asks why a sink that meets neither the deadline test nor the engine-primitive
test ships inside the wheel.

## Decision

1. **A built-in, not an extension wheel.** `VirtualCameraSink` ships inside the wheel
   beside `DisplayWindow`, in the media built-ins crate, and reaches Python as a marker
   class through the five touchpoints every built-in owns.
2. **The criterion gained a clause rather than an exception.** A first-party capability
   that presents an OS-facing device to other applications on the machine, which
   `pip install streamlib` alone must make available, is a built-in. The virtual camera
   is the first case; a virtual microphone would be the same case.
3. **v4l2loopback is a door, created per processor.** The sink adds its own loopback
   device through the module's control node at `setup()` and removes it at `teardown()`:
   memory-mapped output streaming, YUYV, format from the first frame, the frame's
   monotonic stamp passed through. Every application that enumerates `/dev/video*` sees
   it as a camera, and portal-based ones see it through the session manager's V4L2 mirror.
   (Widened the same day from the original loopback-only, pre-loaded-device shape; see the
   two later sections.)
4. **No elevation, ever.** The engine never loads the module, never writes a udev rule,
   and never asks for privilege. Without the one-time system step the sink takes the
   PipeWire door; only `door = "v4l2loopback"` refuses at `setup()` by name, carrying the
   two lines, and the runtime keeps running.
5. **One GPU pass writes the device.** The conversion kernel targets the mapped loopback
   buffer through a host-pointer import the RHI gains. Where the driver refuses the
   import, the same kernel targets cached host staging and one copy lands it. The
   experiment on the platform floor decides which branch is the default.
6. **The PipeWire door is OPEN, not dead.** A camera-role PipeWire node is held until a
   consumer that only looks there appears.

## Why

**Built-in over extension.** The extension route needed three things the tree does not
have: a DMA-BUF export that carries the modifier and plane layout to Python, a separate
maturin package, and a live-add path in the control plane, which was removed on
2026-08-08 and is itself pending reversal. The engine already holds the two hard parts,
the modifier-aware DMA-BUF allocation and the host readback path. The owner's words: "the
easiest for everything, no dynamic module loading and such needed."

**A clause over an exception.** The reasoning was general, not about this one sink. An
OS-facing device that a fresh install must offer is a shape the next capability of its
kind will meet again, and a criterion that names it is contestable; a one-off exception
is not.

**v4l2loopback over PipeWire, decided by a probe.** On 2026-09-06 a camera-role
`Video/Source` node registered from GStreamer landed in the portal-exposed set beside the
vivid and Cam Link V4L2 nodes: WirePlumber's portal script grants camera clients every
node with `media.class = Video/Source` and `media.role = Camera`, with no V4L2 or
libcamera restriction. But Chrome 152 on the rig ships the PipeWire camera flag off, and
Firefox uses PipeWire cameras by default only on distributions that flipped the pref
(Fedora 41 onward). "Treat it like a USB camera in every app" is only true of V4L2.
v4l2loopback has no DMA-BUF import (the upstream request has stood open since 2022), so
the frame must land in host memory once; PipeWire would have been zero-copy, which is why
the door stays OPEN rather than closed.

**The loopback module's own shape sets the write path.** From its source: one
`vmalloc` pool mapped page by page; the output side accepts memory-mapped streaming
only, no user pointers, no DMA-BUF; `write()` is a `copy_from_user`; readers map the same
pool, so the reader side is already zero-copy; a queued buffer keeps a supplied timestamp
under `V4L2_BUF_FLAG_TIMESTAMP_COPY`. So the whole cost is one landing from the GPU into
the mapped pool. The rig's driver reports `VK_EXT_external_memory_host` with a 4 KiB
alignment and the loopback maps whole pages, which is what makes the import branch
plausible; whether the driver pins a range mapped from a character device is the one
unknown, and it is an afternoon on the rig.

**Cached staging in the fallback, never write-combined.** Reading write-combined memory
with `memcpy` is what held decode at 37 ms a frame before the staging fix. The fallback
branch reads staging once, so the staging must be cached.

**YUYV.** Half the bytes of RGBA over the bus, and the format every V4L2 consumer
negotiates first; the writer sets the device format, so the device advertises exactly
that.

**No linked library.** The `v4l` crate speaks ioctls without `libv4l2` unless its
optional feature is on, so the sink adds nothing to `DT_NEEDED` and the portability gate
stays as it is, the same discipline the audio arm keeps by `dlsym`.

## Verified on the rig, 2026-09-06

- v4l2loopback 0.15.3 installed under the running kernel, not loaded; loading needs
  `sudo modprobe v4l2loopback exclusive_caps=1`.
- Cameras: vivid on `/dev/video0`, Cam Link 4K on `/dev/video1` and `/dev/video2`.
- PipeWire 1.0.5 with WirePlumber and the GNOME portal backend running; the portal's
  Camera interface reports a camera present.
- NVIDIA RTX 3090, `VK_EXT_external_memory_host` present,
  `minImportedHostPointerAlignment = 0x1000`.
- The RHI has no host-pointer import today; the readback path is a persistent-mapped
  host staging buffer with timeline-semaphore tickets, single in flight per handle.

## Not decided here

The example that proves it and the ticket split are the change's business. The outcome
of the import experiment is the first ticket's first step, not a plan question.

## The RHI primitive is one abstraction with a tier inside

Added by the `virtual-camera-sink` change, 2026-09-06. The GPU has to land YUYV into a host
range the loopback module mapped for us, and there are two ways to do that: import the range
itself through `VK_EXT_external_memory_host` and let the conversion kernel write it, or write
host-cached staging and copy once. They are one concern with a capability tier inside it, not
two APIs, for the same reason the engine's opaque-fd pool has a host-cached tier that degrades
to the write-combined pool with a warning rather than exposing a second pool to callers. A
sink that had to choose between two primitives would carry the platform's quirk into a
built-in that should only know "give me a buffer the kernel can write and make it visible to
the host". The staged tier is host-cached and never the sequential-write allocation the
storage-buffer path hands out, because reading write-combined memory with `memcpy` is the
exact 37 ms trap the decode path paid before its staging fix. Which tier a driver takes is a
fact about that driver, logged once per sink; the first ticket measures it on the platform
floor and the plan entry records the answer at fold time.

## The modprobe line the message prescribes

> ~~`sudo modprobe v4l2loopback exclusive_caps=1 max_buffers=4 card_label="StreamLib Virtual Camera"`.~~
> — Superseded 2026-09-06, the same day, by the per-processor ruling: the module loads with
> `devices=0` and each sink creates its own device with the label, capability announcement
> and buffer count set per device through the control node; the two lines a user runs once
> are `sudo modprobe v4l2loopback devices=0` (persisted in `modules-load.d` and
> `modprobe.d`) and a udev rule `KERNEL=="v4l2loopback", SUBSYSTEM=="misc", TAG+="uaccess"`.
> The reasoning below for `exclusive_caps` and four buffers still holds; both are now
> per-device fields of the `CTL_ADD` configuration rather than module parameters.


`exclusive_caps=1` because Chromium's V4L2 enumerator lists a node only when it reports
`VIDEO_CAPTURE` and not `VIDEO_OUTPUT`; without the parameter a loopback node reports both
and Chrome never shows it. In 0.15.x the capabilities are computed per opener — OUTPUT to the
writer holding the stream, CAPTURE-only to everyone else — so the writer is unaffected; the
folklore that the parameter breaks producers is pre-0.13 behaviour. `max_buffers=4` because
Chrome asks for four buffers and the module clamps a request to the parameter, whose default
is two. The label is what readers show in their camera pickers. The line is the user's to
run, once, and the engine never runs it; a `modules-load.d` entry and a `modprobe.d` options
file are how it survives a reboot, and the refusal message says so.

## Two doors, one open at a time, and as many cameras as the graph adds

> ~~N instances then need N loopback devices for the loopback door (`devices=N` and a
> `card_label` list on the module line) … `name` is also how an instance finds its device
> among several, since the module fixes each device's label at load time and the sink can
> only choose, never rename.~~ — Superseded 2026-09-06, the same day, by the owner's
> per-processor ruling below: the sink creates its own device, so nothing is pre-loaded,
> selected, or listed on the module line.

Owner ruling, later on 2026-09-06, widening the loopback-only decision above: the sink has
both doors, and the graph adds as many sinks as it wants, each a camera of its own with its
own `name`. What settled the shape between the doors was one observation from the probe:
the session manager mirrors every V4L2 capture device into the portal's camera set, so a
loopback device is already visible to portal-based applications as well as to `/dev/video*`
ones. The loopback door is therefore the widest, and the PipeWire door is not a
compatibility widening but the door that needs no module and no root — what a fresh install
gets — and the one that is zero-copy. Opening both for one sink would list the same camera
twice in every portal-aware picker, so the rule is one door per instance, chosen at
`setup()`: a free loopback device if one exists, else the PipeWire node. N instances then
need N loopback devices for the loopback door (`devices=N` and a `card_label` list on the
module line), and any instance past the last free device takes the PipeWire door and says
so. `name` is the one thing a user must be able to set: it is what every picker shows, and
on the loopback door it is also how an instance finds its device among several, since the
module fixes each device's label at load time and the sink can only choose, never rename.

## A device per processor, created and removed by the sink

Owner ruling, 2026-09-06: "This is a new device per processor every time, it gets shut down
when the runtime ends … more like a USB camera I'm sticking in, from other applications'
perspective." That is exactly the shape the loopback module offers through its control
node: `/dev/v4l2loopback` takes `CTL_ADD` with a `v4l2_loopback_config` — the label, the
capability announcement, the buffer count — and answers with the device number; `CTL_REMOVE`
takes it away. No capability check sits behind the ioctls beyond opening the node, and the
module loads with `devices=0` so nothing exists until a sink asks. The PipeWire door is the
same shape by construction: a node lives exactly as long as the stream that registered it.

What stands between a fresh install and the loopback door is the control node's mode. The
module registers it as a misc device without a mode, so the kernel's default is root-only.
The one-time step is therefore not `modprobe` with devices but two lines the user runs once:
load the module with no devices, and a udev rule tagging the node `uaccess`, which lets the
logged-in seat user open it through the ACL logind manages — no group membership, no
elevation at run time. The sink never runs either line; it reports them, and it takes the
PipeWire door in the meantime. `door = "v4l2loopback"` is for the user who wants the
loopback door or nothing, and it refuses by name with those two lines.

Persistence is the one place the model is "ideally": a device a sink created outlives a
crash, and `CTL_REMOVE` returns `EBUSY` while any reader holds the device open, so a camera
still open in another application at teardown stays until that application lets go. The
sink therefore removes at teardown best-effort, and at the next `setup()` reclaims a device
that carries its own label rather than creating a second one. Nothing about the engine's
camera source, the test pattern, or the rig's capture devices enters this design.

## The permission is the standard desktop grant, and the CLI installs it

Owner, 2026-09-06: the concern was giving the whole runtime root. It never has it. What
gates the loopback door is a device node's mode, and the way Linux desktops say "this user
may open this device" is a udev rule with the `uaccess` tag: logind hands the active seat's
user an ACL on the node, the same path that opens the GPU, the sound card and USB devices to
a session without privilege. The engine and its processors run as the user, always; what
root does, once, is install that rule and load the module with no devices. That is the same
one-time shape OBS's virtual camera asks of its users on Linux, which also rides
v4l2loopback.

So the CLI gains `enable-virtual-camera`: it writes the three files and reloads udev as one
privileged step through `pkexec`, the desktop's own password dialog, with `sudo` as the
headless fallback and `--print` for the user who wants to place the files by hand. The verb
is the user's action and never the runtime's, which keeps the no-elevation rule intact. The
finer-grained alternative — a polkit-backed system service exposing "add camera" with a
per-application prompt — needs a root-installed service of its own and buys per-app
consent rather than per-user access; it is the later step if the udev grant ever feels too
blunt, not v1.

A sink that lacks the permission says so by name — which file is absent or not writable
and which command grants it — and the runtime keeps running; under `auto` it takes the
PipeWire door in the meantime and still names the command.

## The default name carries a short stable id

Owner, 2026-09-06: two people who never named their camera must not collide on "StreamLib
Camera". The default is that name plus four characters of a hash over the app's entry
directory and the instance's display name. Random per run would have satisfied uniqueness
but broken reclaim: a device left behind by a crash carries the label the sink chose, and
the next run can only recognise it if it chooses the same label again. Stable per app and
instance gives both — no collisions between instances or apps, and a label the sink can
find again.
