# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""What a running node tells an agent about a processor's config, end to end.

The declaration suite proves the document is derived, and the engine's Rust
tests prove it reaches the shape `/api/registry` serializes. This is the whole
path on a real node with each processor in its own processor interpreter —
which needs a running graph, and a running graph initializes a GPU context, so
it runs on the rig like every other live proof in this suite.
"""

from collections.abc import Callable
from dataclasses import dataclass
from typing import Any

import pytest

from conftest import TatolabdUnderTest
from processor_config_catalog_streams import four_probes_each_configured_its_own_way

pytestmark = pytest.mark.requires_gpu

GRAPH_READY_TIMEOUT_SECONDS = 90.0

PROBE_TYPE_PREFIX = "processor_config_catalog_probes:"


@dataclass(frozen=True)
class CatalogRun:
    """What one run of the four probes served and reported."""

    served_catalog: "dict[str, Any]"
    constructed_reports: "list[Any]"


@pytest.fixture(scope="module")
def catalog_run_of_this_module() -> "dict[str, CatalogRun]":
    """Holds the one run every test in this module reads."""
    return {}


@pytest.fixture
def catalog_run(
    catalog_run_of_this_module: "dict[str, CatalogRun]",
    start_tatolabd_running_stream: "Callable[..., TatolabdUnderTest]",
) -> CatalogRun:
    """One node, one run: four processor interpreter spawns are the cost, so they are paid once.

    The first test to ask runs the stream to a clean exit through
    `start_tatolabd_running_stream`, which reaps it however that test ends;
    later tests read what it served.
    """
    if "run" not in catalog_run_of_this_module:
        tatolabd = start_tatolabd_running_stream(four_probes_each_configured_its_own_way)
        stream_name = tatolabd.await_the_latest_attached_stream_loaded()
        local_api = tatolabd.local_api_client()
        local_api.await_every_node_running(stream=stream_name, timeout=GRAPH_READY_TIMEOUT_SECONDS)
        served = local_api.registry(stream_name)
        tatolabd.interrupt()
        tatolabd.await_clean_exit()
        catalog_run_of_this_module["run"] = CatalogRun(
            served_catalog={
                entry["type"]: entry
                for entry in served["nodes"]
                if entry["type"].startswith(PROBE_TYPE_PREFIX)
            },
            constructed_reports=tatolabd.marker_payloads("CONSTRUCTED"),
        )
    return catalog_run_of_this_module["run"]


@pytest.fixture
def served_catalog(catalog_run: CatalogRun) -> "dict[str, Any]":
    return catalog_run.served_catalog


def entry_for(served_catalog: "dict[str, Any]", probe: str) -> "dict[str, Any]":
    return served_catalog[f"{PROBE_TYPE_PREFIX}{probe}"]


def schema_for(served_catalog: "dict[str, Any]", probe: str) -> "dict[str, Any]":
    document = entry_for(served_catalog, probe).get("config_schema")
    assert document is not None, f"{probe} served a null config schema"
    return document


def test_a_processor_with_no_description_is_served_its_docstring(served_catalog):
    """The text the author already wrote reaches the agent reading the catalog."""
    assert entry_for(served_catalog, "ModelConfiguredProbe")["description"] == (
        "Configured by a model, and described by this docstring alone."
    )


def test_an_explicit_description_is_served_over_the_docstring(served_catalog):
    assert (
        entry_for(served_catalog, "UnconfiguredProbe")["description"]
        == "Takes no configuration at all"
    )


def test_a_dataclass_config_reaches_the_registry_with_types_defaults_and_descriptions(
    served_catalog,
):
    document = schema_for(served_catalog, "DataclassConfiguredProbe")

    assert document["properties"]["width"] == {
        "type": "integer",
        "description": "How wide the probe pretends its frames are.",
        "default": 640,
    }
    assert document["properties"]["label"]["default"] == "unlabelled"
    assert document["additionalProperties"] is False


def test_a_null_default_survives_the_hop_into_the_descriptor(served_catalog):
    """The document crosses into Rust through the msgpack value tree the data
    plane uses, where `None` is the one value that could arrive as absent."""
    fallback = schema_for(served_catalog, "DataclassConfiguredProbe")["properties"][
        "fallback"
    ]

    assert fallback["anyOf"] == [{"type": "string"}, {"type": "null"}]
    assert "default" in fallback, f"the null default was dropped: {fallback}"
    assert fallback["default"] is None


def test_a_typed_dict_config_reaches_the_registry(served_catalog):
    document = schema_for(served_catalog, "TypedDictConfiguredProbe")

    assert document["properties"]["width"]["type"] == "integer"
    assert document["properties"]["width"]["description"]
    # `total=False`, so nothing is required and the class admits an unknown key.
    assert "required" not in document
    assert "additionalProperties" not in document


def test_a_model_config_reaches_the_registry_as_the_model_describes_itself(
    served_catalog,
):
    document = schema_for(served_catalog, "ModelConfiguredProbe")

    assert document["properties"]["width"] == {
        "default": 1280,
        "title": "Width",
        "type": "integer",
    }
    assert "$schema" not in document
    assert "title" not in document, "the catalog entry names the processor already"


def test_a_processor_declaring_no_config_serves_an_empty_object_not_a_null(
    served_catalog,
):
    """A null would read as "this node does not know", which is a different
    claim from "this processor takes nothing"."""
    assert schema_for(served_catalog, "UnconfiguredProbe") == {
        "type": "object",
        "description": "This node declares no configuration.",
        "additionalProperties": False,
    }


def test_every_helper_constructed_the_config_class_its_processor_named(
    catalog_run: CatalogRun,
):
    """The half a served document cannot show: the object really arrived in the
    processor interpreter, built from the config the loaded graph carried."""
    constructed = {
        report["processor"]: report["config_type"] for report in catalog_run.constructed_reports
    }

    assert constructed == {
        "TypedDictConfiguredProbe": "dict",
        "DataclassConfiguredProbe": "DataclassProbeConfig",
        "ModelConfiguredProbe": "ModelProbeConfig",
        "UnconfiguredProbe": "NoneType",
    }
