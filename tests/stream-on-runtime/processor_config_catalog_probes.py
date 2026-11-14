# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Four processors — three kinds of config class, and one declaring none.

Their own module, not the test module, because each runs in a processor
interpreter that reaches its class by importing the module it was declared in.

Each reports, from inside its interpreter, the object its `__init__` was handed
— which is the half of the contract a served schema cannot show.
"""

import dataclasses
import json
from typing import Annotated, Optional, TypedDict

import pydantic

from tatolab.stream import Description, RuntimeContextFullAccess, log, node


def _report(processor_name: str, config: object) -> None:
    log.info(
        f"MARKER:CONSTRUCTED {json.dumps({'processor': processor_name, 'config_type': type(config).__name__})}"
    )


class TypedDictProbeConfig(TypedDict, total=False):
    width: Annotated[int, Description("How wide the probe pretends its frames are.")]


@dataclasses.dataclass
class DataclassProbeConfig:
    width: Annotated[int, Description("How wide the probe pretends its frames are.")] = 640
    label: Annotated[str, Description("What to call this probe.")] = "unlabelled"
    # A null default has to survive the msgpack hop the document takes into
    # Rust, which is the one value on this class that could be dropped there.
    fallback: Annotated[Optional[str], Description("Where the probe falls back to.")] = None


class ModelProbeConfig(pydantic.BaseModel):
    width: int = 1280


@node.meta(description="Configured by a TypedDict")
@node.process(execution="manual")
class TypedDictConfiguredProbe:
    def __init__(self, config: TypedDictProbeConfig) -> None:
        self.config = config

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        _report("TypedDictConfiguredProbe", self.config)


@node.meta(description="Configured by a dataclass")
@node.process(execution="manual")
class DataclassConfiguredProbe:
    def __init__(self, config: DataclassProbeConfig) -> None:
        self.config = config

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        _report("DataclassConfiguredProbe", self.config)


@node.process(execution="manual")
class ModelConfiguredProbe:
    """Configured by a model; this docstring is no description."""

    def __init__(self, config: ModelProbeConfig) -> None:
        self.config = config

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        _report("ModelConfiguredProbe", self.config)


@node.meta(description="Takes no configuration at all")
@node.process(execution="manual")
class UnconfiguredProbe:
    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        _report("UnconfiguredProbe", None)
