# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Type stubs for the compiled engine module.

A type checker and an editor can read nothing out of `_engine.abi3.so`, so this
file is the only description of the native surface they get — without it,
`rt.add` offers no completion and `Runtime` resolves as unknown.

Hand-maintained, and kept honest by `mypy.stubtest`, which imports the built
module and compares it against this file in CI. `pyright --verifytypes` does not
catch that: it scores annotation completeness, so a stub describing a method the
binary no longer exports still reads as complete.
"""

from pathlib import Path
from types import TracebackType
from collections.abc import Callable, Mapping, Sequence
from typing import Any, Literal, TypeVar, final, overload

from .claimed_surface_pixel_access import ClaimedSurfacePixelAccess

from typing_extensions import disjoint_base

_EscalateResult = TypeVar("_EscalateResult")
_BagReadTarget = TypeVar("_BagReadTarget")

__all__ = [
    "AccelerationStructureHandle",
    "AddedProcessor",
    "ComputeKernel",
    "GraphicsKernel",
    "KernelDispatchBatch",
    "RayTracingKernel",
    "GpuContextFullAccess",
    "GpuContextLimitedAccess",
    "GpuSurfaceCheckOutLease",
    "GpuSurfaceDeviceTensorScope",
    "GpuSurfaceHandle",
    "LinkInputDataReader",
    "LinkOutputDataWriter",
    "MonotonicTimer",
    "OpaqueFdTextureExport",
    "ProcessorInputPortReference",
    "ProcessorOwnedWindow",
    "ProcessorOwnedWindowEvents",
    "ProcessorLinkDataAccess",
    "ProcessorOutputPortReference",
    "CameraSource",
    "CapabilityExtensionHost",
    "DisplayWindow",
    "H264Decoder",
    "H264Encoder",
    "H265Decoder",
    "H265Encoder",
    "MicrophoneSource",
    "Mp4Sink",
    "OpusDecoder",
    "OpusEncoder",
    "Runtime",
    "RuntimeContextFullAccess",
    "RuntimeContextLimitedAccess",
    "SpeakerSink",
    "TestPatternSource",
    "VirtualCameraSink",
    "TestBagCollector",
    "TestBagFeeder",
    "await_test_harness_bag",
    "capability_extension_host_for_the_app_process",
    "capability_extension_host_for_the_helper_process",
    "close_test_harness_channel",
    "decode_msgpack_bytes_to_python_object",
    "decode_tapped_channel_bag_frame_to_python_object",
    "encode_bag_to_msgpack_bytes",
    "feed_test_harness_bag",
    "gpu_limited_access_of_the_typed_read_in_progress",
    "log_event",
    "monotonic_now_ns",
    "open_test_harness_channel",
    "runtime_log_directory",
]

@final
class CameraSource:
    """Native built-in block: live V4L2 camera capture (Linux).

    A marker type — pass the class itself to `Runtime.add`
    (`rt.add(CameraSource, config={"device_id": "/dev/video0"})`); it is
    never instantiated and its per-frame path never enters the interpreter.
    Camera→GPU transport auto-selects zero-copy DMA-BUF or CPU upload.
    """

@final
class DisplayWindow:
    """Native built-in block: video frames in a vsync'd window (Linux).

    A marker type — pass the class itself to `Runtime.add`
    (`rt.add(DisplayWindow, config={"title": "My app", "scaling": "fit"})`);
    it is never instantiated and its per-frame path never enters the
    interpreter. `scaling` is `"fit"`, `"fill"`, or `"stretch"`.

    Add as many as the graph needs: each instance registers its own window
    with the engine's shared event pump and renders on its own thread. An
    instance that cannot get a window drains its input without showing
    anything, so upstream still sees a live consumer.
    """

@final
class H264Decoder:
    """Native built-in block: H.264 encoded-frame bags to decoded video
    frames via Vulkan Video hardware decode (Linux).

    A marker type — pass the class itself to `Runtime.add`
    (`rt.add(H264Decoder)`); it is never instantiated and its per-frame path
    never enters the interpreter.

    Input `encoded_video` (`ordered`) takes encoded-frame bags in the wire
    shape `H264Encoder` publishes; a bag the decoder cannot read is refused
    by name, never reshaped. Output `video` publishes an ordinary
    `streamlib.VideoFrame` on a pooled RGBA pixel-buffer surface at the
    conformance-windowed extent — never the coded picture — carrying the
    encoded frame's own timestamp and `color_info`, with `fps`,
    `texture_layout` and the HDR sidecars absent, so `DisplayWindow` and a
    `read(port, into=VideoFrame)` consume it unchanged. A decoded frame is
    buffer-backed, so it reaches a Python kernel through a DLPack landing
    copy, never by bare surface id — the camera's own gap, not a new one.

    Config keys, all optional (`rt.add(H264Decoder)` bare is legal):
    `max_width` and `max_height` cap the decoded-picture-buffer allocation
    together or not at all — a half-specified pair warns and auto-detects
    both from the stream's first SPS, as an absent pair does.

    The decode session is minted at `setup()`, sized by the caps above. On a
    device with no Vulkan Video decode queue for the codec, setup refuses by
    name: the processor never reaches Running, and
    `Runtime.wait_until_every_processor_is_running` raises rather than the
    graph running with an empty channel.
    """

@final
class H264Encoder:
    """Native built-in block: video frames to H.264 encoded-frame bags via
    Vulkan Video hardware encode (Linux).

    A marker type — pass the class itself to `Runtime.add`
    (`rt.add(H264Encoder, config={"keyframe_interval_seconds": 2})`); it is
    never instantiated and its per-frame path never enters the interpreter.

    Input `video` (`ordered`) takes any published `streamlib.VideoFrame` —
    buffer-backed (camera, test pattern) or texture-backed (a kernel
    output). Output `encoded_video` publishes encoded-frame bags: one
    Annex-B access unit per bag, beside the stream metadata keys. A frame
    the encoder cannot consume is logged and dropped while the processor
    keeps running.

    Config keys, every one an optional non-negative integer
    (`rt.add(H264Encoder)` bare is legal): `width` and `height` are
    guardrails, not a resize — a mismatching frame wins with a warning;
    `fps` is the fallback rate, resolved frame → config → 60; `bitrate_bps`
    absent means constant-QP encoding at the medium preset;
    `keyframe_interval_seconds` is the IDR cadence, defaulting to 2;
    `effort_level` is the Vulkan encoder-effort index (driver analysis
    budget, not a codec quality knob). The session mints from the first
    frame's dimensions and re-mints when the upstream extent changes.

    On a device without Vulkan Video encode the session fails to mint: the
    failure latches and every later frame is discarded with one error line —
    no exception reaches Python.
    """

@final
class H265Decoder:
    """Native built-in block: H.265 encoded-frame bags to decoded video
    frames via Vulkan Video hardware decode (Linux).

    A marker type — pass the class itself to `Runtime.add`
    (`rt.add(H265Decoder)`); it is never instantiated and its per-frame path
    never enters the interpreter.

    Input `encoded_video` (`ordered`) takes encoded-frame bags in the wire
    shape `H265Encoder` publishes; a bag the decoder cannot read is refused
    by name, never reshaped. Output `video` publishes an ordinary
    `streamlib.VideoFrame` on a pooled RGBA pixel-buffer surface at the
    conformance-windowed extent — never the coded picture — carrying the
    encoded frame's own timestamp and `color_info`, with `fps`,
    `texture_layout` and the HDR sidecars absent, so `DisplayWindow` and a
    `read(port, into=VideoFrame)` consume it unchanged. A decoded frame is
    buffer-backed, so it reaches a Python kernel through a DLPack landing
    copy, never by bare surface id — the camera's own gap, not a new one.

    Config keys, all optional (`rt.add(H265Decoder)` bare is legal):
    `max_width` and `max_height` cap the decoded-picture-buffer allocation
    together or not at all — a half-specified pair warns and auto-detects
    both from the stream's first SPS, as an absent pair does.

    The decode session is minted at `setup()`, sized by the caps above. On a
    device with no Vulkan Video decode queue for the codec, setup refuses by
    name: the processor never reaches Running, and
    `Runtime.wait_until_every_processor_is_running` raises rather than the
    graph running with an empty channel.
    """

@final
class H265Encoder:
    """Native built-in block: video frames to H.265 encoded-frame bags via
    Vulkan Video hardware encode (Linux).

    A marker type — pass the class itself to `Runtime.add`
    (`rt.add(H265Encoder, config={"keyframe_interval_seconds": 2})`); it is
    never instantiated and its per-frame path never enters the interpreter.

    Input `video` (`ordered`) takes any published `streamlib.VideoFrame` —
    buffer-backed (camera, test pattern) or texture-backed (a kernel
    output). Output `encoded_video` publishes encoded-frame bags: one
    Annex-B access unit per bag, beside the stream metadata keys. A frame
    the encoder cannot consume is logged and dropped while the processor
    keeps running.

    Config keys, every one an optional non-negative integer
    (`rt.add(H265Encoder)` bare is legal): `width` and `height` are
    guardrails, not a resize — a mismatching frame wins with a warning;
    `fps` is the fallback rate, resolved frame → config → 60; `bitrate_bps`
    absent means constant-QP encoding at the medium preset;
    `keyframe_interval_seconds` is the IDR cadence, defaulting to 2;
    `effort_level` is the Vulkan encoder-effort index (driver analysis
    budget, not a codec quality knob). The session mints from the first
    frame's dimensions and re-mints when the upstream extent changes.

    On a device without Vulkan Video encode the session fails to mint: the
    failure latches and every later frame is discarded with one error line —
    no exception reaches Python.
    """

@final
class MicrophoneSource:
    """Native built-in block: audio capture as timestamped sample blocks.

    A marker type — pass the class itself to `Runtime.add`
    (`rt.add(MicrophoneSource, config={"device_id": "..."})`); it is never
    instantiated and its capture callback never enters the interpreter.

    The backend chain is probed once per process with no configuration dial;
    where no audio backend exists at all the blocks are silence, so a pipeline
    authored on a workstation runs unchanged in a headless container. Omitting
    `device_id` takes the backend's default device; naming one the backend
    cannot open raises rather than landing on a different device.

    Blocks arrive on the `audio` output as bags `streamlib.AudioBlock` casts.
    """

@final
class Mp4Sink:
    """Native built-in block: encoded video and audio bags recorded to one
    fragmented MP4 file.

    A marker type — pass the class itself to `Runtime.add`
    (`rt.add(Mp4Sink, config={"path": "recording.mp4"})`); it is never
    instantiated and its per-bag path never enters the interpreter.

    One input, `tracks` (`ordered`), and no output. Any number of links may
    enter it and **each inbound link is one track**, named by its source
    channel name — `<lowercased producer processor id>/<output port>`, what
    `graph` and `tap` already show — so two cameras are two video tracks and
    three microphones three audio tracks with nothing configured between
    them. A track's kind is its bags' `codec`: `"h264"` and `"h265"` a video
    track, `"opus"` an audio track, anything else refused by name.

    `path` is the one config key and it is required. The file is created or
    truncated at `setup()` — an app re-run from the same `app.py` overwrites
    its last recording — and a path that cannot be opened, or a sink no link
    enters, is refused by name there.

    The layout is fragmented: `ftyp`, one `moov`, then `moof` + `mdat` per
    fragment with one `traf` per track. `moov` is written once every track
    has delivered its first sync-point bag, since a sample entry needs the
    parameter sets or the Opus header; a link still silent is named once a
    second, and latched by name if the samples held for it reach the writer's
    budget, so the tracks that did deliver start recording rather than wait
    for one that never will. A fragment closes at the first video track's
    sync points, or once a second when no video track is wired. Because a
    fragment is complete when it lands, a file plays to its last closed
    fragment even if the process dies — `teardown()` closes the open one,
    held-back frames included, but teardown is never a promise.

    Refusals latch per track and never per file, since one `moov` holds one
    sample entry per track and there is no second to switch to: a parameter
    set that changes mid-file, a track whose `codec` changes, an Opus track
    whose `channels` change, and an Opus track above two channels each stop
    that track — named once, with its last written stamp — while every other
    track keeps recording. A bag stamped at or before its track's last
    written one is dropped and counted, a producer bug on an `ordered` input.
    """

@final
class OpusDecoder:
    """Native built-in block: Opus encoded-audio-packet bags to decoded audio
    blocks via libopus.

    A marker type — pass the class itself to `Runtime.add`
    (`rt.add(OpusDecoder)`); it is never instantiated and its per-packet path
    never enters the interpreter. There is no config.

    Input `encoded_audio` (`ordered`) takes encoded-audio-packet bags in the
    wire shape `OpusEncoder` publishes, and declares no window contract — an
    encoded link carries whole packets, and reframing a compressed bitstream
    is not a thing the stage could do. A bag the decoder cannot read is
    refused by name: a `codec` other than `"opus"`, a `bitstream` that is not
    msgpack bin, a `sample_rate` other than 48 000, or a `pre_skip` past the
    5 760 samples one Opus packet can span. Output `audio` publishes ordinary
    `streamlib.AudioBlock` bags — `f32` at 48 000 Hz in the packet's own
    channel count.

    It enters the stream at any packet, since every Opus packet is a sync
    point, and trims the encoder's `pre_skip` lookahead at entry — so the
    first block after entry is short by exactly that many samples and every
    block after it spans the packet's full `sample_count`. Stamps are derived
    from the entry packet's own stamp plus the samples emitted since, so the
    first emitted sample is the stamped instant rather than one lookahead
    later.

    A `sequence_index` step other than one is a gap: the decoder resets,
    re-enters at that packet, and logs how many it did not see. Nothing is
    invented to bridge it — no concealment, no FEC decode — so the gap stays
    derivable from the stamps either side.
    """

@final
class OpusEncoder:
    """Native built-in block: 20 ms windows of audio to Opus
    encoded-audio-packet bags via libopus.

    A marker type — pass the class itself to `Runtime.add`
    (`rt.add(OpusEncoder, config={"bitrate_bps": 96000})`); it is never
    instantiated and its per-window path never enters the interpreter.

    Input `audio` (`ordered`) declares
    `audio_window(sample_rate=48000, dtype="f32", window_size=960, hop=960)`
    and states no channel count, so the engine resamples to Opus's own clock,
    converts to `f32` and frames into 20 ms windows while the count follows
    whatever the source publishes — one microphone and one ambisonic rig
    reach this encoder with nothing configured between them. One to eight
    channels; more is refused by name. Output `encoded_audio` publishes
    encoded-audio-packet bags: one Opus packet per bag, beside the codec,
    ordering pair and stream format.

    The libopus encoder is minted from the first window's channel count — one
    or two channels as a single stream, three to eight as a multistream under
    channel mapping family 1 — and re-mints when the source's count changes,
    which libopus offers no other mechanism for. A re-mint costs prediction
    state, not decodability, and `sequence_index` does not reset across it, so
    a consumer still reads a gap as loss and never as a restart.

    Config keys, both optional (`rt.add(OpusEncoder)` bare is legal):
    `bitrate_bps` absent means libopus picks its own rate from the sample rate
    and channel count; `application` is `"audio"`, `"voip"` or `"lowdelay"`,
    absent meaning `"audio"`. In-band FEC and DTX are off and are not knobs.
    """

@final
class SpeakerSink:
    """Native built-in block: plays timestamped blocks of interleaved samples.

    A marker type — pass the class itself to `Runtime.add`
    (`rt.add(SpeakerSink, config={"device_id": "..."})`); it is never
    instantiated and its device callback never enters the interpreter.

    The backend chain is probed once per process with no configuration dial;
    where no audio backend exists at all the samples are discarded, so a
    pipeline authored on a workstation runs unchanged in a headless container.
    Omitting `device_id` takes the backend's default device; naming one the
    backend cannot open raises rather than landing on a different device.

    Blocks to play arrive on the `audio` input as bags in the
    `streamlib.AudioBlock` shape. The port declares
    `audio_window = match_device`, so the engine resamples every block to the
    device's rate and re-frames it into device-period windows — `graph` renders
    the resolved values on the port. Conversion is not unconditional: `dtype`
    must be `"f32"` or `"i16"`, and channels convert N to 1 by averaging and 1
    to N by duplicating, so a pair with neither side mono (stereo into a
    five-channel device, say) is refused by name rather than mixed. The device
    is never left waiting on the graph — a period the graph had no samples for
    is silence, and the count of it is reported.
    """

@final
class TestPatternSource:
    """Native built-in block: SMPTE-style color bars, no hardware.

    A marker type — pass the class itself to `Runtime.add`
    (`rt.add(TestPatternSource, config={"width": 1280, "height": 720})`);
    it is never instantiated and its per-frame path never enters the
    interpreter.
    """

    # Keeps pytest from collecting the `Test*`-named class in user suites.
    __test__: Literal[False]

@final
class VirtualCameraSink:
    """Native built-in block: video frames to a virtual camera any Linux
    application can select. Each instance is one camera that exists while its
    processor runs — created at setup, removed at teardown, like a USB camera
    plugged in and pulled out — showing whatever the graph writes into it.

    A marker type — pass the class itself to `Runtime.add`
    (`rt.add(VirtualCameraSink, config={"name": "Desk cam"})`); it is never
    instantiated and its per-frame path never enters the interpreter.

    One input, `video` (`newest`), and no output. Add as many instances as the
    graph needs; each is its own camera.

    Config keys: `name`, the camera's name in every picker, defaulting to
    "StreamLib Camera" plus a short id that is unique per instance and app and
    stable across runs; `door`, "auto" (default), "v4l2loopback", or
    "pipewire". Under "auto" the sink creates a v4l2loopback device when the
    module's control node is writable — the door every application sees — and
    otherwise registers a PipeWire camera node, which needs no module and no
    root. The door is logged at setup; without permission to create a loopback
    camera the log names `streamlib enable-virtual-camera`, the one-time
    command that grants it behind your desktop's password prompt.
    "v4l2loopback" refuses by name at `setup()` in that case, and the runtime
    keeps running. "pipewire" takes that door whatever the control node says,
    refusing by name only where no PipeWire session answers. One instance is
    never on both doors: a session manager mirrors every V4L2 capture device
    into the portal's camera set, so it would list the same camera twice. The
    engine never loads a module or asks for elevation.

    A loopback device a reader still holds at teardown is left in place and
    reclaimed by name at the next setup; a PipeWire node is gone with its
    stream. Frames are stamped with their monotonic timestamp on both doors,
    and the format follows the first frame's extent — YUYV at an even width on
    the loopback door, RGBA offered as a DMA-BUF the consumer imports with no
    copy (or a shared-memory sibling) on the PipeWire one. An extent change
    re-negotiates it.
    """

@final
class TestBagFeeder:
    """`streamlib.testing`'s feeder endpoint: publishes bags a test queued.

    A marker type, like the media built-ins — never instantiated, resolved by
    `Runtime.add`. Native so that its queue lives in the app process, where the
    test reading it does.
    """

    # Keeps pytest from collecting the `Test*`-named class in user suites.
    __test__: Literal[False]

@final
class TestBagCollector:
    """`streamlib.testing`'s collector endpoint: records every bag produced."""

    # Keeps pytest from collecting the `Test*`-named class in user suites.
    __test__: Literal[False]

