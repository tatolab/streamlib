# Rig profile (example)

Copy this file to `docs/rig-profile.local.md` (gitignored, one per machine) and fill in the real
values for your host. **A runtime probe always beats this file** — when you can query the device,
query it (`v4l2-ctl --list-devices`, `nvidia-smi`, `vulkaninfo`); this profile is only the
starting hint for a sandboxed session that can't probe.

The example values below are illustrative for this class of workstation — replace every one.

## Video devices
| Node | Role | Notes |
|---|---|---|
| `/dev/video0` | vivid virtual camera (test pattern) | driver-synthesized SMPTE/colorbar source; always present on this class of host |
| `/dev/video2` | real UVC camera (e.g. Cam Link 4K) | physical capture; only present when the hardware is attached |
| `/dev/video10` | v4l2loopback | for motion fixtures (`ffmpeg → testsrc2 → /dev/video10`), loaded on demand |

## GPU
- Model: `<e.g. NVIDIA RTX 3090>`
- Driver: `<e.g. 570.211.01>`
- Vendor id: `<e.g. 0x10DE (NVIDIA)>` — gates driver-specific paths (DMA-BUF probe, QFOT acquire)

## Cameras
- List each physical camera, its node, and what it sees (for reading E2E PNG samples against a
  known scene).

## Audio
- Playback / capture devices, virtual sinks, and how to route a silent-but-present track for
  fixtures that need one.
