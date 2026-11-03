# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`LowerThirdTitleTicker`: a numbered title for a `RiveSource`'s `data` input once a second."""

import dataclasses
from typing import Annotated

from tatolab.stream import RuntimeContextLimitedAccess, msgpack, node


@dataclasses.dataclass
class LowerThirdTitleTickerConfig:
    """What a `LowerThirdTitleTicker` is configured with."""

    property_path: Annotated[str, "The view-model string property each title is assigned to."]
    entrance_trigger_paths: Annotated[
        "list[str]", "View-model triggers fired, in order, with the first title, to bring the card in."
    ] = dataclasses.field(default_factory=list)


@node(execution="continuous", interval_ms=1000)
class LowerThirdTitleTicker:
    """Sends a numbered title to a `RiveSource`'s `data` input once a second."""

    def __init__(self, config: LowerThirdTitleTickerConfig) -> None:
        self._property_path = config.property_path
        self._entrance_trigger_paths = config.entrance_trigger_paths
        self._titles_sent = 0

    @node.output()
    def data(self) -> None:
        """View-model data messages for a `RiveSource`."""

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        self._titles_sent += 1
        view_model_values: dict[str, object] = {
            self._property_path: f"Live title {self._titles_sent}"
        }
        if self._titles_sent == 1:
            for entrance_trigger_path in self._entrance_trigger_paths:
                view_model_values[entrance_trigger_path] = True
        ctx.outputs.write("data", msgpack.pack(view_model_values))
