# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A web page as a stream source: `from tatolab.browser import BrowserSource`.

Chromium, through CEF, renders the page offscreen in the node's own helper
process. Its frames, audio and published objects go out through the module
host table from CEF's threads; nothing here links the engine.
"""

from .browser_source import BrowserSource as BrowserSource
from .browser_source import BrowserSourceConfig as BrowserSourceConfig
from .browser_source_output_probe import BrowserSourceOutputProbe as BrowserSourceOutputProbe
from .browser_source_output_probe import (
    BrowserSourceOutputProbeConfig as BrowserSourceOutputProbeConfig,
)

__all__ = [
    "BrowserSource",
    "BrowserSourceConfig",
    "BrowserSourceOutputProbe",
    "BrowserSourceOutputProbeConfig",
]
