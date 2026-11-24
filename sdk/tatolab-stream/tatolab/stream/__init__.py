# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""What a stream or node module imports.

A graph generator is a `@graph_generator` function that adds and links nodes on a
`StreamBuilder`; `run_graph_generator` returns the graph it builds, and one
generator `add`ed in another is stored inside its graph as a fragment. A node is declared
with bare `@node`, on a function whose parameters and return are its typed pins or on a class
whose fields are, and receives a capability-typed context in every lifecycle hook. The
contexts, GPU classes and timer a node is handed while it runs are `typing.Protocol`s the
runtime implements; the clock and the timer's start are functions that reach the runtime when
called, and raise `RuntimeError` naming themselves where no runtime is lent. Everything else
here is pure Python, so a stream imports, type-checks and compiles with no runtime installed.
The built-in nodes are generated from the runtime's own declarations, each with its fields and
pins. A name this module retired is refused on import with what replaces it.
"""

from typing import TYPE_CHECKING

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
from ._formerly_named import FormerlyNamed as FormerlyNamed
from ._pin_type_narrowing import AssetMedia as AssetMedia
from ._pin_type_narrowing import EncodedCodecs as EncodedCodecs
from ._pin_type_narrowing import FlowsAsValue as FlowsAsValue
from ._pin_type_narrowing import TexturePixelFormats as TexturePixelFormats
from ._connection_type_declaration import Connection as Connection
from ._connection_type_declaration import SoleReferrer as SoleReferrer
from ._connection_type_declaration import connection_type as connection_type
from ._named_pin_type_declaration import NamedPinTypeValue as NamedPinTypeValue
from ._named_pin_type_declaration import named_pin_type as named_pin_type
from ._number_range import NumberRange as NumberRange
from ._pin_annotations import NumberStep as NumberStep
from ._pin_annotations import NumberUnit as NumberUnit
from ._pin_annotations import PinControl as PinControl
from ._pin_annotations import PinMeaning as PinMeaning
from ._pin_type_choice import PinTypeChoice as PinTypeChoice
from ._pin_type_choice import PinTypeFromSetting as PinTypeFromSetting
from ._pin_declaration import NodeDeclarationRefusal as NodeDeclarationRefusal
from ._class_node_pins import InputPin as InputPin
from ._class_node_pins import OutputPin as OutputPin
from ._class_node_pins import PinGroup as PinGroup
from ._input_declaration_annotations import InputValueLifetime as InputValueLifetime
from ._input_declaration_annotations import SettingOnly as SettingOnly
from ._input_declaration_annotations import StreamDelivery as StreamDelivery
from ._input_declaration_annotations import WireOnly as WireOnly
from ._pin_types import Asset as Asset
from ._pin_types import Audio as Audio
from ._pin_types import Color as Color
from ._pin_types import EncodedAudio as EncodedAudio
from ._pin_types import EncodedVideo as EncodedVideo
from ._pin_types import Float32 as Float32
from ._pin_types import Float64 as Float64
from ._pin_types import Int8 as Int8
from ._pin_types import Int16 as Int16
from ._pin_types import Int32 as Int32
from ._pin_types import Int64 as Int64
from ._pin_types import UInt8 as UInt8
from ._pin_types import UInt16 as UInt16
from ._pin_types import UInt32 as UInt32
from ._pin_types import UInt64 as UInt64
from ._pin_types import Rectangle as Rectangle
from ._pin_types import Tensor as Tensor
from ._pin_types import Texture as Texture
from ._pin_types import Transform as Transform
from ._pin_types import Trigger as Trigger
from ._pin_types import Vector2 as Vector2
from ._pin_types import Vector3 as Vector3
from ._pin_types import Vector4 as Vector4
from ._project_asset_uuid import ProjectAssetMedia as ProjectAssetMedia
from ._project_asset_uuid import ProjectAssetUuid as ProjectAssetUuid
from ._built_in_nodes import AudioToTensor as AudioToTensor
from ._built_in_nodes import BooleanToFloat as BooleanToFloat
from ._built_in_nodes import BooleanToInteger as BooleanToInteger
from ._built_in_nodes import BreakBundle as BreakBundle
from ._built_in_nodes import FloatToBoolean as FloatToBoolean
from ._built_in_nodes import FloatToInteger as FloatToInteger
from ._built_in_nodes import IntegerToBoolean as IntegerToBoolean
from ._built_in_nodes import IntegerToFloat as IntegerToFloat
from ._built_in_nodes import MakeBundle as MakeBundle
from ._built_in_nodes import TensorToAudio as TensorToAudio
from ._built_in_nodes import TensorToTexture as TensorToTexture
from ._built_in_nodes import TextureFormatConvert as TextureFormatConvert
from ._built_in_nodes import TextureToTensor as TextureToTensor
from ._gpu_protocols import AccelerationStructureHandle as AccelerationStructureHandle
from ._gpu_protocols import ComputeKernel as ComputeKernel
from ._gpu_protocols import GpuContextFullAccess as GpuContextFullAccess
from ._gpu_protocols import GpuContextLimitedAccess as GpuContextLimitedAccess
from ._gpu_surface_protocols import GpuSurfaceCheckOutLease as GpuSurfaceCheckOutLease
from ._gpu_surface_protocols import (
    GpuSurfaceDeviceTensorScope as GpuSurfaceDeviceTensorScope,
)
from ._gpu_surface_protocols import GpuSurfaceHandle as GpuSurfaceHandle
from ._gpu_protocols import GraphicsKernel as GraphicsKernel
from ._gpu_surface_protocols import IOSurfaceMachPortExport as IOSurfaceMachPortExport
from ._gpu_protocols import KernelDispatchBatch as KernelDispatchBatch
from ._gpu_surface_protocols import OpaqueFdAllocationExport as OpaqueFdAllocationExport
from ._gpu_protocols import RayTracingKernel as RayTracingKernel
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
from ._retired_declarations import RETIRED_IMPORTS_BY_NAME, RetiredImportRefusal
from ._function_node_declaration import FunctionNode as FunctionNode
from ._function_node_declaration import State as State
from ._function_node_declaration import Stateful as Stateful
from ._function_node_declaration import Time as Time
from ._rust_function_node_declaration import RustFunctionNode as RustFunctionNode
from ._shader_node_declaration import ShaderNode as ShaderNode
from ._graph_interface_nodes import GraphInput as GraphInput
from ._graph_interface_nodes import GraphOutput as GraphOutput
from ._graph_variable_nodes import GetVariable as GetVariable
from ._graph_variable_nodes import SetVariable as SetVariable
from ._stream_graph_builder import GraphFragmentReference as GraphFragmentReference
from ._stream_graph_builder import NodeInputPortReference as NodeInputPortReference
from ._stream_graph_builder import (
    NodeOutputPortReference as NodeOutputPortReference,
)
from ._stream_graph_builder import NodeReference as NodeReference
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
    "ArrayFrame",
    "ArrayFrameCpuAttribute",
    "ArrayFrameGpuAttribute",
    "AudioToTensor",
    "AudioWindowContract",
    "BooleanToFloat",
    "BooleanToInteger",
    "BreakBundle",
    "ChannelsBlock",
    "ClaimedSurfacePixelAccess",
    "ColorInfo",
    "ComputeKernel",
    "Connection",
    "DEFAULT_SCALED_STREAM_CLOCK_STEP_NS",
    "FloatToBoolean",
    "FloatToInteger",
    "GetVariable",
    "GraphFragmentReference",
    "GraphInput",
    "GraphOutput",
    "GpuContextFullAccess",
    "GpuContextLimitedAccess",
    "GpuSurfaceCheckOutLease",
    "GpuSurfaceDeviceTensorScope",
    "GpuSurfaceHandle",
    "GraphicsKernel",
    "IOSurfaceMachPortExport",
    "ImageFrame",
    "ImageFrameToWrite",
    "IntegerToBoolean",
    "IntegerToFloat",
    "KernelDispatchBatch",
    "LinkInputDataReader",
    "LinkOutputDataWriter",
    "MakeBundle",
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
    "FormerlyNamed",
    "FlowsAsValue",
    "AssetMedia",
    "EncodedCodecs",
    "TexturePixelFormats",
    "NamedPinTypeValue",
    "NumberRange",
    "NumberStep",
    "NumberUnit",
    "PinControl",
    "PinMeaning",
    "PinTypeChoice",
    "PinTypeFromSetting",
    "NodeDeclarationRefusal",
    "Float32",
    "Float64",
    "Int8",
    "Int16",
    "Int32",
    "Int64",
    "UInt8",
    "UInt16",
    "UInt32",
    "UInt64",
    "Asset",
    "Audio",
    "EncodedAudio",
    "EncodedVideo",
    "InputPin",
    "InputValueLifetime",
    "OutputPin",
    "PinGroup",
    "SettingOnly",
    "StreamDelivery",
    "Tensor",
    "Texture",
    "Trigger",
    "WireOnly",
    "Color",
    "Vector2",
    "Vector3",
    "Vector4",
    "Rectangle",
    "Transform",
    "ProjectAssetMedia",
    "ProjectAssetUuid",
    "ExposedNameCastsToNothingError",
    "FunctionNode",
    "RustFunctionNode",
    "ShaderNode",
    "cast_exposed_name_to_url_safe",
    "SoleReferrer",
    "State",
    "Stateful",
    "OpaqueFdAllocationExport",
    "PacketsUnit",
    "PixelAccessToOneClaimedSurface",
    "PreviousValue",
    "PortPayloadBorrow",
    "PortPayloadLoan",
    "RayTracingKernel",
    "RealTimeStreamClock",
    "RuntimeContextFullAccess",
    "RuntimeContextLimitedAccess",
    "ScaledStreamClock",
    "SetVariable",
    "SteppedStreamClock",
    "StoreScopedToThisNode",
    "StreamBuilder",
    "StreamClock",
    "StreamClockReading",
    "TensorToAudio",
    "TensorToTexture",
    "TextureFormatConvert",
    "TextureToTensor",
    "Time",
    "UnreadMessage",
    "clock",
    "connection_type",
    "graph_generator",
    "gpu_limited_access_of_the_typed_read_in_progress",
    "log",
    "monotonic_now_ns",
    "named_pin_type",
    "node",
    "run_graph_generator",
    "start_monotonic_timer",
    "stream_facts",
]

if not TYPE_CHECKING:
    # Hidden from the type checker, which would otherwise read every missing name as present.

    def __getattr__(name: str) -> object:
        """Refuse a name `tatolab.stream` retired with what replaces it, and any other as missing."""
        retired = RETIRED_IMPORTS_BY_NAME.get(name)
        if retired is not None:
            raise RetiredImportRefusal(retired)
        raise AttributeError(f"module 'tatolab.stream' has no attribute {name!r}", name=name, obj=None)
