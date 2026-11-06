# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A stand-in for the `tatolab.h264` the scaffold imports, which a project's
`uv sync` installs and this package's suite does not.

It declares the module encoder's ports and nothing else; the module's own suite
holds the real declaration. Written beside a project, it joins the installed
`tatolab` namespace package the compile imports from.
"""

from pathlib import Path

H264_MODULE_PROCESSORS_STAND_IN_SOURCE = (
    "from tatolab.stream import Image, Input, Output, Packets, node\n"
    "\n"
    "\n"
    '@node(execution="manual")\n'
    "class H264Encoder:\n"
    '    video = Input(Image, delivery="ordered")\n'
    '    encoded_video = Output(Packets, codec="h264")\n'
)

H264_MODULE_PACKAGE_STAND_IN_SOURCE = "from .processors import H264Encoder as H264Encoder\n"


def write_the_h264_module_stand_in_into(project_directory: Path) -> None:
    """Write the `tatolab/h264/` package, laid out as the module's, under `project_directory`."""
    stand_in_package = project_directory / "tatolab" / "h264"
    stand_in_package.mkdir(parents=True)
    (stand_in_package / "processors.py").write_text(H264_MODULE_PROCESSORS_STAND_IN_SOURCE)
    (stand_in_package / "__init__.py").write_text(H264_MODULE_PACKAGE_STAND_IN_SOURCE)
