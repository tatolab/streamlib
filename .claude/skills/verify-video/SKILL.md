---
name: verify-video
description: Capture a verification video from the vivid virtual camera through the streamlib processor pipeline and send to Telegram. Use when the user asks for a verification video, test video, or wants to confirm the encode pipeline works.
user_invocable: true
arguments:
  - name: codec
    description: "h264 or h265 (default: h264)"
    required: false
---

Record an MP4 through the streamlib processor pipeline (vivid camera → encoder → `Mp4Sink`,
beside the known audio signal → `OpusEncoder` → the same sink), prove the container with our own
tooling, and send the file to the user via Telegram. Use this when the user asks to verify the
video pipeline, requests a test video, or wants confirmation that recording works.

The recording gate does the recording, the inspection and the decode-back in one script; this
skill runs it and sends what it wrote.

## Arguments

- `codec`: First argument — `h264` or `h265`. Default: `h264`

## Steps

1. Run the recording gate (**debug build only** — release has a known race condition, see #273):
   ```bash
   runtime/streamlib-engine/tests/fixtures/e2e_fixture_recording.sh /tmp/streamlib-verify-video $codec
   ```
   Three phases, each failing on its own terms: `recording_node.py` records until the file holds
   enough video and then takes SIGTERM (a run needing SIGKILL is a hard fail — teardown is what
   closes the last fragment); `cargo xtask mp4-inspect` reads the written file; and
   `codec_roundtrip_rig --source mp4:<file>` replays the video track back through our decoder,
   locked to the per-codec vivid baseline within ±0.05.

   Exit codes: `0` pass, `1` fail, `77` skip (no vivid, no GPU). A skip is not a pass — say so.

2. Read the verdict and the written MP4 out of the output directory:
   ```bash
   ls /tmp/streamlib-verify-video
   cargo xtask mp4-inspect /tmp/streamlib-verify-video/recording.mp4
   ```
   `mp4-inspect` reports the tracks under their inbound link names, each one's sample entry
   (`avc1`/`hvc1` for the video track, `Opus` for the audio one), the fragments and the per-track
   durations — as JSON, so nothing here needs ffprobe.

3. Send the MP4 to the user via Telegram using the `reply` tool. Look up the chat_id from memory
   (reference_telegram_chat). Include: codec, the track list `mp4-inspect` reported, the
   decode-back's channel-mean drift against the baseline, and that it was captured from the vivid
   virtual camera through the streamlib processor pipeline.

## Important

- **Always use debug build** (no `--release`) — release build has a threading race condition (#273)
- The vivid virtual camera is at `/dev/video2` — outputs animated SMPTE color bars with frame counter
- If vivid isn't available, check `v4l2-ctl --list-devices`
- No ffmpeg step: `Mp4Sink` strips parameter sets from samples and writes `avc1`/`hvc1`, which is
  already what Apple hardware plays, so there is no re-tag to do
- `INJECT_BUG=bt601-bt709 | swap-channels | swap-chroma` proves the decode-back's lock is
  non-vacuous, if the user asks whether the gate can fail