@disjoint_base
class Runtime:
    """The engine, running in this process."""

    def __init__(self) -> None: ...
    def add(
        self,
        processor_class: type,
        *,
        config: dict[str, Any] | None = None,
        display_name: str | None = None,
    ) -> AddedProcessor:
        """Add a processor class to the graph, configured with `config`."""

    def connect(
        self, source: ProcessorOutputPortReference, destination: ProcessorInputPortReference
    ) -> None:
        """Link one processor's output port to another's input port."""

    # `bind_host` is `...` rather than its literal default because the binding
    # builds that string at call time, which is what the compiled signature
    # reports — the same shape `__exit__` below has.
    def host_control_plane(
        self,
        *,
        bind_host: str = ...,
        bind_port: int = 9000,
        node_name: str | None = None,
    ) -> None:
        """Host the control plane in this process, so the node is discoverable.

        Binds all interfaces (`0.0.0.0`) and port 9000 by default, incrementing
        the port on collision. Opt-in: a runtime that never calls this
        publishes no node-registry entry. Call it before `run()`.
        """

    def run(self) -> None:
        """Run the pipeline until Ctrl-C, SIGTERM or `shutdown()`, then tear down."""

    def wait_until_every_processor_is_running(self, *, timeout: float = 30.0) -> None:
        """Block until every processor in the graph is running.

        Call it before `run()` or from another thread while `run()` blocks — a
        graph that has not started yet is waited through, not refused. A Python
        processor is running once its helper process has registered and wired
        its ports; anything published into the graph before that is dropped by
        the link. Raises `RuntimeError` if a processor failed instead of
        starting — carrying that processor's own refusal text, so a built-in
        that refused at setup is read by name — if `timeout` elapses, or if
        this runtime has already been shut down; and `ValueError` for a
        `timeout` that is negative, NaN, or too large to be a duration.
        """

    def shutdown(self) -> None:
        """Ask the pipeline to stop. Safe from any thread; idempotent."""

    def __enter__(self) -> Runtime: ...
    # `Literal[False]`, not `bool`: `__exit__` never suppresses the exception,
    # and saying so is what lets a checker know that code after a `with` block
    # only runs when the block completed.
    def __exit__(
        self,
        exception_type: type[BaseException] | None = ...,
        exception: BaseException | None = ...,
        traceback: TracebackType | None = ...,
    ) -> Literal[False]: ...

