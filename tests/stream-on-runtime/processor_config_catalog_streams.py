# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The `@graph_generator` function `test_processor_config_catalog.py` runs from the suite project."""

from tatolab.stream import StreamBuilder, graph_generator

import processor_config_catalog_probes as probes


@graph_generator
def four_probes_each_configured_its_own_way(stream_builder: StreamBuilder) -> None:
    """Probes configured by a TypedDict, a dataclass and a model, beside one taking none."""
    stream_builder.add(probes.TypedDictConfiguredProbe, config={"width": 320})
    stream_builder.add(probes.DataclassConfiguredProbe, config={"width": 640, "label": "left"})
    stream_builder.add(probes.ModelConfiguredProbe, config={"width": 1280})
    stream_builder.add(probes.UnconfiguredProbe)
