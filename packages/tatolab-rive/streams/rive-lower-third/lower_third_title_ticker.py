# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""`LowerThirdTitleTicker`: a numbered title for a `RiveSource`'s `data` input once a second."""

from typing import Annotated, Callable

from tatolab.rive import ViewModelValues
from tatolab.stream import Description, Input, OutputPin, RuntimeContextLimitedAccess, node


@node(execution="continuous", interval_ms=1000)
def LowerThirdTitleTicker(
    *,
    property_path: Input[
        Annotated[str, Description("The view-model string property each title is assigned to.")]
    ] = Input("title"),
    entrance_trigger_paths: Input[
        Annotated[
            list[str],
            Description(
                "View-model triggers fired, in order, with the first title, to bring the card in."
            ),
        ]
    ] = Input([]),
    data: OutputPin[
        Annotated[ViewModelValues, Description("View-model values for a `RiveSource`")]
    ],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Sends a numbered title to a `RiveSource`'s `data` input once a second."""
    titles_sent = 0

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        nonlocal titles_sent
        del ctx
        titles_sent += 1
        view_model_values: dict[str, "str | bool"] = {
            property_path.get(): f"Live title {titles_sent}"
        }
        if titles_sent == 1:
            for entrance_trigger_path in entrance_trigger_paths.get():
                view_model_values[entrance_trigger_path] = True
        data.write(ViewModelValues(view_model_values))

    return each_frame