@final
class CapabilityExtensionHost:
    """What a capability extension's `load(host)` hook is handed.

    A wheel declares its hook as a `streamlib.extensions` entry point in its
    `pyproject.toml`, and the engine calls it once in every process taking an
    engine role — the app process as `Runtime()` is constructed, and each
    helper process before the processor's own module is imported. App code
    never constructs one.

    A hook is expected to be cheap and to do no I/O: bring a runtime or a
    device library up, register the capability's name, and return. It must not
    connect, open a device, or block — the app is waiting on `Runtime()` and a
    helper is inside its registration budget. Raising from a hook fails the
    process it was loading into, by design: an extension that half loaded is
    worse than one that refused.
    """

    @property
    def role(self) -> Literal["app", "helper"]:
        """Which role this process takes."""

    def register_capability(self, name: str, version: str) -> None:
        """Declare a capability this wheel brought up.

        The name is unique across every installed distribution: a second
        distribution registering one already taken is refused, naming both. In
        the app process the registration renders under `extensions` in
        `streamlib graph`; in a helper it is the process's own record.
        """

@final
class AddedProcessor:
    """A processor in the graph."""

    @property
    def processor_id(self) -> str: ...
    @property
    def display_name(self) -> str: ...
    def output(self, port_name: str) -> ProcessorOutputPortReference: ...
    def input(self, port_name: str) -> ProcessorInputPortReference: ...
    def __repr__(self) -> str: ...

