# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""What a stream or node module imports.

A stream is a `@stream` function that adds, links and exposes nodes on a
`Stream`; `compile_stream_to_graph` returns the graph it builds. A node
declares its identity and ports with `@node` / `@input` / `@output` and
receives a capability-typed context in every lifecycle hook. The contexts, the
GPU classes, the built-in nodes and the bag codec are the engine's own, from
`tatolab.runtime._engine`.
"""

from tatolab.runtime._engine import CameraSource as CameraSource
from tatolab.runtime._engine import DisplayWindow as DisplayWindow
from tatolab.runtime._engine import GpuContextFullAccess as GpuContextFullAccess
from tatolab.runtime._engine import GpuContextLimitedAccess as GpuContextLimitedAccess
from tatolab.runtime._engine import GpuSurfaceCheckOutLease as GpuSurfaceCheckOutLease
from tatolab.runtime._engine import (
    GpuSurfaceDeviceTensorScope as GpuSurfaceDeviceTensorScope,
)
from tatolab.runtime._engine import GpuSurfaceHandle as GpuSurfaceHandle
from tatolab.runtime._engine import H264Decoder as H264Decoder
from tatolab.runtime._engine import H264Encoder as H264Encoder
from tatolab.runtime._engine import H265Decoder as H265Decoder
from tatolab.runtime._engine import H265Encoder as H265Encoder
from tatolab.runtime._engine import IOSurfaceMachPortExport as IOSurfaceMachPortExport
from tatolab.runtime._engine import LinkInputDataReader as LinkInputDataReader
from tatolab.runtime._engine import LinkOutputDataWriter as LinkOutputDataWriter
from tatolab.runtime._engine import MicrophoneSource as MicrophoneSource
from tatolab.runtime._engine import MonotonicTimer as MonotonicTimer
from tatolab.runtime._engine import Mp4Sink as Mp4Sink
from tatolab.runtime._engine import ProcessorLinkDataAccess as ProcessorLinkDataAccess
from tatolab.runtime._engine import ProcessorOwnedWindow as ProcessorOwnedWindow
from tatolab.runtime._engine import ProcessorOwnedWindowEvents as ProcessorOwnedWindowEvents
from tatolab.runtime._engine import OpaqueFdTextureExport as OpaqueFdTextureExport
from tatolab.runtime._engine import OpusDecoder as OpusDecoder
from tatolab.runtime._engine import OpusEncoder as OpusEncoder
from tatolab.runtime._engine import RuntimeContextFullAccess as RuntimeContextFullAccess
from tatolab.runtime._engine import (
    RuntimeContextLimitedAccess as RuntimeContextLimitedAccess,
)
from tatolab.runtime._engine import SpeakerSink as SpeakerSink
from tatolab.runtime._engine import TestPatternSource as TestPatternSource
from tatolab.runtime._engine import VirtualCameraSink as VirtualCameraSink
from tatolab.runtime._engine import (
    decode_msgpack_bytes_to_python_object as decode_msgpack_bytes_to_python_object,
)
from tatolab.runtime._engine import (
    encode_bag_to_msgpack_bytes as encode_bag_to_msgpack_bytes,
)
from tatolab.runtime._engine import (
    gpu_limited_access_of_the_typed_read_in_progress as gpu_limited_access_of_the_typed_read_in_progress,
)
from tatolab.runtime._engine import monotonic_now_ns as monotonic_now_ns

from . import clock as clock
from . import log as log
from ._processor_declaration import AudioWindowContract as AudioWindowContract
from ._processor_declaration import input as input  # noqa: A004 — deliberate, see below
from ._processor_declaration import node as node
from ._processor_declaration import output as output
from ._stream_graph_builder import NodeInputPortReference as NodeInputPortReference
from ._stream_graph_builder import (
    NodeOutputPortReference as NodeOutputPortReference,
)
from ._stream_graph_builder import NodeReference as NodeReference
from ._stream_graph_builder import Stream as Stream
from ._stream_graph_builder import compile_stream_to_graph as compile_stream_to_graph
from ._stream_graph_builder import stream as stream
from .audio_block import AudioBlock as AudioBlock
from .claimed_surface_pixel_access import (
    ClaimedSurfacePixelAccess as ClaimedSurfacePixelAccess,
)
from .claimed_surface_pixel_access import (
    PixelAccessToOneClaimedSurface as PixelAccessToOneClaimedSurface,
)
from .encoded_audio_packet import EncodedAudioPacket as EncodedAudioPacket
from .encoded_video_frame import EncodedVideoFrame as EncodedVideoFrame
from .glsl_pixel_effect import GlslPixelEffect as GlslPixelEffect
from .glsl_pixel_effect import GlslPixelEffectDialType as GlslPixelEffectDialType
from .model_input_tensor_kernel import ModelInputTensor as ModelInputTensor
from .model_input_tensor_kernel import (
    ModelInputTensorChannelOrder as ModelInputTensorChannelOrder,
)
from .model_input_tensor_kernel import ModelInputTensorDtype as ModelInputTensorDtype
from .model_input_tensor_kernel import ModelInputTensorFit as ModelInputTensorFit
from .model_input_tensor_kernel import (
    ModelInputTensorGeometry as ModelInputTensorGeometry,
)
from .model_input_tensor_kernel import ModelInputTensorKernel as ModelInputTensorKernel
from .model_input_tensor_kernel import ModelInputTensorLayout as ModelInputTensorLayout
from .node_output_texture_ring import (
    ProcessorOutputTextureRing as ProcessorOutputTextureRing,
)
from .video_frame import ColorInfo as ColorInfo
from .video_frame import ContentLight as ContentLight
from .video_frame import MasteringDisplay as MasteringDisplay
from .video_frame import VideoFrame as VideoFrame

# `input` and `output` shadow the builtins at module scope on purpose — the
# authoring grammar reads `@input(...)` / `@output(...)`.
__all__ = [
    "AudioBlock",
    "AudioWindowContract",
    "CameraSource",
    "ClaimedSurfacePixelAccess",
    "ColorInfo",
    "ContentLight",
    "DisplayWindow",
    "EncodedAudioPacket",
    "EncodedVideoFrame",
    "GlslPixelEffect",
    "GlslPixelEffectDialType",
    "GpuContextFullAccess",
    "GpuContextLimitedAccess",
    "GpuSurfaceCheckOutLease",
    "GpuSurfaceDeviceTensorScope",
    "GpuSurfaceHandle",
    "H264Decoder",
    "H264Encoder",
    "H265Decoder",
    "H265Encoder",
    "IOSurfaceMachPortExport",
    "LinkInputDataReader",
    "LinkOutputDataWriter",
    "MasteringDisplay",
    "MicrophoneSource",
    "ModelInputTensor",
    "ModelInputTensorChannelOrder",
    "ModelInputTensorDtype",
    "ModelInputTensorFit",
    "ModelInputTensorGeometry",
    "ModelInputTensorKernel",
    "ModelInputTensorLayout",
    "MonotonicTimer",
    "Mp4Sink",
    "NodeInputPortReference",
    "ProcessorLinkDataAccess",
    "NodeOutputPortReference",
    "ProcessorOutputTextureRing",
    "ProcessorOwnedWindow",
    "ProcessorOwnedWindowEvents",
    "NodeReference",
    "OpaqueFdTextureExport",
    "OpusDecoder",
    "OpusEncoder",
    "PixelAccessToOneClaimedSurface",
    "RuntimeContextFullAccess",
    "RuntimeContextLimitedAccess",
    "SpeakerSink",
    "Stream",
    "TestPatternSource",
    "VideoFrame",
    "VirtualCameraSink",
    "clock",
    "compile_stream_to_graph",
    "decode_msgpack_bytes_to_python_object",
    "encode_bag_to_msgpack_bytes",
    "gpu_limited_access_of_the_typed_read_in_progress",
    "input",
    "log",
    "monotonic_now_ns",
    "node",
    "output",
    "stream",
]
