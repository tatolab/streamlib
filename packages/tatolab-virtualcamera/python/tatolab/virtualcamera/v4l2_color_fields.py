# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A frame's colour as the YUYV picture carries it, and the `v4l2_pix_format` fields that announce it."""

from __future__ import annotations

import dataclasses

from tatolab.stream import ColorInfo

V4L2_COLORSPACE_DEFAULT = 0
V4L2_COLORSPACE_SMPTE170M = 1
V4L2_COLORSPACE_SMPTE240M = 2
V4L2_COLORSPACE_REC709 = 3
V4L2_COLORSPACE_470_SYSTEM_M = 5
V4L2_COLORSPACE_470_SYSTEM_BG = 6
V4L2_COLORSPACE_SRGB = 8
V4L2_COLORSPACE_BT2020 = 10
V4L2_COLORSPACE_DCI_P3 = 12

V4L2_XFER_FUNC_DEFAULT = 0
V4L2_XFER_FUNC_709 = 1
V4L2_XFER_FUNC_SRGB = 2
V4L2_XFER_FUNC_NONE = 5
V4L2_XFER_FUNC_SMPTE2084 = 7

V4L2_YCBCR_ENC_DEFAULT = 0
V4L2_YCBCR_ENC_601 = 1
V4L2_YCBCR_ENC_709 = 2
V4L2_YCBCR_ENC_BT2020 = 6
V4L2_YCBCR_ENC_BT2020_CONST_LUM = 7
V4L2_YCBCR_ENC_SMPTE240M = 8

V4L2_QUANTIZATION_FULL_RANGE = 1
V4L2_QUANTIZATION_LIM_RANGE = 2

_V4L2_COLORSPACE_BY_PRIMARIES = {
    "smpte170m": V4L2_COLORSPACE_SMPTE170M,
    "smpte240m": V4L2_COLORSPACE_SMPTE240M,
    "bt470_m": V4L2_COLORSPACE_470_SYSTEM_M,
    "bt470_bg": V4L2_COLORSPACE_470_SYSTEM_BG,
    "bt2020": V4L2_COLORSPACE_BT2020,
    "smpte431": V4L2_COLORSPACE_DCI_P3,
}

# The engine converts under one of five transfer curves; every other H.273
# name is carried as BT.709.
_V4L2_XFER_FUNC_BY_ENGINE_TRANSFER_NAME = {
    "srgb": V4L2_XFER_FUNC_SRGB,
    "linear": V4L2_XFER_FUNC_NONE,
    "smpte2084": V4L2_XFER_FUNC_SMPTE2084,
    "arib_std_b67": V4L2_XFER_FUNC_DEFAULT,
}

_V4L2_YCBCR_ENC_BY_MATRIX = {
    "smpte170m": V4L2_YCBCR_ENC_601,
    "bt470_bg": V4L2_YCBCR_ENC_601,
    "fcc": V4L2_YCBCR_ENC_601,
    "bt709": V4L2_YCBCR_ENC_709,
    "bt2020_ncl": V4L2_YCBCR_ENC_BT2020,
    "bt2020_cl": V4L2_YCBCR_ENC_BT2020_CONST_LUM,
    "smpte240m": V4L2_YCBCR_ENC_SMPTE240M,
}


@dataclasses.dataclass(frozen=True)
class V4l2PixFormatColorFields:
    """The four colour fields of a `v4l2_pix_format`."""

    colorspace: int
    xfer_func: int
    ycbcr_enc: int
    quantization: int


def yuyv_picture_color_of(frame_color_info: "ColorInfo | None") -> ColorInfo:
    """The frame's colour with every absent axis given the engine's YCbCr default: BT.709, SMPTE 170M matrix, limited range.

    An RGB frame's identity matrix names no YCbCr encoding, so it takes the default matrix too.
    """
    color_info = frame_color_info or ColorInfo()
    frame_ycbcr_matrix = None if color_info.matrix == "identity" else color_info.matrix
    return ColorInfo(
        primaries=color_info.primaries or "bt709",
        transfer=color_info.transfer or "bt709",
        matrix=frame_ycbcr_matrix or "smpte170m",
        range=color_info.range or "limited",
    )


def v4l2_color_fields_of(yuyv_picture_color: ColorInfo) -> V4l2PixFormatColorFields:
    """The `v4l2_pix_format` colour fields announcing a picture of this resolved colour."""
    transfer = yuyv_picture_color.transfer
    if yuyv_picture_color.primaries == "bt709":
        colorspace = (
            V4L2_COLORSPACE_SRGB if transfer == "srgb" else V4L2_COLORSPACE_REC709
        )
    else:
        colorspace = _V4L2_COLORSPACE_BY_PRIMARIES.get(
            yuyv_picture_color.primaries or "", V4L2_COLORSPACE_DEFAULT
        )
    return V4l2PixFormatColorFields(
        colorspace=colorspace,
        xfer_func=_V4L2_XFER_FUNC_BY_ENGINE_TRANSFER_NAME.get(
            transfer or "", V4L2_XFER_FUNC_709
        ),
        ycbcr_enc=_V4L2_YCBCR_ENC_BY_MATRIX.get(
            yuyv_picture_color.matrix or "", V4L2_YCBCR_ENC_DEFAULT
        ),
        quantization=(
            V4L2_QUANTIZATION_FULL_RANGE
            if yuyv_picture_color.range == "full"
            else V4L2_QUANTIZATION_LIM_RANGE
        ),
    )