@final
class ProcessorOutputPortReference:
    """The producing end of a link."""

    def __repr__(self) -> str: ...

@final
class ProcessorInputPortReference:
    """The consuming end of a link."""

    def __repr__(self) -> str: ...

@final
class ProcessorLinkDataAccess:
    """One processor's links. The engine binds it; app code never builds one.

    Constructing one opens a helper process's own data plane, with its own
    iceoryx2 node — only `streamlib._helper` does that.
    """

    def __new__(cls) -> ProcessorLinkDataAccess: ...
    def wire_output_link(
        self,
        port_name: str,
        channel_service_name: str,
        dest_notify_service_name: str,
        expected_payload_bytes: int,
        max_payload_bytes_per_channel: int,
        max_queued_messages: int,
        max_subscribers: int,
        notify_max_notifiers: int,
        link_id: str,
    ) -> None: ...
    def wire_input_link(
        self,
        port_name: str,
        channel_service_name: str,
        notify_service_name: str,
        read_mode: str,
        max_queued_messages: int,
        max_subscribers: int,
        notify_max_notifiers: int,
        link_id: str,
        audio_window: dict[str, Any] | None = None,
    ) -> None: ...
    def unwire_output_link(self, port_name: str, link_id: str) -> None: ...
    def unwire_input_link(self, link_id: str) -> None: ...
    def input_listener_fd(self) -> int | None: ...
    def drain_input_listener(self) -> None: ...
    def any_input_port_has_data(self) -> bool: ...
    @overload
    def read_from_input_port(
        self, port_name: str, *, into: None = None
    ) -> Any | None: ...
    @overload
    def read_from_input_port(
        self, port_name: str, *, into: Callable[..., _BagReadTarget]
    ) -> _BagReadTarget | None: ...
    def read_from_input_port_with_timestamp(
        self, port_name: str
    ) -> tuple[Any, int] | tuple[None, None]: ...
    def input_port_has_data(self, port_name: str) -> bool: ...
    def write_to_output_port(
        self,
        port_name: str,
        bag: Mapping[str, Any],
        timestamp_ns: int | None = None,
    ) -> None: ...

@final
class RuntimeContextFullAccess:
    """Privileged runtime context handed to `setup` / `teardown` / `start` / `stop`.

    Built in the helper process the processor runs in; app code never
    constructs one.
    """

    @property
    def config(self) -> dict[str, Any]: ...
    @property
    def time(self) -> int: ...
    @property
    def inputs(self) -> LinkInputDataReader: ...
    @property
    def outputs(self) -> LinkOutputDataWriter: ...
    @property
    def gpu_limited_access(self) -> GpuContextLimitedAccess: ...
    @property
    def gpu_full_access(self) -> GpuContextFullAccess: ...
    @property
    def runtime_id(self) -> str: ...
    @property
    def processor_id(self) -> str: ...
    def is_paused(self) -> bool: ...
    def should_process(self) -> bool: ...
    @staticmethod
    def open_for_helper_process(
        configuration: Mapping[str, Any],
        link_data_access: ProcessorLinkDataAccess,
        runtime_id: str,
        processor_id: str,
        escalate_request_to_parent: Callable[[dict[str, Any]], dict[str, Any]] | None = None,
    ) -> RuntimeContextFullAccess: ...
    def limited_access_view_for_helper_process(self) -> RuntimeContextLimitedAccess: ...
    def note_pause_state_from_parent(self, paused: bool) -> None: ...

@final
class RuntimeContextLimitedAccess:
    """Restricted runtime context handed to `process` / `on_pause` / `on_resume`.

    `gpu_full_access` is deliberately absent — reaching for it raises
    `AttributeError`, mirroring the Rust capability split.
    """

    @property
    def config(self) -> dict[str, Any]: ...
    @property
    def time(self) -> int: ...
    @property
    def inputs(self) -> LinkInputDataReader: ...
    @property
    def outputs(self) -> LinkOutputDataWriter: ...
    @property
    def gpu_limited_access(self) -> GpuContextLimitedAccess: ...
    @property
    def runtime_id(self) -> str: ...
    @property
    def processor_id(self) -> str: ...
    def is_paused(self) -> bool: ...
    def should_process(self) -> bool: ...

@final
class LinkInputDataReader:
    """A processor's input ports, as `ctx.inputs`."""

    @overload
    def read(self, port_name: str, *, into: None = None) -> Any | None: ...
    @overload
    def read(
        self, port_name: str, *, into: Callable[..., _BagReadTarget]
    ) -> _BagReadTarget | None:
        """The next bag on `port_name`, read into `into`.

        The opt-in strictness dial. A TypedDict casts for free — the bag
        arrives as itself, unvalidated. A dataclass or pydantic model is
        constructed from the bag's entries, so a bag that does not fit raises
        here, at the consuming read.
        """

    def read_with_timestamp(
        self, port_name: str
    ) -> tuple[Any, int] | tuple[None, None]: ...
    @overload
    def read_from_inbound_link(
        self, port_name: str, *, into: None = None
    ) -> tuple[Any, str] | None: ...
    @overload
    def read_from_inbound_link(
        self, port_name: str, *, into: Callable[..., _BagReadTarget]
    ) -> tuple[_BagReadTarget, str] | None:
        """The next bag on `port_name` with the link it arrived on, or `None`.

        Any number of links may enter one input port, and each one is a
        separate producer. This is how a many-input processor tells them
        apart: the name is the source channel the link subscribed to —
        `{source processor id}/{source output port}`, the name `graph` and
        `tap` show — which the engine knows and a producer cannot misstate.

        Bags from one link arrive in that link's order. Nothing is promised
        about how two links interleave, so a reader that needs time order
        reasons per link.

        `into` is the same strictness dial `read` carries.
        """

    @overload
    def read_from_inbound_link_with_timestamp(
        self, port_name: str, *, into: None = None
    ) -> tuple[Any, str, int] | None: ...
    @overload
    def read_from_inbound_link_with_timestamp(
        self, port_name: str, *, into: Callable[..., _BagReadTarget]
    ) -> tuple[_BagReadTarget, str, int] | None:
        """The next bag on `port_name` with its link and its timestamp.

        The fan-in read and the timestamped read at once, which a many-track
        sink needs together: the link names the producer, and the stamp is the
        one that producer wrote — the source frame's instant, not the moment of
        the read. Restating a producer's timing downstream needs both.
        """

    def inbound_link_names(self, port_name: str) -> list[str]:
        """Every link feeding `port_name`, in wiring order.

        Readable in `setup()` — links are wired before it runs — which is how
        a sink learns how many producers it owes before the first bag
        arrives. A port nothing is connected to lists none.
        """

    def has_data(self, port_name: str) -> bool: ...

@final
class LinkOutputDataWriter:
    """A processor's output ports, as `ctx.outputs`."""

    def write(
        self,
        port_name: str,
        bag: Mapping[str, Any],
        timestamp_ns: int | None = None,
    ) -> None:
        """Publish one bag to every downstream link on `port_name`.

        A bag over the channel's payload ceiling is refused here and counted
        against this port, never raised. Loss from a consumer that cannot keep
        up is a separate mechanism and lands at the consuming port.
        """

