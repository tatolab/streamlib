# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""What a stream or node module imports.

A stream is a `@stream` function that adds, links and exposes nodes on a
`StreamBuilder`; `compile_stream_to_graph` returns the graph it builds. A node
declares its execution and ports with `@node` / `@node.input` / `@node.output` and
receives a capability-typed context in every lifecycle hook. The contexts, link
endpoints, GPU classes, windows and timer a node is handed while it runs are
`typing.Protocol`s the runtime implements; the clock, the timer's start and the
bag codec are functions that reach the runtime when called, and raise
`RuntimeError` naming themselves where no runtime is lent. Everything else here
is pure Python, so a stream imports, type-checks and compiles with no runtime
installed. The built-in nodes are generated from the runtime's own
declarations, each with a `TypedDict` for its config.
"""

from . import clock as clock
from . import log as log
from ._bag_codec import (
    decode_msgpack_bytes_to_python_object as decode_msgpack_bytes_to_python_object,
)
from ._bag_codec import encode_bag_to_msgpack_bytes as encode_bag_to_msgpack_bytes
from ._built_in_nodes import CameraSource as CameraSource
from ._built_in_nodes import CameraSourceConfig as CameraSourceConfig
from ._built_in_nodes import DisplayWindow as DisplayWindow
from ._built_in_nodes import DisplayWindowConfig as DisplayWindowConfig
from ._built_in_nodes import H264Decoder as H264Decoder
from ._built_in_nodes import H264DecoderConfig as H264DecoderConfig
from ._built_in_nodes import H264Encoder as H264Encoder
from ._built_in_nodes import H264EncoderConfig as H264EncoderConfig
from ._built_in_nodes import H265Decoder as H265Decoder
from ._built_in_nodes import H265DecoderConfig as H265DecoderConfig
from ._built_in_nodes import H265Encoder as H265Encoder
from ._built_in_nodes import H265EncoderConfig as H265EncoderConfig
from ._built_in_nodes import MicrophoneSource as MicrophoneSource
from ._built_in_nodes import MicrophoneSourceConfig as MicrophoneSourceConfig
from ._built_in_nodes import Mp4Sink as Mp4Sink
from ._built_in_nodes import Mp4SinkConfig as Mp4SinkConfig
from ._built_in_nodes import OpusDecoder as OpusDecoder
from ._built_in_nodes import OpusDecoderConfig as OpusDecoderConfig
from ._built_in_nodes import OpusEncoder as OpusEncoder
from ._built_in_nodes import OpusEncoderConfig as OpusEncoderConfig
from ._built_in_nodes import SpeakerSink as SpeakerSink
from ._built_in_nodes import SpeakerSinkConfig as SpeakerSinkConfig
from ._built_in_nodes import TestPatternSource as TestPatternSource
from ._built_in_nodes import TestPatternSourceConfig as TestPatternSourceConfig
from ._built_in_nodes import VirtualCameraSink as VirtualCameraSink
from ._built_in_nodes import VirtualCameraSinkConfig as VirtualCameraSinkConfig
from ._gpu_protocols import AccelerationStructureHandle as AccelerationStructureHandle
from ._gpu_protocols import ComputeKernel as ComputeKernel
from ._gpu_protocols import GpuContextFullAccess as GpuContextFullAccess
from ._gpu_protocols import GpuContextLimitedAccess as GpuContextLimitedAccess
from ._gpu_protocols import GpuSurfaceCheckOutLease as GpuSurfaceCheckOutLease
from ._gpu_protocols import (
    GpuSurfaceDeviceTensorScope as GpuSurfaceDeviceTensorScope,
)
from ._gpu_protocols import GpuSurfaceHandle as GpuSurfaceHandle
from ._gpu_protocols import GraphicsKernel as GraphicsKernel
from ._gpu_protocols import IOSurfaceMachPortExport as IOSurfaceMachPortExport
from ._gpu_protocols import KernelDispatchBatch as KernelDispatchBatch
from ._gpu_protocols import OpaqueFdTextureExport as OpaqueFdTextureExport
from ._gpu_protocols import RayTracingKernel as RayTracingKernel
from ._node_context_protocols import LinkInputDataReader as LinkInputDataReader
from ._node_context_protocols import LinkOutputDataWriter as LinkOutputDataWriter
from ._node_context_protocols import NodeLinkDataAccess as NodeLinkDataAccess
from ._node_context_protocols import (
    RuntimeContextFullAccess as RuntimeContextFullAccess,
)
from ._node_context_protocols import (
    RuntimeContextLimitedAccess as RuntimeContextLimitedAccess,
)
from ._node_context_protocols import (
    gpu_limited_access_of_the_typed_read_in_progress as gpu_limited_access_of_the_typed_read_in_progress,
)
from ._node_declaration import AudioWindowContract as AudioWindowContract
from ._node_declaration import node as node
from ._node_owned_window_protocols import NodeOwnedWindow as NodeOwnedWindow
from ._node_owned_window_protocols import (
    NodeOwnedWindowEvents as NodeOwnedWindowEvents,
)
from ._stream_graph_builder import NodeInputPortReference as NodeInputPortReference
from ._stream_graph_builder import (
    NodeOutputPortReference as NodeOutputPortReference,
)
from ._stream_graph_builder import NodeReference as NodeReference
from ._stream_graph_builder import StreamBuilder as StreamBuilder
from ._stream_graph_builder import compile_stream_to_graph as compile_stream_to_graph
from ._stream_graph_builder import stream as stream
from .audio_block import AudioBlock as AudioBlock
from .claimed_surface_pixel_access import (
    ClaimedSurfacePixelAccess as ClaimedSurfacePixelAccess,
)
from .claimed_surface_pixel_access import (
    PixelAccessToOneClaimedSurface as PixelAccessToOneClaimedSurface,
)
from .clock import MonotonicTimer as MonotonicTimer
from .clock import monotonic_now_ns as monotonic_now_ns
from .clock import start_monotonic_timer as start_monotonic_timer
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
from .node_output_texture_ring import NodeOutputTextureRing as NodeOutputTextureRing
from .video_frame import ColorInfo as ColorInfo
from .video_frame import ContentLight as ContentLight
from .video_frame import MasteringDisplay as MasteringDisplay
from .video_frame import VideoFrame as VideoFrame

__all__ = [
    "AccelerationStructureHandle",
    "AudioBlock",
    "AudioWindowContract",
    "CameraSource",
    "CameraSourceConfig",
    "ClaimedSurfacePixelAccess",
    "ColorInfo",
    "ComputeKernel",
    "ContentLight",
    "DisplayWindow",
    "DisplayWindowConfig",
    "EncodedAudioPacket",
    "EncodedVideoFrame",
    "GlslPixelEffect",
    "GlslPixelEffectDialType",
    "GpuContextFullAccess",
    "GpuContextLimitedAccess",
    "GpuSurfaceCheckOutLease",
    "GpuSurfaceDeviceTensorScope",
    "GpuSurfaceHandle",
    "GraphicsKernel",
    "H264Decoder",
    "H264DecoderConfig",
    "H264Encoder",
    "H264EncoderConfig",
    "H265Decoder",
    "H265DecoderConfig",
    "H265Encoder",
    "H265EncoderConfig",
    "IOSurfaceMachPortExport",
    "KernelDispatchBatch",
    "LinkInputDataReader",
    "LinkOutputDataWriter",
    "MasteringDisplay",
    "MicrophoneSource",
    "MicrophoneSourceConfig",
    "ModelInputTensor",
    "ModelInputTensorChannelOrder",
    "ModelInputTensorDtype",
    "ModelInputTensorFit",
    "ModelInputTensorGeometry",
    "ModelInputTensorKernel",
    "ModelInputTensorLayout",
    "MonotonicTimer",
    "Mp4Sink",
    "Mp4SinkConfig",
    "NodeInputPortReference",
    "NodeLinkDataAccess",
    "NodeOutputPortReference",
    "NodeOutputTextureRing",
    "NodeOwnedWindow",
    "NodeOwnedWindowEvents",
    "NodeReference",
    "OpaqueFdTextureExport",
    "OpusDecoder",
    "OpusDecoderConfig",
    "OpusEncoder",
    "OpusEncoderConfig",
    "PixelAccessToOneClaimedSurface",
    "RayTracingKernel",
    "RuntimeContextFullAccess",
    "RuntimeContextLimitedAccess",
    "SpeakerSink",
    "SpeakerSinkConfig",
    "StreamBuilder",
    "TestPatternSource",
    "TestPatternSourceConfig",
    "VideoFrame",
    "VirtualCameraSink",
    "VirtualCameraSinkConfig",
    "clock",
    "compile_stream_to_graph",
    "decode_msgpack_bytes_to_python_object",
    "encode_bag_to_msgpack_bytes",
    "gpu_limited_access_of_the_typed_read_in_progress",
    "log",
    "monotonic_now_ns",
    "node",
    "start_monotonic_timer",
    "stream",
]
