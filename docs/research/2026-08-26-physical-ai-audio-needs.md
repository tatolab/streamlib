# Research memo: what a physical-AI audio subsystem actually needs

2026-08-26, for the `[audio-subsystem]` align. Question: with StreamLib reframed as
a physical-AI / sensor-fusion runtime, what does its audio subsystem need — and is
plugin hosting central, adjacent, or a distraction? Rates, window sizes, and
licences below were fetched from primary sources (model repos and their
preprocessing code); items marked inferred are flagged.

## TL;DR (adopted)

Physical-AI audio is perception-first: essentially every model in the
embodied/voice stack wants **16 kHz mono float32 in fixed-size windows**, on CPU,
with monotonic timestamps, while devices run at 44.1/48 kHz. The winning subsystem:
timestamped capture → always-on resampling → **window/hop framing declared on the
port** → zero-copy numpy in the helper, plus a WebRTC-APM conditioning block
(BSD-3-Clause + patent grant — statically linkable in a BUSL-1.1 product). Plugin
hosting is a distraction for this use case. Holoscan — the closest analogue — has
essentially no audio subsystem (3 thin audio wrappers among 88+ operators), so this
is differentiation, not catch-up.

## Rate/format reality (verified per model)

- Whisper: 16 kHz mono f32 [-1,1]; 30 s chunks; mel hop 160. Silero VAD: exactly
  512 samples @16 k (asserts), <1 ms/chunk on one CPU thread. WebRTC VAD: int16,
  exactly 10/20/30 ms. Porcupine: int16, exactly 512. openWakeWord: int16,
  1280-sample multiples. wav2vec2 / ECAPA / YAMNet / AST: 16 kHz mono float.
  Exception: PANNs at 32 kHz; conditioning (RNNoise, DeepFilterNet) at 48 kHz; TTS
  out at 22.05/24/44.1 kHz (Kokoro 24 k Apache-2.0; Riva default 44.1 k).
- Two rate domains per interactive robot: a 48 kHz conditioning domain and a
  16 kHz model domain — resampling appears twice (mic 48→16 k, TTS 24→48 k).
  **Resampling is a first-class always-on stage, not a utility.**

## Tensor handoff — CPU correct, GPU pure loss

A 512-sample f32 window is 2 KB; 16 kHz mono f32 is 64 KB/s — ~4,000× smaller than
1080p30 video. Every surveyed framework hands audio around as CPU int16/float32
(Wyoming PCM-over-TCP, ROS `uint8[]`, Pipecat bytes, HF extractors numpy); no
DLPack-for-audio practice found anywhere. VAD/wake models are designed for CPU;
where GPU inference matters the framework's own ~2 MB H2D copy (~80 µs) is noise.
Adopted: samples inline in the bag, zero-copy `np.frombuffer` view in the helper
(then `torch.from_numpy` is also zero-copy); no surface machinery for audio.

## Windowing is the real API problem

Devices deliver quantum-sized blocks (PipeWire default 1024 @48 k = 21.3 ms);
models reject anything but their exact window. Every framework re-solves this
privately (openWakeWord buffers internally, Silero ships `VADIterator`, Pipecat
chunks in transports, GStreamer has `audiobuffersplit`); **nobody puts window/hop
in the port contract** — LiveKit hands you an `AudioStream` to buffer yourself,
Holoscan hands you nothing. The adopted shape: rate/channels/dtype/window/hop
declared on the input port, engine does resample→mixdown→framing natively.
One semantic delta from video: speech perception needs lossless in-order delivery
with explicit overrun signalling, not latest-wins — dropped samples corrupt ASR
silently.

## Latency — soft realtime, honestly

Human turn gap ~200 ms modal (Stivers et al., PNAS 2009); practitioner
voice-to-voice budgets ~800 ms good / >1500 ms broken, with LLM TTFT (400–800 ms)
dominant. Capture at a 21.3 ms quantum is ~3% of budget. Hard-realtime pro-audio
engineering (sub-5 ms, 64-sample quanta) is not warranted. What is required:
zero dropped samples; bounded jitter at the 10–20 ms scale; barge-in reaction
~200 ms (VAD on mic while TTS plays → needs AEC + an immediately-cancellable
playback sink); and the one genuinely tight constraint — AEC mic/reference
sample alignment with drift compensation (inferred from AEC operating principles;
PipeWire solves it by owning both streams in one graph).

## A/V sync — block-level is enough

Lip-sync detectability +45/−125 ms (ITU-R BT.1359); AV-HuBERT fuses at 25 Hz
(40 ms blocks). Nothing surveyed needs cross-modal sample accuracy. Stamp each
block with first-sample time + rate + count on CLOCK_MONOTONIC and fusion reduces
to join-by-timestamp — the epoch StreamLib already unified.

