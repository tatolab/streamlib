# h264-opus-validator

A self-contained H.264 + Opus / WebRTC validator. It generates an H.264 test
pattern and a stereo sine-wave audio track with `ffmpeg`, muxes them into an
MP4, and verifies the result with `ffprobe`. Pass `--webrtc` to run the WebRTC
isolation test instead.

This example depends **only on public crates** (`webrtc`, `opus`, `hyper`,
`rustls`, …) and shells out to `ffmpeg`/`ffprobe` — it does **not** depend on
the streamlib SDK or any `@tatolab/*` package. There is nothing to link and no
processor package to install.

## Run it

```bash
cargo run              # H.264 + Opus → MP4 validation
cargo run -- --webrtc  # WebRTC isolation test
```

`ffmpeg` and `ffprobe` must be on your `PATH`. `./setup.sh` is provided for
uniformity with the other examples but does no linking — see its comment.

`Cargo.lock` and the generated media (`temp_video.mp4`, `temp_audio.aac`,
`output.mp4`) are not committed.