@final
class GpuContextLimitedAccess:
    """Non-allocating GPU capability, valid for the whole processor life."""

    def acquire_pixel_buffer(
        self, width: int, height: int, format: str = "bgra"
    ) -> GpuSurfaceHandle: ...
    def acquire_texture(
        self, width: int, height: int, format: str, usage: list[str]
    ) -> GpuSurfaceHandle:
        """Acquire a pooled device texture, named by the surface id the engine minted.

        The id is the whole handle: a kernel dispatch binds it, and a downstream
        processor resolves it. `copy_src` and `copy_dst` ride every request, so
        the CPU doors reach the pixels over the surface's host-visible staging
        without the caller spelling a transfer usage.
        """
    def resolve_surface(self, surface_id: str) -> GpuSurfaceHandle: ...
    def claim_surface_against_producer_reuse(
        self, surface_id: str
    ) -> GpuSurfaceCheckOutLease:
        """Claim a published surface until the returned lease is dropped.

        The cheap half of `resolve_surface`: it holds the frame still without
        importing its memory, so an object that wants only the pixels it was
        handed to stay put can keep the lease in a field and let its own
        lifetime do the releasing.
        """

    def surface_can_take_write_back(self, surface_id: str) -> bool:
        """Whether an edit written back into this surface publishes at all.

        The engine's one answer for every write door: a write-back belongs to
        a pooled frame whose allocation is its only backing, or to a
        registered texture that takes a recorded copy in; a frame backed by
        neither answers False. Every texture this processor acquired answers
        True — it can take the copy — so False narrows to a pooled frame its
        producer still owns and a foreign registration without transfer usage.
        `writable()` refuses on this answer; `cpu()` hands its array out
        read-only on it.
        """

    def escalate(self, privileged_callback: Callable[[GpuContextFullAccess], _EscalateResult]) -> _EscalateResult:
        """Refuses: the callback's one atomic privileged scope cannot span a
        process boundary. The operations it wrapped are methods on this
        capability and on `ctx.gpu_full_access` — call them directly."""

@final
class GpuContextFullAccess:
    """The privileged GPU capability a full-access hook receives.

    Each method is its own escalate round trip to the parent, which runs the
    privileged work against the engine and answers with a handle.
    """

    def acquire_pixel_buffer(
        self, width: int, height: int, format: str = "bgra"
    ) -> GpuSurfaceHandle: ...
    def acquire_texture(
        self, width: int, height: int, format: str, usage: list[str]
    ) -> GpuSurfaceHandle:
        """Acquire a pooled device texture through the privileged path.

        The id is the whole handle: a kernel dispatch binds it, and a downstream
        processor resolves it. `copy_src` and `copy_dst` ride every request, so
        the CPU doors reach the pixels over the surface's host-visible staging
        without the caller spelling a transfer usage.
        """

    def create_window(
        self, title: str, width: int = 1280, height: int = 720
    ) -> ProcessorOwnedWindow:
        """Request a window this processor owns, presented by the engine.

        Constructed once in `setup()`, named frames per frame in `process()`.
        The window lives in the app process on its own present loop, so it
        keeps its frame rate whatever this processor's pace is, and naming no
        frame leaves the last one up.

        Raises when the process can get no window at all — no display server,
        or a window event pump that has already failed — rather than handing
        back a window that would show nothing. An author for whom the window
        is optional writes the `try/except`.
        """

    def create_compute_kernel(
        self,
        source: str | None = None,
        spirv: bytes | None = None,
        push_constant_size: int = 0,
        bindings: dict[str, str] | None = None,
        entry_point: str = "main",
    ) -> ComputeKernel:
        """Build a compute kernel from GLSL `source`, or from pre-compiled SPIR-V.

        Constructed once in `setup()`, dispatched per frame in `process()`. The
        engine compiles the source and reflects the shader at construction,
        taking its binding names from it — those names are what `dispatch`
        resolves against. Re-creating an identical kernel is free of
        compilation. Authoring needs no shader toolchain: the compiler is in
        the wheel.

        `source` and `spirv` are alternatives — supply exactly one. A GLSL
        entry point is always `main`; `entry_point` is meaningful only with
        `spirv`.

        `bindings` optionally asserts `{name: kind}` against reflection; each
        kind is one of `sampled_image`, `sampled_texture`, `storage_buffer`,
        `storage_image`, `uniform_buffer`.
        """

    def create_graphics_kernel(
        self,
        color_attachment_formats: Sequence[str],
        vertex_source: str | None = None,
        vertex_spirv: bytes | None = None,
        vertex_entry_point: str = "main",
        fragment_source: str | None = None,
        fragment_spirv: bytes | None = None,
        fragment_entry_point: str = "main",
        push_constant_size: int = 0,
        bindings: dict[str, str | tuple[str, Sequence[str]]] | None = None,
        label: str = "",
        topology: str = "triangle_list",
        polygon_mode: str = "fill",
        cull_mode: str = "none",
        front_face: str = "counter_clockwise",
        line_width: float = 1.0,
        color_write_channels: str = "rgba",
        color_blend: Mapping[str, str] | None = None,
        dynamic_state: str = "viewport_scissor",
    ) -> GraphicsKernel:
        """Build a graphics kernel from GLSL sources, or from pre-compiled SPIR-V.

        Constructed once in `setup()`, drawn per frame in `process()`. The
        engine compiles both stages and reflects them at construction, taking
        its binding names from them — those names are what `draw` resolves
        against. Re-creating an identical kernel is free of compilation.

        Each stage takes `*_source` or `*_spirv`, never both. The vertices are
        the shaders' own: no vertex or index buffer is reachable from a Python
        processor, so a vertex stage fabricates its positions from
        `gl_VertexIndex`. The pass attaches colour targets only, so the
        pipeline carries no depth state.

        `bindings` optionally asserts the shape against reflection — `{name:
        kind}`, or `{name: (kind, stages)}` to assert which stages read a
        binding. Each kind is one of `sampled_texture`, `storage_buffer`,
        `storage_image`, `uniform_buffer`; each stage is `vertex` or
        `fragment`.

        `color_blend` is `None` for no blending, or a mapping of any of
        `src_color_factor`, `dst_color_factor`, `color_op`,
        `src_alpha_factor`, `dst_alpha_factor`, `alpha_op` — the rest default
        to source-alpha-over.
        """

    def create_ray_tracing_kernel(
        self,
        stages: Sequence[Mapping[str, Any]],
        groups: Sequence[Mapping[str, Any]],
        max_recursion_depth: int = 1,
        push_constant_size: int = 0,
        bindings: dict[str, str | tuple[str, Sequence[str]]] | None = None,
        label: str = "",
    ) -> RayTracingKernel:
        """Build a ray-tracing kernel from GLSL sources, or from pre-compiled SPIR-V.

        `stages` is one mapping per shader module — `{"stage": "ray_gen",
        "source": …}`, where `stage` is one of `ray_gen`, `miss`,
        `closest_hit`, `any_hit`, `intersection`, `callable`, and the module
        itself is `source` or `spirv` with an optional `entry_point`.

        `groups` says how the shader binding table is laid out over them:
        `{"kind": "general", "general_stage": 0}`, `{"kind": "triangles_hit",
        "closest_hit_stage": 2}`, or `{"kind": "procedural_hit",
        "intersection_stage": 3}`. A group names its modules by index into
        `stages`, because two modules can fill the same stage.

        `bindings` takes the same shape `create_graphics_kernel` does, plus the
        `acceleration_structure` kind.
        """

    def build_triangles_blas(
        self,
        vertices: Sequence[float],
        indices: Sequence[int],
        label: str = "",
    ) -> AccelerationStructureHandle:
        """Build a bottom-level acceleration structure over triangle geometry.

        `vertices` is `[x, y, z, x, y, z, …]` and `indices` is three per
        triangle. The returned handle is what `build_tlas` places in a scene.
        """

    def build_tlas(
        self,
        instances: Sequence[Mapping[str, Any]],
        label: str = "",
    ) -> AccelerationStructureHandle:
        """Build the top-level acceleration structure a trace binds.

        Each instance names its `blas` and, optionally, the row-major 3×4
        `transform` that places it (12 floats, identity by default), its 8-bit
        `mask`, its 24-bit `custom_index`, its `sbt_record_offset`, and its
        geometry `flags` — some of `triangle_facing_cull_disable`,
        `triangle_flip_facing`, `force_opaque`, `force_no_opaque`.

        The structure keeps every bottom-level one it references alive.
        """

    def kernel_dispatch_batch(self) -> KernelDispatchBatch:
        """Open a scope that records several dispatches and runs them as one.

        The Python equivalent of the engine's command-recorder flow, and why
        dispatch has two entry points: `kernel.dispatch()` for a single pass,
        this for several. A two-pass filter costs one round trip, one
        submission and one fence wait instead of two of each.

        Leaving the scope runs the batch and returns when the GPU work has
        retired, exactly as a single dispatch does. Leaving it by a raise runs
        nothing.
        """

    def export_dma_buf(self, surface: GpuSurfaceHandle) -> tuple[int, int]:
        """Export a DMA-BUF file descriptor for `surface`, as `(fd, byte_size)`.

        The caller owns the fd and must close it, or hand it to something that
        takes ownership. Answered without leaving this process: the fds arrived
        over SCM_RIGHTS when the surface was checked out, and they are the same
        ones a host-side export would mint.

        Refuses by name for an OPAQUE_FD-flavoured texture — that fd imports
        through Vulkan or CUDA external memory, not as a DMA-BUF; export it
        through `export_opaque_fd` instead — and for a pooled-texture handle
        whose memory was never checked out into this process (resolve the
        surface id first).
        """

    def export_opaque_fd(self, surface: GpuSurfaceHandle) -> OpaqueFdTextureExport:
        """Export the OPAQUE_FD texture handle for `surface`, for native code
        that runs its own Vulkan or CUDA external-memory import against the
        allocation.

        The caller owns the returned object's fd: a successful foreign import
        adopts it — never close it after one; always close it after a failed
        one. Consume the texture as an image (CUDA maps the mipmapped array;
        Vulkan recreates the image from the carried recipe) — a linear buffer
        mapping over OPTIMAL-tiled memory yields block-linear bytes, never
        pixels.

        A raw handle names the allocation, never the frame: the surface-id
        lifetime guarantees end at export, and per-frame reach stays with
        surface ids and `as_device_tensor()`. Answered without leaving this
        process: the fd arrived over SCM_RIGHTS when the surface was checked
        out.

        Refuses by name for a DMA-BUF-flavoured texture (use
        `export_dma_buf`), for a pixel buffer, and for a pooled-texture
        handle whose memory was never checked out into this process (resolve
        the surface id first).
        """

    def import_dma_buf(
        self,
        fd: int,
        width: int,
        height: int,
        format: str = "bgra",
        byte_size: int | None = None,
    ) -> GpuSurfaceHandle:
        """Adopt a foreign single-plane DMA-BUF fd as a surface this graph can
        resolve.

        The fd crosses to the engine's surface-share service over SCM_RIGHTS —
        the caller keeps ownership and may close it once this returns. The
        returned handle maps the same memory and travels under a freshly minted
        surface id; closing its last holder removes the registration. When
        `byte_size` is omitted a tight plane is assumed — pass the exporter's
        own byte size whenever the buffer carries row padding. The fd must
        reference host-mappable linear memory (a pixel-buffer export); a
        tiled or device-local exporter's fd fails at the Vulkan import.
        """

    def wait_device_idle(self) -> None: ...
    def escalate(self, privileged_callback: Callable[[GpuContextFullAccess], _EscalateResult]) -> _EscalateResult:
        """Refuses: the callback's one atomic privileged scope cannot span a
        process boundary. The operations it wrapped are methods on this
        capability and on `ctx.gpu_limited_access` — call them directly."""

