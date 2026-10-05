# JPEG after the robotics cut: no block, a decoder inside capture

Rationale for the `[jpeg-after-the-robotics-cut]` entries in `docs/plan/ARCHITECTURE.md`
§Media I/O and §Consumers, decided by the owner 2026-10-04.

## Trigger

Read this before proposing a JPEG decoder block, before reviving a vendor JPEG backend,
before deleting `sdk/vulkan-jpeg` as dead code, or when a Linux webcam shows a few frames a
second at its top resolution.

## Decision

1. **`JpegDecoder` is retired unbuilt.** It was frozen for one consumer, a drone video
   path. §Product puts robotics out of scope, so that consumer does not return, and a
   block with no consumer fails the built-in criterion's third clause.
2. **The parked nvJPEG backend and its `libnvjpeg` probe are deleted.** A decoder that
   needs one vendor's CUDA library has no place once accelerators are optional and the
   block it backed is gone.
3. **The held JPEG consumers delete outright.** `packages/jpeg`, `examples/jpeg-psnr` and
   the fixture that drives the example were held for a JPEG rung. With no block there is
   no rung, so nothing is mined and no proof-rig job is owed. `packages/jpeg` builds
   against plugin crates the tree no longer has, and the example and its fixture run
   through it, so the fixture had already stopped being a proof.
4. **MJPEG capture on Linux is decided, and `sdk/vulkan-jpeg` is its decoder.** The V4L2
   arm reads a camera's MJPEG modes and decodes inside the capture path. The crate stays
   a workspace member until the change that builds this moves it into the engine.

## Why MJPEG capture is decided rather than left open

The case is arithmetic. USB 2.0 carries at most about 24 MB/s of isochronous video.
Uncompressed YUYV needs 18 MB/s at 640x480 and 30 fps, 55 MB/s at 1280x720 and 30 fps, and
124 MB/s at 1920x1080 and 30 fps. A USB 2.0 camera therefore sends anything above VGA
either compressed or slowly. The V4L2 arm takes the highest NV12 or YUYV resolution and
whatever rate the driver has there, so such a camera is expected to run at its slowest
rate. The
product sentence promises a live camera within a minute, and the people it now addresses
own ordinary webcams, not capture cards.

Leaving the crate in the tree with no entry behind it would have repeated the state this
decision cleans up: code held for a consumer nobody had committed to.

## What was measured, and what was not

- The rig's capture card offers MJPEG. One 1920x1080 frame from it is baseline JPEG,
  4:2:2, with its Huffman tables present.
- The decoder's compute kernel accepts 4:2:0 only and refuses that frame by name. Reading
  4:2:2 is part of the build, which is why the plan entry says so.
- The crate builds against the `streamlib` facade, which sits above the engine, so the
  engine cannot link it where it is. Moving it is part of the build.
- No USB 2.0 webcam has been on the rig. The capture card is USB 3 and sends uncompressed
  1080p at 60 fps, so it shows the frame shape and nothing about the bandwidth limit. A
  real webcam is the build's acceptance check for that reason.
- Some MJPEG sources omit their Huffman tables and rely on the standard ones. Whether
  webcams reaching the V4L2 arm do is unmeasured. The decoder refuses a scan with a
  missing table today.
- The Apple arm was not examined with an MJPEG-only camera. The entry leaves it unchanged.

## Rejected alternatives

- **Keep `JpegDecoder` frozen** — the freeze waited on a consumer the product no longer
  has.
- **Keep the crate and leave capture undecided** — an orphan crate with no dependent and
  an unproven GPU half, waiting on a maybe.
- **Delete the crate with the rest** — the parser, entropy decode, kernel and colour
  handling are written and the need is arithmetic; rebuilding them later costs more than
  carrying them.
- **Keep the JPEG example and fixture as the crate's proof** — they cannot build against
  the tree, and a test owns its fixtures. The capture change brings its own proof.
- **A JPEG block as well as capture** — nothing in the tree produces a JPEG stream for a
  block to decode.

## Consequences

- Between the removals and the capture build, the decoder's GPU half is compiled and
  unproven; only its parser, Huffman and colour tests run.
- The capture build owes 4:2:2 decode, the move into the engine, a mode-selection rule
  that weighs frame rate, and a proof on a real USB 2.0 webcam.
- The decoder choice is the owner's stated one. Its internals — how much runs on the CPU,
  whether default Huffman tables are needed — stay ticket-level.
