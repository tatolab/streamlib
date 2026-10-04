# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The one-processor stream `capability_extension_app.py`'s helper scenarios load."""

from capability_extension_processor import ReportsTheExtensionItsHelperLoaded
from streamlib import Stream, stream


@stream
def one_processor_that_reports_its_helpers_extensions(stream: Stream) -> None:
    """A `ReportsTheExtensionItsHelperLoaded`, the reason a helper process exists."""
    stream.add(ReportsTheExtensionItsHelperLoaded)
