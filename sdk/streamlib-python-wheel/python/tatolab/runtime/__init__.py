# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""What the runtime lends a processor interpreter.

`_engine` is the native extension behind every runtime-backed name
`tatolab.stream` declares, and `_processor_interpreter_bootstrap` is the
script the runtime starts each processor interpreter with.
"""

import os

from ._bundled_vulkan_driver import point_the_vulkan_loader_at_the_bundled_driver

# Before `_engine` loads, so no Vulkan instance can predate the driver search.
point_the_vulkan_loader_at_the_bundled_driver(os.environ)

__all__: "list[str]" = []
