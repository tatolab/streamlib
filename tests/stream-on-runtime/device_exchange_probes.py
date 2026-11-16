# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Probes that exercise the device half of the pixel exchange from where it
really runs.

Each probe runs in its own helper process, reaches the frame's pixels as device
memory there — CUDA on Linux, Metal on macOS — and reports what it observed as
one `MARKER:PROBE_RESULT` JSON line, the same child→parent log forwarding every
node's records ride.

On Linux the device path crosses the process boundary twice per surface: the
parent allocates and publishes the export staging, the child imports it, and
every refill is a round trip whose answer is the timeline value to wait for. On
macOS the device view is a no-copy Metal buffer over the frame's own IOSurface.
What is worth breaking a build over on both is that the tensor really is
device-resident, that its pixels are the frame's pixels, and that an edit
published from the child is visible to a second, independent resolve.

The probes live in a module per kind; this one names each for the streams and
tests that add them.
"""

from device_exchange_frame_tensor_probes import (
    CopyRequestRefusedAtBothDoorsProbe,
    DeviceEditProbe,
    DeviceTensorStridesFollowTheRowPitchProbe,
    GraphFrameToTorchProbe,
    HostSideProbe,
    LaggedConsumerHoldsItsFrameProbe,
    TensorOutlivesHandleProbe,
    WithBlockEditProbe,
)
from device_exchange_probe_shared_surfaces_and_reporting import (
    FILL_CONSTANT_RGBA,
    NATURAL_DLPACK_DEVICE,
    NATURAL_TORCH_DEVICE_TYPE,
    SURFACE_HEIGHT,
    SURFACE_WIDTH,
)
from device_exchange_scope_and_mlx_probes import (
    DeviceTensorScopeDiscardsOnRaiseProbe,
    DeviceTensorScopeDoublesAKernelOutputProbe,
    DeviceTensorScopeTakesEveryAcquiredTextureProbe,
    MlxArrayOutlivesTextureHandleProbe,
    MlxDeviceWriteThenEngineGpuReadProbe,
    MlxReadsTheFrameProbe,
    MlxWholeArrayAssignmentMissesTheFrameProbe,
    MlxWriteWithAViewAliveMissesTheFrameProbe,
    MlxWritesTheFrameThroughTheWriteDoorProbe,
    PixelBufferScopeDiscardsOnRaiseProbe,
    TorchAndMlxScopesAlternateInOneHelperProbe,
    TorchDeviceWriteThenEngineGpuReadProbe,
    TorchTensorOutlivesTextureHandleProbe,
)
from device_exchange_texture_and_export_probes import (
    DmaBufExportProbe,
    IOSurfaceExportProbe,
    OpaqueFdExportHandoffProbe,
    OpaqueFdPixelBufferExportHandoffProbe,
    PooledTextureExportProbe,
    PrivilegedCapabilityProbe,
    RawHandleOffItsPlatformRefusesProbe,
    TextureHandleRoundTripProbe,
)

__all__ = [
    "CopyRequestRefusedAtBothDoorsProbe",
    "DeviceEditProbe",
    "DeviceTensorScopeDiscardsOnRaiseProbe",
    "DeviceTensorScopeDoublesAKernelOutputProbe",
    "DeviceTensorScopeTakesEveryAcquiredTextureProbe",
    "DeviceTensorStridesFollowTheRowPitchProbe",
    "DmaBufExportProbe",
    "FILL_CONSTANT_RGBA",
    "GraphFrameToTorchProbe",
    "HostSideProbe",
    "IOSurfaceExportProbe",
    "LaggedConsumerHoldsItsFrameProbe",
    "MlxArrayOutlivesTextureHandleProbe",
    "MlxDeviceWriteThenEngineGpuReadProbe",
    "MlxReadsTheFrameProbe",
    "MlxWholeArrayAssignmentMissesTheFrameProbe",
    "MlxWriteWithAViewAliveMissesTheFrameProbe",
    "MlxWritesTheFrameThroughTheWriteDoorProbe",
    "NATURAL_DLPACK_DEVICE",
    "NATURAL_TORCH_DEVICE_TYPE",
    "OpaqueFdExportHandoffProbe",
    "OpaqueFdPixelBufferExportHandoffProbe",
    "PixelBufferScopeDiscardsOnRaiseProbe",
    "PooledTextureExportProbe",
    "PrivilegedCapabilityProbe",
    "RawHandleOffItsPlatformRefusesProbe",
    "SURFACE_HEIGHT",
    "SURFACE_WIDTH",
    "TensorOutlivesHandleProbe",
    "TextureHandleRoundTripProbe",
    "TorchAndMlxScopesAlternateInOneHelperProbe",
    "TorchDeviceWriteThenEngineGpuReadProbe",
    "TorchTensorOutlivesTextureHandleProbe",
    "WithBlockEditProbe",
]
