---
name: verify-video
description: Capture a verification video from the vivid virtual camera through the streamlib processor pipeline and send to Telegram. Use when the user asks for a verification video, test video, or wants to confirm the encode pipeline works.
user_invocable: true
arguments:
  - name: codec
    description: "h264 or h265 (default: h265)"
    required: false
  - name: duration
    description: "seconds to capture (default: 5)"
    required: false
---

Capture a verification video using the streamlib processor pipeline (Camera → Encoder → MP4Writer) and send it to the user via Telegram. Use this when the user asks to verify the video pipeline, requests a test video, or wants confirmation that encoding works.

> **Blocked on the muxer.** No `Mp4Sink` built-in exists yet — it lands with the recording
> showcase. The codec rig below proves encode → decode end to end but writes no file, so steps
> 3–5 have nothing to send. Until the muxer lands, verify with `/verify-live`, which reads the
> decoded frames out of the running rig over the control plane.

## Arguments

- `codec`: First argument — `h264` or `h265`. Default: `h265`
- `duration`: Second argument — seconds the `timeout` wrapper gives the rig. Default: `5`

## Steps

1. Delete any previous output:
   ```bash
   rm -f /tmp/streamlib_live_h264.mp4 /tmp/streamlib_live_h265.mp4 /tmp/streamlib_test_h265.mp4
   ```

2. Run the codec rig (**debug build only** — release has a known race condition, see #273):
   ```bash
   timeout $((duration + 15)) cargo run -p streamlib-engine --example codec_roundtrip_rig -- \
     --codec $codec --source camera --camera /dev/video2
   ```
   Output: none on disk — the rig hosts the control plane and stays up. Steps 3–5 resume when
   the muxer lands.

3. If codec is `h265`, re-mux with `hvc1` tag for Apple device playback:
   ```bash
   ffmpeg -y -i /tmp/streamlib_live_h265.mp4 -c:v copy -c:a copy -tag:v hvc1 -movflags +faststart /tmp/streamlib_test_h265.mp4
   ```
   Send path: `/tmp/streamlib_test_h265.mp4`

   For `h264`, send path: `/tmp/streamlib_live_h264.mp4`

4. Verify the output with ffprobe:
   ```bash
   ffprobe -v error -select_streams v:0 -show_entries stream=codec_name,r_frame_rate,nb_read_frames -count_frames -of csv $send_path
   ```

5. Send the MP4 to the user via Telegram using the `reply` tool. Look up the chat_id from memory (reference_telegram_chat). Include: codec, frame count, duration, and that it was captured from vivid virtual camera via the streamlib processor pipeline.

## Important

- **Always use debug build** (no `--release`) — release build has a threading race condition (#273)
- The vivid virtual camera is at `/dev/video2` — outputs animated SMPTE color bars with frame counter
- If vivid isn't available, check `v4l2-ctl --list-devices`
- The rig takes no duration flag and does not auto-stop — it hosts the control plane and runs until killed, so the `timeout` wrapper is the only bound; its margin also covers compilation
