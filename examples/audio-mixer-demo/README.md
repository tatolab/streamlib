# audio-mixer-demo

Three sine voices → mixer → speaker. Plays a C major chord.

## The model this example teaches

**A window contract is how a consumer states the audio shape it wants.** The
three voices are deliberately mismatched — different sample rates, different
block sizes, one of them stereo:

| Voice | Node | Frequency | Publishes |
| --- | --- | --- | --- |
| C4 | `c4voice` | 261.63 Hz | 48 kHz mono, 512-sample blocks |
| E4 | `e4voice` | 329.63 Hz | 44.1 kHz mono, 441-sample blocks |
| G4 | `g4voice` | 392.00 Hz | 16 kHz **stereo**, 256-sample blocks |

The mixer declares one contract on all three of its input ports:

```python
CHORD_MIX_WINDOW = AudioWindowContract(
    sample_rate=48_000,
    channels=1,
    dtype="f32",
    window_size=512,
)


@node.input(delivery_profile="ordered", audio_window=CHORD_MIX_WINDOW)
def root_voice_from_upstream(self) -> AudioBlock: ...
```

so the engine resamples each voice to 48 kHz, averages the stereo one down to
mono, and frames all three into exactly-512-sample blocks **before** `process()`
runs. What the mixer reads with `ctx.inputs.read(port, into=AudioBlock)` is
three windows of identical shape, and mixing them is one `+`. There is no
resampler, no ring buffer and no format negotiation in this stream, because none
of that is a node's job.

Further things it teaches:

- **A declared contract requires `delivery_profile="ordered"`.** Order matters
  for samples, and `newest` skips messages by design — an accumulator that needs
  contiguous samples would flush on nearly every read. `@node.input` refuses the
  pair at import.
- **`SpeakerSink` matches its own device.** The mixer publishes 48 kHz mono; the
  speaker's input declares `match_device`, so the same stage converts the mix to
  whatever the machine's playback device actually opened at. Nothing in
  the graph document names a device format.
- **A message's payload is bytes.** Each voice and the mixer build an
  `AudioBlock` and write `block.to_bytes()`, so a missing field or a sample
  buffer of the wrong length is refused where it is written, not downstream.
- **Every timestamp is the machine's monotonic clock.** Each voice stamps a
  block with its run anchor plus an integer sample offset, never an accumulated
  per-block delta — which is what keeps 44.1 kHz-family rates exact.
- **The mixer joins on those timestamps, not on arrival order.** Each voice
  starts when its own child interpreter does, so the three streams sit on grids
  tens of milliseconds apart. `ChordMixer` discards any window a whole window or
  more behind the newest of the three, which is the block-level join the
  monotonic clock exists for; pairing by arrival order instead would freeze the
  startup skew in for the whole run and publish a timestamp two of the three
  contributions never came from. A whole window is the only stable bound — the
  grids carry sub-window offsets that discarding cannot remove, so a tighter one
  thrashes forever instead of converging.

The nodes live in `nodes/`, one module each: each of the
four Python nodes — three `ToneSource`s and the `ChordMixer` — runs in its own
child interpreter with its own GIL, which imports the class by name.
`SpeakerSink` comes from the `tatolab-speaker` module.

## Run it

With this machine's runtime running (`tatolabd`, which Tato Studio starts):

```bash
tatolab dev
```

You should hear a steady C major chord.

The runtime makes the project's `.venv` on its first load: it installs `tatolab-stream` from PyPI — the authoring surface — and
`numpy`, which the nodes use, and builds `tatolab-speaker` from this checkout,
as an editable path dependency (`[tool.uv.sources]` in `pyproject.toml`); the
module is a maturin project, so the build needs a Rust toolchain. The runtime
itself is not on pip.

`tatolab dev` follows the project's log and loads the project again on every
saved edit; `tatolab run` queues the load and returns. Ctrl-C ends `tatolab dev`
and leaves the stream loaded; `tatolab stop <stream>` stops it.

**No audio hardware?** It still runs. Where no audio backend exists,
`SpeakerSink` discards what it receives — so the graph, the contract and the
mixing are all exercised in a headless container, silently.

## Observing it

From another terminal, in this folder:

```bash
tatolab streams                  # the streams this machine's runtime holds, `main` among them
tatolab graph --stream <stream>  # nodes, ports, links and counters, as JSON
tatolab logs -f                  # the project's records, Rust and Python alike
```

`graph` renders each windowed port's contract beside its delivery profile, and
the speaker's `match_device` port renders the values its device settled on.
The nodes are `c4voice`, `e4voice`, `g4voice`, `chordmixer` and `speakersink`.
The mix itself is the mixer's `mixed_chord_to_downstream`, which Tato Studio's
viewer plays.

### A handful of dropped windows at startup is expected

The four Python nodes are four child interpreters, and they do not finish
spawning at the same instant. A voice that starts first has nowhere to put its
windows until its partners arrive, so the mixer discards its oldest — bounded,
counted, and reported once at teardown in `tatolab logs`, as the warning
`voices ran far enough apart that windows were dropped waiting to be mixed`
with a `dropped_windows` count.

It is a startup transient, not a leak: the count stops climbing once all three
voices are live, so a longer run reports roughly the same handful as a short one.
The same startup skew is why the mixer also reports a few `realigned_windows` —
the windows it discarded to bring the three voices onto one instant. Both counts
settle once the grids line up and stay put, because the voices then advance in
lockstep.

## Knobs

Each knob is a node's config in `streams/main.graph.json`. Change it in Tato
Studio's inspector or with the runtime's `set_node_config` MCP tool, which
records the edit in the project's history and applies it to the running stream.

| Knob | Node | Effect |
| --- | --- | --- |
| `frequency_hz`, `amplitude` | `c4voice`, `e4voice`, `g4voice` | Each voice's pitch and peak; three at 0.3 sum below full scale. |
| `sample_rate`, `channels`, `block_size` | `c4voice`, `e4voice`, `g4voice` | What one voice publishes. |
| `voice_gain` | `chordmixer` | Gain applied to the sum of the voices, which is then clipped to ±1. |
| `device_id` | `speakersink` | Playback device, as the audio backend names it. Absent picks the backend's default; a named device that cannot be opened raises rather than landing on another one. |

Give the three voices the same rate and block size and the contract converts
nothing — the mixer's code does not change, which is the point.