## Feature extraction — hand over raw samples

Every model ships its own extractor tuned to its training statistics (Whisper's
`(log_mel+4)/4`, AST's AudioSet mean −4.27/std 4.57, YAMNet's `log(mel+0.001)`);
a runtime-generic mel block would be subtly wrong for each. The subsystem contract
ends at windowed raw float32; torchaudio/model code owns features in the helper.

## Conditioning licensing — the headline

**WebRTC APM is BSD-3-Clause with an explicit patent grant**
(https://webrtc.org/support/license): AEC3, NS, AGC1/2, HPF, VAD. It is what
PipeWire's `module-echo-cancel` wraps (sole AEC method: `aec/libspa-aec-webrtc`)
and what PulseAudio ships (freedesktop `webrtc-audio-processing`). A BUSL-1.1
product can statically link it. Supporting cast, all permissive: speexdsp
(revised BSD), RNNoise (BSD-3, 48 kHz), **DeepFilterNet (dual MIT/Apache, written
in Rust, real-time)**. Premium alternatives (Krisp, Koala, ai-coustics) are all
proprietary. Caution: **Piper TTS is now GPL-3** (`piper1-gpl`) — never link.
Hardware trend: XMOS XU316-class DSPs (HA Voice PE, ReSpeaker Lite) condition
before the host — the chain must be bypassable, not hardwired.

## Mic arrays

N-channel capture + channel-select/mixdown belongs in the format model from day
one (cheap). Native beamforming/DoA is niche: arrays increasingly condition
on-chip and present as mono/stereo USB; ODAS (MIT) covers robot audition as a
wrappable external process. Do not build.

## Plugin hosting verdict (adopted)

Not one surveyed physical-AI or voice system hosts audio plugins — not Riva,
Holoscan, LiveKit, Pipecat, Wyoming/HA, ROS, ODAS. The DSP physical AI needs
ships as permissive libraries that link directly; the plugin ABIs serve DAW
workflows (GUI editors, host tempo) with no robot counterpart. Genuine-but-
marginal uses (DeepFilterNet's plugin build; room-simulation for synthetic data)
are reachable later through ordinary wrappers. Classification: adjacent at best;
revisit only when a concrete consumer demands a specific plugin.

## Sources (load-bearing)

Whisper audio.py: https://github.com/openai/whisper/blob/main/whisper/audio.py ·
Silero: https://github.com/snakers4/silero-vad · py-webrtcvad:
https://github.com/wiseman/py-webrtcvad · Porcupine:
https://picovoice.ai/docs/api/porcupine-python/ · openWakeWord:
https://github.com/dscripka/openWakeWord · wav2vec2:
https://huggingface.co/docs/transformers/en/model_doc/wav2vec2 · ECAPA:
https://huggingface.co/speechbrain/spkrec-ecapa-voxceleb · YAMNet:
https://github.com/tensorflow/models/blob/master/research/audioset/yamnet/README.md ·
AST: https://huggingface.co/docs/transformers/en/model_doc/audio-spectrogram-transformer ·
PANNs: https://github.com/qiuqiangkong/audioset_tagging_cnn · RNNoise:
https://github.com/xiph/rnnoise · DeepFilterNet:
https://github.com/Rikorose/DeepFilterNet · Kokoro:
https://huggingface.co/hexgrad/Kokoro-82M · piper1-gpl:
https://github.com/OHF-Voice/piper1-gpl · WebRTC licence:
https://webrtc.org/support/license · webrtc-audio-processing:
https://freedesktop.org/software/pulseaudio/webrtc-audio-processing/ ·
module-echo-cancel: https://docs.pipewire.org/page_module_echo_cancel.html ·
HoloHub operators: https://nvidia-holoscan.github.io/holohub/operators/ · Riva
ASR: https://docs.nvidia.com/deeplearning/riva/user-guide/docs/asr/asr-overview.html ·
LiveKit turns: https://docs.livekit.io/agents/build/turns/ · Pipecat params:
https://docs.pipecat.ai/server/pipeline/pipeline-params · Wyoming:
https://github.com/rhasspy/wyoming · audio_common:
https://github.com/ros-drivers/audio_common · ODAS:
https://github.com/introlab/odas · AV-HuBERT:
https://github.com/facebookresearch/av_hubert · Stivers et al.:
https://www.pnas.org/doi/10.1073/pnas.0903616106

Open questions carried: resampler library pick (rubato MIT high-confidence,
unverified; libsamplerate BSD-2; soxr LGPL — avoid); Silero weight-licence check
before bundling; AEC3 far-field quality vs hardware DSP (rig work); delegate to
PipeWire echo-cancel when present vs always in-engine APM.