@final
class GpuSurfaceHandle:
    """An owned GPU surface, and the pixels behind it."""

    @property
    def surface_id(self) -> str: ...
    @property
    def width(self) -> int: ...
    @property
    def height(self) -> int: ...
    @property
    def format(self) -> str: ...
    @property
    def bytes_per_row(self) -> int:
        """Row pitch in bytes, including any padding the allocation carries.

        Over a texture backing it is the staging's pitch, not the tiled
        texture's — the staging is the allocation the CPU addresses. Asking
        maps that staging, which needs no lock and costs one checkout the
        first time this process asks about the surface.
        """

    @property
    def base_address(self) -> int | None:
        """Base address of the host mapping, or None when not locked.

        A surface the CPU cannot address directly opens its staged door here,
        as `as_numpy` and `__dlpack__` do — so the address is the staging's,
        and reading it has read this frame in.
        """

    def close(self) -> None:
        """Release the underlying GPU resource. Idempotent."""

    def __enter__(self) -> GpuSurfaceHandle: ...
    def __exit__(
        self,
        exception_type: type[BaseException] | None = ...,
        exception: BaseException | None = ...,
        traceback: TracebackType | None = ...,
    ) -> Literal[False]: ...
    def lock(self, read_only: bool = True) -> None:
        """Open CPU access, declaring read or write intent.

        Performs no wait — ordering against the producer comes from
        publication, since a source finishes its GPU work before it sends the
        frame on. `read_only=False` marks an exported tensor writable.

        Never refused for want of a host mapping: a texture-backed surface
        reaches its pixels through the engine's host-visible staging, which
        the first host-side accessor inside the lock checks out and reads this
        frame into.
        """

    def unlock(self) -> None:
        """Close CPU access, publishing any pending staged write back into the
        surface first — through whichever staging holds the edit. Idempotent.
        """

    def as_numpy(self) -> Any:
        """A numpy view over the surface's pixels. Requires a lock.

        Shares memory with a surface the CPU can address directly; over a
        texture backing it is the surface's staging, read in on entry and
        published at `unlock()` when the lock declared a write.
        """

    def as_device_tensor(self) -> GpuSurfaceDeviceTensorScope:
        """The scoped device-tensor view over this surface's pixels.

        Entering blits the surface to a linear DLPack view a third-party
        GPU package writes in place; leaving normally blits the write
        back, ordered by the engine ahead of its next read; leaving by a
        propagating exception discards it, and the surface keeps the
        frame it already held.
        """

    def __dlpack_device__(self) -> tuple[int, int]: ...
    def __dlpack__(
        self,
        stream: Any | None = ...,
        max_version: tuple[int, int] | None = ...,
        dl_device: tuple[int, int] | None = ...,
        copy: bool | None = ...,
    ) -> Any:
        """A DLPack capsule over the pixels. Requires a lock.

        A graph frame's natural side is the device: with a usable CUDA
        runtime the tensor is GPU-resident (one engine-side blit into an
        exportable staging buffer — zero CPU copies, never claimed
        copy-free); otherwise, or with `dl_device=(1, 0)`, it is the host
        mapping. A writable device tensor's edits publish back to the
        surface at `unlock()`.

        The tensor may outlive this handle: it holds its own share of the
        surface, so the pool slot is not reused until the tensor is released.
        """

@final
class GpuSurfaceDeviceTensorScope:
    """A scope handing a surface's pixels to a third-party GPU package.

    Entering blits the surface to a linear DLPack view; leaving normally
    blits any write back, ordered ahead of the engine's next read; leaving
    by a propagating exception discards the write and the surface keeps
    the frame it already held. The engine owns the ordering — no fence or
    timeline vocabulary appears here, and no `torch.cuda.synchronize()` is
    owed before leaving.

    Independent of `lock()` by design: entering the scope is the write
    declaration. A surface whose export cannot take a write-back — a pool
    member its producer still owns, or a texture acquired without
    `copy_dst` usage — refuses at `__enter__` rather than discarding edits
    silently.
    """

    def __enter__(self) -> GpuSurfaceDeviceTensorScope: ...
    def __exit__(
        self,
        exception_type: type[BaseException] | None = ...,
        exception: BaseException | None = ...,
        traceback: TracebackType | None = ...,
    ) -> Literal[False]: ...
    def __dlpack_device__(self) -> tuple[int, int]: ...
    def __dlpack__(
        self,
        stream: Any | None = ...,
        max_version: tuple[int, int] | None = ...,
        dl_device: tuple[int, int] | None = ...,
        copy: bool | None = ...,
    ) -> Any:
        """A DLPack capsule over the blitted view — what `torch.from_dlpack`
        consumes. Always writable — a read-only export was refused at
        `__enter__` — and minting one arms the blit-back on leaving the
        scope normally.
        """

