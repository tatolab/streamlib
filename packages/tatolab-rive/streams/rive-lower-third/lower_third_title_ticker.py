# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""`LowerThirdTitleTicker`: a numbered title for a `RiveSource`'s `data` input once a second."""

from typing import Annotated

from tatolab.rive import ViewModelValues
from tatolab.stream import Description, OutputPin, RuntimeContextLimitedAccess, node


@node(execution="continuous", interval_ms=1000)
class LowerThirdTitleTicker:
    """Sends a numbered title to a `RiveSource`'s `data` input once a second."""

    property_path: Annotated[
        str, Description("The view-model string property each title is assigned to.")
    ] = "title"
    entrance_trigger_paths: Annotated[
        list[str],
        Description("View-model triggers fired, in order, with the first title, to bring the card in."),
    ] = []  # noqa: RUF012
    data: OutputPin[
        Annotated[ViewModelValues, Description("View-model values for a `RiveSource`")]
    ]

    def __init__(self) -> None:
        self._titles_sent = 0

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        del ctx
        self._titles_sent += 1
        view_model_values: dict[str, "str | bool"] = {
            self.property_path: f"Live title {self._titles_sent}"
        }
        if self._titles_sent == 1:
            for entrance_trigger_path in self.entrance_trigger_paths:
                view_model_values[entrance_trigger_path] = True
        self.data.write(ViewModelValues(view_model_values))
