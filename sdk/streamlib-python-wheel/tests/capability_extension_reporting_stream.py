# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The one-processor stream `capability_extension_app.py`'s helper scenarios load."""

from capability_extension_processor import ReportsTheExtensionItsHelperLoaded
from tatolab.stream import StreamBuilder, stream


@stream
def one_processor_that_reports_its_helpers_extensions(stream_builder: StreamBuilder) -> None:
    """A `ReportsTheExtensionItsHelperLoaded`, the reason a helper process exists."""
    stream_builder.add(ReportsTheExtensionItsHelperLoaded)
