# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""StreamLib — a realtime streaming engine with Python authoring.

The engine runs in this interpreter's process: `Runtime()` boots it, `rt.add`
puts processors in its graph, `rt.connect` links them, and `rt.run()` blocks
until Ctrl-C with the GIL released. Processors declare identity and ports with
`@processor` / `@input` / `@output` and receive a capability-typed context in
every lifecycle hook.
"""

import atexit
import weakref
from functools import partial

from . import clock as clock
from . import log as log
from ._capability_extensions import (
    load_installed_capability_extensions_once_per_process,
)
from ._engine import AddedProcessor as AddedProcessor
from ._engine import CapabilityExtensionHost as CapabilityExtensionHost
from ._engine import capability_extension_host_for_the_app_process
from ._engine import GpuContextFullAccess as GpuContextFullAccess
from ._engine import GpuContextLimitedAccess as GpuContextLimitedAccess
from ._engine import GpuSurfaceCheckOutLease as GpuSurfaceCheckOutLease
from ._engine import GpuSurfaceDeviceTensorScope as GpuSurfaceDeviceTensorScope
from ._engine import GpuSurfaceHandle as GpuSurfaceHandle
from ._engine import LinkInputDataReader as LinkInputDataReader
from ._engine import LinkOutputDataWriter as LinkOutputDataWriter
from ._engine import MonotonicTimer as MonotonicTimer
from ._engine import OpaqueFdTextureExport as OpaqueFdTextureExport
from ._engine import ProcessorInputPortReference as ProcessorInputPortReference
from ._engine import ProcessorLinkDataAccess as ProcessorLinkDataAccess
from ._engine import ProcessorOutputPortReference as ProcessorOutputPortReference
from ._engine import ProcessorOwnedWindow as ProcessorOwnedWindow
from ._engine import ProcessorOwnedWindowEvents as ProcessorOwnedWindowEvents
from ._engine import CameraSource as CameraSource
from ._engine import DisplayWindow as DisplayWindow
from ._engine import H264Decoder as H264Decoder
from ._engine import H264Encoder as H264Encoder
from ._engine import H265Decoder as H265Decoder
from ._engine import H265Encoder as H265Encoder
from ._engine import MicrophoneSource as MicrophoneSource
from ._engine import Mp4Sink as Mp4Sink
from ._engine import OpusDecoder as OpusDecoder
from ._engine import OpusEncoder as OpusEncoder
from ._engine import Runtime as _NativeRuntime
from ._engine import SpeakerSink as SpeakerSink
from ._engine import RuntimeContextFullAccess as RuntimeContextFullAccess
from ._engine import RuntimeContextLimitedAccess as RuntimeContextLimitedAccess
from ._engine import TestPatternSource as TestPatternSource
from ._engine import (
    gpu_limited_access_of_the_typed_read_in_progress as gpu_limited_access_of_the_typed_read_in_progress,
)
from ._engine import monotonic_now_ns as monotonic_now_ns
from ._processor_declaration import AudioWindowContract as AudioWindowContract
from ._processor_declaration import input as input  # noqa: A004 — deliberate, see below
from ._processor_declaration import output as output
from ._processor_declaration import processor as processor
from .audio_block import AudioBlock as AudioBlock
from .claimed_surface_pixel_access import (
    ClaimedSurfacePixelAccess as ClaimedSurfacePixelAccess,
)
from .claimed_surface_pixel_access import (
    PixelAccessToOneClaimedSurface as PixelAccessToOneClaimedSurface,
)
from .encoded_audio_packet import EncodedAudioPacket as EncodedAudioPacket
from .encoded_video_frame import EncodedVideoFrame as EncodedVideoFrame
from .processor_output_texture_ring import (
    ProcessorOutputTextureRing as ProcessorOutputTextureRing,
)
from .video_frame import ColorInfo as ColorInfo
from .video_frame import ContentLight as ContentLight
from .video_frame import MasteringDisplay as MasteringDisplay
from .video_frame import VideoFrame as VideoFrame

# `input` and `output` shadow the builtins at module scope on purpose — the
# authoring grammar reads `@input(...)` / `@output(...)`, matching the old SDK.
__all__ = [
    "AddedProcessor",
    "AudioBlock",
    "AudioWindowContract",
    "CameraSource",
    "CapabilityExtensionHost",
    "ClaimedSurfacePixelAccess",
    "ColorInfo",
    "ContentLight",
    "DisplayWindow",
    "EncodedAudioPacket",
    "EncodedVideoFrame",
    "GpuContextFullAccess",
    "GpuContextLimitedAccess",
    "GpuSurfaceCheckOutLease",
    "GpuSurfaceDeviceTensorScope",
    "GpuSurfaceHandle",
    "H264Decoder",
    "H264Encoder",
    "H265Decoder",
    "H265Encoder",
    "LinkInputDataReader",
    "LinkOutputDataWriter",
    "MasteringDisplay",
    "MicrophoneSource",
    "MonotonicTimer",
    "Mp4Sink",
    "OpaqueFdTextureExport",
    "OpusDecoder",
    "OpusEncoder",
    "PixelAccessToOneClaimedSurface",
    "ProcessorInputPortReference",
    "ProcessorLinkDataAccess",
    "ProcessorOutputPortReference",
    "ProcessorOutputTextureRing",
    "ProcessorOwnedWindow",
    "ProcessorOwnedWindowEvents",
    "Runtime",
    "RuntimeContextFullAccess",
    "RuntimeContextLimitedAccess",
    "SpeakerSink",
    "TestPatternSource",
    "VideoFrame",
    "clock",
    "gpu_limited_access_of_the_typed_read_in_progress",
    "input",
    "log",
    "monotonic_now_ns",
    "output",
    "processor",
]


# Engine threads must be joined before CPython finalizes. `Runtime.run()` does
# that on its own; this covers the paths where `run()` never returns normally —
# an exception between construction and `run()`, or an interpreter exiting while
# a Runtime is still referenced, where `__del__` ordering is not guaranteed.
_live_runtimes: "weakref.WeakSet[Runtime]" = weakref.WeakSet()


class Runtime(_NativeRuntime):
    """The engine, running in this process."""

    def __init__(self) -> None:
        super().__init__()
        # Registered before the hooks run, not after: a hook that raises leaves
        # a constructed engine behind whose threads still need joining, and the
        # `atexit` teardown below only reaches a Runtime it knows about.
        _live_runtimes.add(self)
        load_installed_capability_extensions_once_per_process(
            partial(capability_extension_host_for_the_app_process, self)
        )


@atexit.register
def _shut_down_live_runtimes() -> None:
    # Copied first: `shutdown()` can drop the last reference to a Runtime, and
    # mutating the WeakSet while iterating it would raise.
    for runtime in list(_live_runtimes):
        runtime.shutdown()