@final
class GpuSurfaceCheckOutLease:
    """A claim on a published surface, held for as long as this object is.

    While a claim is outstanding the pool never rehands that surface's slot to
    its producer, and dropping this object is the release — there is nothing to
    call. Claims are counted, so holding one and resolving the same surface for
    its pixels are independent.
    """

    @property
    def surface_id(self) -> str:
        """The surface this claim holds still."""

@final
class OpaqueFdTextureExport:
    """A raw OPAQUE_FD texture handle: the allocation's memory fd plus the
    allocation-stable shape a foreign Vulkan or CUDA external-memory import
    must reproduce.

    Deliberately outside the `GpuSurface*` family prefix: the object names an
    allocation, never a frame-bearing surface — the surface-id lifetime
    guarantees end at export.
    """

    @property
    def fd(self) -> int:
        """The exported memory fd. The caller owns it: a successful foreign
        import adopts it — never close it after one; always close it after a
        failed one.
        """

    @property
    def allocation_byte_size(self) -> int:
        """Byte size of the whole `VkDeviceMemory` at offset zero — what the
        foreign import states, never a tight width x height x bpp figure.
        """

    @property
    def width(self) -> int:
        """Texture width in pixels."""

    @property
    def height(self) -> int:
        """Texture height in pixels."""

    @property
    def format(self) -> str:
        """The engine's format name for the texture, e.g. `"rgba16_float"`."""

    @property
    def vk_image_tiling(self) -> int:
        """Raw `VkImageTiling` the exporter created the image with."""

    @property
    def vk_image_usage_flags(self) -> int:
        """Raw `VkImageUsageFlags` bitfield the exporter created the image
        with.
        """

    @property
    def vk_image_mip_levels(self) -> int:
        """`VkImageCreateInfo.mipLevels` of the exporter's image."""

    @property
    def vk_image_array_layers(self) -> int:
        """`VkImageCreateInfo.arrayLayers` of the exporter's image."""

    @property
    def vk_image_samples(self) -> int:
        """Raw `VkSampleCountFlagBits` of the exporter's image."""

    @property
    def dedicated_allocation(self) -> bool:
        """Whether the allocation is dedicated — always true for this
        flavour. A Vulkan importer chains `VkMemoryDedicatedAllocateInfo`, a
        CUDA importer sets `cudaExternalMemoryDedicated`; omitting either is
        undefined behaviour, not leniency.
        """

    @property
    def vk_memory_type_index(self) -> int:
        """The exporter's Vulkan memory type index, for the importer-side
        `vkAllocateMemory(VkImportMemoryFdInfoKHR)`.
        """

    @property
    def exporting_device_uuid(self) -> bytes:
        """The exporting device's `VkPhysicalDeviceIDProperties.deviceUUID`,
        16 bytes. An OPAQUE_FD is device-bound: importing on the wrong GPU of
        a multi-GPU rig corrupts silently, so match this against the
        importer's own device UUID first.
        """

@final
class ComputeKernel:
    """A compute kernel the engine built and holds, dispatched by name.

    Constructed in `setup()` where the capability is Full, dispatched per frame
    in `process()`. No kernel handle string, fence, timeline or slot number
    reaches Python — the object is the handle.
    """

    @property
    def binding_names(self) -> list[str]:
        """The shader's own names for this kernel's bindings, in slot order."""

    def dispatch(
        self,
        bindings: dict[str, GpuSurfaceHandle | str],
        group_count: tuple[int, int, int],
        push_constants: bytes | None = None,
    ) -> None:
        """Dispatch, binding each of the shader's declared resources by name.

        Bindings never persist on the kernel, so every dispatch supplies all of
        them: there is no implicit default and no value carried over from the
        previous frame. Supplying an unknown name or omitting a declared one
        raises before anything is submitted. Each binding's kind comes from the
        shader's own reflection, never from the caller.

        Returns when the GPU work has retired and the writes are visible.
        """

@final
class ProcessorOwnedWindow:
    """A window this processor owns, presented by the engine at vsync.

    Constructed in `setup()` through `ctx.gpu_full_access.create_window(...)`;
    named frames per frame in `process()`. No window handle, swapchain or
    present thread reaches Python — the object is the handle.
    """

    @property
    def title(self) -> str:
        """The title this window was requested with."""

    @property
    def is_closed(self) -> bool:
        """Whether the window has closed — by the user's gesture or this
        owner's own `close()`.

        Reflects what the last answered call reported, so it needs no round
        trip of its own; `drain_events()` and `show()` keep it current.
        """

    def show(
        self, frame_or_surface_to_show: ClaimedSurfacePixelAccess | GpuSurfaceHandle | str
    ) -> None:
        """Name the frame this window shows next.

        Takes anything that names a published surface: a cast object read with
        `ctx.inputs.read(port, into=T)` — whose claim is what guarantees the id
        un-recycled — a `GpuSurfaceHandle` a kernel wrote, or a bare surface id.
        Returns without waiting for the frame to be shown: the window presents
        at vsync, latest-wins, and naming nothing leaves the last frame up.

        A bare id names a **texture-backed** surface only, and so does a cast
        type declaring no `width`/`height`: naming no extent is how a caller
        says it knows nothing else about the surface, and the engine reads that
        as refusing a buffer-backed one. Such a frame does not draw — the
        window keeps what it last had, and the engine logs it once per pool
        slot rather than raising here. A camera or a test pattern publishes
        buffer-backed frames; name those with the cast object.

        A no-op once the window has closed, never an error — a user gesture
        does not take a pipeline down. The argument is still read, so a call
        that names no surface at all is refused whether the window is open or
        shut.
        """

    def drain_events(self) -> ProcessorOwnedWindowEvents:
        """Take this window's coalesced state.

        Polling is optional — an owner that never drains still presents; it
        only learns of a resize or a close from the next `show()`.
        """

    def close(self) -> None:
        """Close this window and release its present thread.

        Never an error for a window already closed, and never required: the
        engine closes what a processor still owns at teardown.
        """

    def __repr__(self) -> str: ...

@final
class ProcessorOwnedWindowEvents:
    """The coalesced state one `drain_events()` took off a window."""

    @property
    def current_width_in_physical_pixels(self) -> int: ...
    @property
    def current_height_in_physical_pixels(self) -> int: ...
    @property
    def close_requested_by_user(self) -> bool:
        """Whether the user asked to close this window since the last drain.

        True exactly once per gesture — this drain reported it and cleared it.
        The engine has already closed the window by the time an owner reads
        this, so it is reacted to and never vetoed.
        """

    @property
    def window_is_closed(self) -> bool:
        """Whether the engine has closed this window. Sticky once true."""

    def __repr__(self) -> str: ...

