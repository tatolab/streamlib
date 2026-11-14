# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""What a stream or node module imports.

A graph generator is a `@graph_generator` function that adds and links nodes on a
`StreamBuilder`; `run_graph_generator` returns the graph it builds, and one
generator `add`ed in another is stored inside its graph as a fragment. A node
declares its execution with `@node.process` and its ports as `Input` and `Output` class
attributes of one family each, and receives a capability-typed context in every lifecycle hook. The contexts, link
endpoints, GPU classes and timer a node is handed while it runs are
`typing.Protocol`s the runtime implements; the clock and the timer's start are
functions that reach the runtime when called, and raise `RuntimeError` naming
themselves where no runtime is lent. A read through a port attribute returns
the family's type, and a read by name the raw payload. Everything else here is pure Python, so a stream imports, type-checks and
compiles with no runtime installed. The built-in nodes are generated from the
runtime's own declarations, each with a `TypedDict` for its config.
"""

from . import clock as clock
from . import log as log
from . import stream_facts as stream_facts
from ._exposed_name_cast import (
    ExposedNameCastsToNothingError as ExposedNameCastsToNothingError,
)
from ._exposed_name_cast import (
    cast_exposed_name_to_url_safe as cast_exposed_name_to_url_safe,
)
from ._description import Description as Description
from ._number_range import NumberRange as NumberRange
from ._project_asset_uuid import ProjectAssetMedia as ProjectAssetMedia
from ._project_asset_uuid import ProjectAssetUuid as ProjectAssetUuid
from ._built_in_nodes import ArrayToChannels as ArrayToChannels
from ._built_in_nodes import ArrayToChannelsConfig as ArrayToChannelsConfig
from ._built_in_nodes import ArrayToImage as ArrayToImage
from ._built_in_nodes import ArrayToImageConfig as ArrayToImageConfig
from ._built_in_nodes import ChannelsToArray as ChannelsToArray
from ._built_in_nodes import ChannelsToArrayConfig as ChannelsToArrayConfig
from ._built_in_nodes import ChannelsToRecords as ChannelsToRecords
from ._built_in_nodes import ChannelsToRecordsConfig as ChannelsToRecordsConfig
from ._built_in_nodes import ImageToArray as ImageToArray
from ._built_in_nodes import ImageToArrayConfig as ImageToArrayConfig
from ._built_in_nodes import RecordsToChannels as RecordsToChannels
from ._built_in_nodes import RecordsToChannelsConfig as RecordsToChannelsConfig
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
from ._gpu_protocols import OpaqueFdAllocationExport as OpaqueFdAllocationExport
from ._gpu_protocols import RayTracingKernel as RayTracingKernel
from ._node_context_protocols import (
    CastFromMessagePayloadBytes as CastFromMessagePayloadBytes,
)
from ._node_context_protocols import LinkInputDataReader as LinkInputDataReader
from ._node_context_protocols import LinkOutputDataWriter as LinkOutputDataWriter
from ._node_context_protocols import NodeLinkDataAccess as NodeLinkDataAccess
from ._node_context_protocols import ArrayFrameCpuAttribute as ArrayFrameCpuAttribute
from ._node_context_protocols import ChannelsBlock as ChannelsBlock
from ._node_context_protocols import PacketsUnit as PacketsUnit
from ._node_context_protocols import PortPayloadBorrow as PortPayloadBorrow
from ._node_context_protocols import PortPayloadLoan as PortPayloadLoan
from ._node_context_protocols import StoreScopedToThisNode as StoreScopedToThisNode
from ._node_context_protocols import UnreadMessage as UnreadMessage
from ._node_context_protocols import (
    RuntimeContextFullAccess as RuntimeContextFullAccess,
)
from ._node_context_protocols import (
    RuntimeContextLimitedAccess as RuntimeContextLimitedAccess,
)
from ._node_context_protocols import (
    gpu_limited_access_of_the_typed_read_in_progress as gpu_limited_access_of_the_typed_read_in_progress,
)
from ._message import Message as Message
from ._node_declaration import AudioWindowContract as AudioWindowContract
from ._node_declaration import node as node
from ._previous_value_node import PreviousValue as PreviousValue
from ._function_node_declaration import FunctionNode as FunctionNode
from ._function_node_declaration import State as State
from ._function_node_declaration import Stateful as Stateful
from ._function_node_declaration import Time as Time
from ._rust_function_node_declaration import RustFunctionNode as RustFunctionNode
from ._shader_node_declaration import ShaderNode as ShaderNode
from ._port_family_declaration import Array as Array
from ._port_family_declaration import Channels as Channels
from ._port_family_declaration import Image as Image
from ._port_family_declaration import Input as Input
from ._port_family_declaration import Output as Output
from ._port_family_declaration import Packets as Packets
from ._port_family_declaration import PortFamily as PortFamily
from ._port_family_declaration import PortNamesFromConfig as PortNamesFromConfig
from ._port_family_declaration import Records as Records
from ._port_family_declaration import from_config as from_config
from ._graph_interface_nodes import GraphInput as GraphInput
from ._graph_interface_nodes import GraphInputConfig as GraphInputConfig
from ._graph_interface_nodes import GraphOutput as GraphOutput
from ._graph_interface_nodes import GraphOutputConfig as GraphOutputConfig
from ._graph_variable_nodes import GetVariable as GetVariable
from ._graph_variable_nodes import GetVariableConfig as GetVariableConfig
from ._graph_variable_nodes import SetVariable as SetVariable
from ._graph_variable_nodes import SetVariableConfig as SetVariableConfig
from ._stream_graph_builder import GraphFragmentReference as GraphFragmentReference
from ._stream_graph_builder import NodeInputPortReference as NodeInputPortReference
from ._stream_graph_builder import (
    NodeOutputPortReference as NodeOutputPortReference,
)
from ._stream_graph_builder import NodeReference as NodeReference
from ._stream_graph_builder import (
    NodeReferenceToNodeType as NodeReferenceToNodeType,
    SoleReferrerNodeReferenceToNodeType as SoleReferrerNodeReferenceToNodeType,
)
from ._stream_graph_builder import StreamBuilder as StreamBuilder
from ._stream_graph_builder import graph_generator as graph_generator
from ._stream_graph_builder import run_graph_generator as run_graph_generator
from .claimed_surface_pixel_access import (
    ClaimedSurfacePixelAccess as ClaimedSurfacePixelAccess,
)
from .claimed_surface_pixel_access import (
    PixelAccessToOneClaimedSurface as PixelAccessToOneClaimedSurface,
)
from .clock import DEFAULT_SCALED_STREAM_CLOCK_STEP_NS as DEFAULT_SCALED_STREAM_CLOCK_STEP_NS
from .clock import MonotonicTimer as MonotonicTimer
from .clock import RealTimeStreamClock as RealTimeStreamClock
from .clock import ScaledStreamClock as ScaledStreamClock
from .clock import SteppedStreamClock as SteppedStreamClock
from .clock import StreamClock as StreamClock
from .clock import StreamClockReading as StreamClockReading
from .clock import monotonic_now_ns as monotonic_now_ns
from .clock import start_monotonic_timer as start_monotonic_timer
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
from .color_info import ColorInfo as ColorInfo
from .array_frame import ArrayFrame as ArrayFrame
from .array_frame import ArrayFrameGpuAttribute as ArrayFrameGpuAttribute
from .image_frame import ImageFrame as ImageFrame
from .image_frame import ImageFrameToWrite as ImageFrameToWrite

__all__ = [
    "AccelerationStructureHandle",
    "Array",
    "ArrayFrame",
    "ArrayFrameCpuAttribute",
    "ArrayFrameGpuAttribute",
    "ArrayToChannels",
    "ArrayToChannelsConfig",
    "ArrayToImage",
    "ArrayToImageConfig",
    "AudioWindowContract",
    "CastFromMessagePayloadBytes",
    "Channels",
    "ChannelsBlock",
    "ChannelsToArray",
    "ChannelsToArrayConfig",
    "ChannelsToRecords",
    "ChannelsToRecordsConfig",
    "ClaimedSurfacePixelAccess",
    "ColorInfo",
    "ComputeKernel",
    "DEFAULT_SCALED_STREAM_CLOCK_STEP_NS",
    "GetVariable",
    "GetVariableConfig",
    "GraphFragmentReference",
    "GraphInput",
    "GraphInputConfig",
    "GraphOutput",
    "GraphOutputConfig",
    "GpuContextFullAccess",
    "GpuContextLimitedAccess",
    "GpuSurfaceCheckOutLease",
    "GpuSurfaceDeviceTensorScope",
    "GpuSurfaceHandle",
    "GraphicsKernel",
    "IOSurfaceMachPortExport",
    "Image",
    "ImageFrame",
    "ImageFrameToWrite",
    "ImageToArray",
    "ImageToArrayConfig",
    "Input",
    "KernelDispatchBatch",
    "LinkInputDataReader",
    "LinkOutputDataWriter",
    "Message",
    "ModelInputTensor",
    "ModelInputTensorChannelOrder",
    "ModelInputTensorDtype",
    "ModelInputTensorFit",
    "ModelInputTensorGeometry",
    "ModelInputTensorKernel",
    "ModelInputTensorLayout",
    "MonotonicTimer",
    "NodeInputPortReference",
    "NodeLinkDataAccess",
    "NodeOutputPortReference",
    "NodeOutputTextureRing",
    "NodeReference",
    "NodeReferenceToNodeType",
    "NumberRange",
    "ProjectAssetMedia",
    "ProjectAssetUuid",
    "ExposedNameCastsToNothingError",
    "FunctionNode",
    "RustFunctionNode",
    "ShaderNode",
    "cast_exposed_name_to_url_safe",
    "SoleReferrerNodeReferenceToNodeType",
    "State",
    "Stateful",
    "OpaqueFdAllocationExport",
    "Output",
    "Packets",
    "PacketsUnit",
    "PixelAccessToOneClaimedSurface",
    "PortFamily",
    "PreviousValue",
    "PortNamesFromConfig",
    "PortPayloadBorrow",
    "PortPayloadLoan",
    "RayTracingKernel",
    "RealTimeStreamClock",
    "Records",
    "RecordsToChannels",
    "RecordsToChannelsConfig",
    "RuntimeContextFullAccess",
    "RuntimeContextLimitedAccess",
    "ScaledStreamClock",
    "SetVariable",
    "SetVariableConfig",
    "SteppedStreamClock",
    "StoreScopedToThisNode",
    "StreamBuilder",
    "StreamClock",
    "StreamClockReading",
    "Time",
    "UnreadMessage",
    "clock",
    "from_config",
    "graph_generator",
    "gpu_limited_access_of_the_typed_read_in_progress",
    "log",
    "monotonic_now_ns",
    "node",
    "run_graph_generator",
    "start_monotonic_timer",
    "stream_facts",
]
