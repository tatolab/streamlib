# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A web page as a stream source: `from tatolab.browser import BrowserSource`.

Chromium, through CEF, renders the page offscreen in the node's own helper
process. Its frames, audio and published objects go out through the module
host table from CEF's threads; nothing here links the engine.
"""

from .browser_source import BrowserSource as BrowserSource
from .browser_source_output_probe import BrowserSourceOutputProbe as BrowserSourceOutputProbe
from .page_message import PageMessage as PageMessage

__all__ = ["BrowserSource", "BrowserSourceOutputProbe", "PageMessage"]