@final
class GraphicsKernel:
    """A graphics kernel the engine built and holds, drawn by name.

    Constructed in `setup()` where the capability is Full, drawn per frame in
    `process()`. No kernel handle string, fence, timeline or slot number
    reaches Python — the object is the handle.
    """

    @property
    def binding_names(self) -> list[str]:
        """The shaders' own names for this kernel's bindings, in slot order."""

    def draw(
        self,
        bindings: dict[str, GpuSurfaceHandle | str],
        color_targets: Sequence[GpuSurfaceHandle | str],
        extent: tuple[int, int],
        vertex_count: int,
        instance_count: int = 1,
        first_vertex: int = 0,
        first_instance: int = 0,
        push_constants: bytes | None = None,
    ) -> None:
        """Render one offscreen pass, binding each declared resource by name.

        Exactly one colour target, `extent` pixels of it. The pass discards
        what the target held and starts from transparent black, so a draw
        paints the whole frame it publishes.

        Bindings never persist on the kernel, so every draw supplies all of
        them. Supplying an unknown name or omitting a declared one raises
        before anything is submitted. Each binding's kind comes from the
        shaders' own reflection, never from the caller.

        Returns when the GPU work has retired and the pixels are visible.
        """

@final
class RayTracingKernel:
    """A ray-tracing kernel the engine built and holds, traced by name.

    Constructed in `setup()` where the capability is Full, traced per frame in
    `process()`. No kernel handle string, fence, timeline or slot number
    reaches Python — the object is the handle.
    """

    @property
    def binding_names(self) -> list[str]:
        """The shaders' own names for this kernel's bindings, in slot order."""

    def trace(
        self,
        bindings: dict[str, GpuSurfaceHandle | AccelerationStructureHandle | str],
        grid: tuple[int, int, int],
        push_constants: bytes | None = None,
    ) -> None:
        """Trace a `(width, height, depth)` grid of rays.

        An `acceleration_structure` binding takes the handle `build_tlas`
        returned; every other kind takes a surface. Bindings never persist on
        the kernel, so every trace supplies all of them, and an unknown or
        omitted name raises before anything is submitted.

        Returns when the GPU work has retired and the writes are visible.
        """

@final
class AccelerationStructureHandle:
    """An acceleration structure the engine built and holds.

    The object is the handle: a bottom-level structure is placed in a scene by
    `build_tlas`, and the top-level one it returns is what a trace binds. No id
    string reaches Python, and nothing publishes an acceleration structure for
    another processor to resolve.

    The engine holds the structure's device memory for as long as this object
    lives, and releases it when the last reference goes away. A scene keeps
    every bottom-level structure it instances alive, so dropping a BLAS a live
    TLAS uses frees nothing until the TLAS goes too.
    """

    @property
    def label(self) -> str:
        """The name this structure was built under, as engine logs show it."""

@final
class KernelDispatchBatch:
    """Several dispatches recorded as one: one submission, one fence wait.

    A two-pass filter dispatching on its own pays the round trip, the
    submission and the stall twice; inside this scope it pays each once.
    Leaving the scope normally runs the batch — leaving it by a raise runs
    nothing, because half of a multi-pass filter is not what the author wrote.

    Nothing about the synchronous contract changes: the scope returns when the
    GPU work has retired and the writes are visible.
    """

    def __enter__(self) -> KernelDispatchBatch: ...
    def __exit__(
        self,
        exception_type: type[BaseException] | None = ...,
        exception: BaseException | None = ...,
        traceback: TracebackType | None = ...,
    ) -> Literal[False]: ...
    def dispatch(
        self,
        kernel: ComputeKernel,
        bindings: dict[str, GpuSurfaceHandle | str],
        group_count: tuple[int, int, int],
        push_constants: bytes | None = None,
    ) -> None:
        """Add a dispatch to this batch.

        The receiver is explicit because a batch dispatches several kernels;
        `kernel.dispatch()` names its own. Bindings are checked here, so a name
        the shader does not declare or a wrong push-constant size raises at
        this line rather than when the scope closes.

        One kernel may appear only once per batch: a kernel owns a single
        descriptor set, so dispatching it again would give its earlier dispatch
        these bindings.
        """

@final
class MonotonicTimer:
    """Drift-free periodic timer backed by `timerfd_create(CLOCK_MONOTONIC)`.

    The first absolute deadline is `now + interval`, then `TFD_TIMER_ABSTIME`
    repeats, so ticks never accumulate drift.
    """

    def __new__(cls, interval_ns: int) -> MonotonicTimer: ...
    @property
    def interval_ns(self) -> int: ...
    def wait(self, timeout_ms: int = 100) -> int:
        """Wait up to `timeout_ms` for the next tick.

        Returns a positive expiration count when a tick fired, 0 on timeout,
        -1 once closed.
        """

    def close(self) -> None:
        """Release the timer's file descriptor. Idempotent."""

    def __enter__(self) -> MonotonicTimer: ...
    def __exit__(
        self,
        exception_type: type[BaseException] | None = ...,
        exception: BaseException | None = ...,
        traceback: TracebackType | None = ...,
    ) -> Literal[False]: ...

def gpu_limited_access_of_the_typed_read_in_progress() -> GpuContextLimitedAccess | None:
    """The GPU capability of the `read(port, into=T)` currently constructing an
    object, or `None` when nothing is being read into a type.

    The same capability as `ctx.gpu_limited_access`, offered so a type can do
    per-frame work at construction that needs the engine — claiming the frame's
    surface against producer reuse is what the shipped `VideoFrame` does with
    it. Any class reachable through `into=` may call this; there is no
    registration, no marker and no privileged type.
    """

def decode_tapped_channel_bag_frame_to_python_object(
    framed_bag_bytes: bytes,
) -> Any:
    """Decode one raw bag a `tap` forwarded — transport-framed msgpack — into
    ordinary Python data.

    The bytes a tap hands back are the channel's wire bytes verbatim, header
    included; this reads exactly the payload the header declares. Refuses a bag
    shorter than its own declared length rather than returning the prefix that
    did arrive.
    """

def encode_bag_to_msgpack_bytes(bag: Mapping[str, Any]) -> bytes:
    """Encode a bag to the msgpack bytes the wire carries, for a caller — an
    extension wheel with its own transport — that carries them itself.

    The engine's one bag codec, reachable: a dict with string keys at every
    level, values from `dict`, `list`, `tuple`, `str`, `bytes`, `int`, `float`,
    `bool` and `None`, `bytes` as msgpack `bin` at 1×. Anything else raises
    `TypeError`, and an integer wider than 64 bits raises `ValueError`.
    """

def decode_msgpack_bytes_to_python_object(msgpack_bytes: bytes) -> Any:
    """Decode msgpack bytes into ordinary Python data.

    Unlike `decode_tapped_channel_bag_frame_to_python_object` these are payload
    bytes with no transport frame header in front of them. Nesting is bounded
    at decode, so bytes from an untrusted peer cannot recurse without limit.
    """

def capability_extension_host_for_the_app_process(
    distribution: str,
) -> CapabilityExtensionHost:
    """Mint the host `distribution`'s hook is handed in the app process."""

def capability_extension_host_for_the_helper_process(
    distribution: str,
) -> CapabilityExtensionHost:
    """Mint the host `distribution`'s hook is handed in a helper process."""

def monotonic_now_ns() -> int:
    """Current monotonic time in nanoseconds via `clock_gettime(CLOCK_MONOTONIC)`."""

def runtime_log_directory() -> Path:
    """The directory the engine writes its per-runtime JSONL logs into."""

def open_test_harness_channel(channel: str) -> None:
    """Open a test-harness channel; raises if the name is already in use."""

def close_test_harness_channel(channel: str) -> None:
    """Close a test-harness channel, dropping anything still queued on it."""

def feed_test_harness_bag(channel: str, bag: Any) -> None:
    """Queue one bag for delivery through `channel`'s feeder."""

def await_test_harness_bag(channel: str, timeout_seconds: float) -> Any | None:
    """The next bag collected on `channel`, or `None` if the wait ran out."""

def log_event(
    level: str, message: str, attrs: dict[str, Any] | None = None
) -> None:
    """Emit one record on the engine's log pipeline, with structured attrs."""

