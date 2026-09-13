# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""What a running node tells an agent about a processor's config, end to end.

The declaration suite proves the document is derived, and the wheel's Rust tests
prove it reaches the shape `/api/registry` serializes. This is the whole path on
a real node with each processor in its own helper process — which needs a
running graph, and a running graph initializes a GPU context, so it runs on the
rig like every other live proof in this suite.
"""

import json
import re
from pathlib import Path
from typing import Any, Iterator

import pytest

from app_under_test import start_app

pytestmark = pytest.mark.requires_gpu

APP = Path(__file__).parent / "processor_config_catalog_app.py"

CATALOG = re.compile(r"MARKER:CATALOG (\{.*\})\s*$", re.MULTILINE)
CONSTRUCTED = re.compile(r"MARKER:CONSTRUCTED (\{.*?\})")


@pytest.fixture(scope="module")
def catalog_app_output() -> "Iterator[str]":
    """One node, one run: four helper spawns are the cost, so they are paid once.

    Module-scoped, so it reaps its own process group rather than reaching for
    the function-scoped fixture every other app suite uses. Leaving that to a
    failed assertion would strand a live engine holding a socket and an
    iceoryx2 node.
    """
    app = start_app(APP)
    try:
        app.await_output_containing("MARKER:CATALOG", "the served catalog")
        app.await_marker("CLEAN_EXIT")
        app.await_clean_exit()
        yield app.output
    finally:
        app.kill_process_group()


@pytest.fixture(scope="module")
def served_catalog(catalog_app_output: str) -> "dict[str, Any]":
    match = CATALOG.search(catalog_app_output)
    assert match is not None, f"no catalog line:\n{catalog_app_output}"
    return json.loads(match.group(1))


def entry_for(served_catalog: "dict[str, Any]", probe: str) -> "dict[str, Any]":
    return served_catalog[f"processor_config_catalog_probes:{probe}"]


def schema_for(served_catalog: "dict[str, Any]", probe: str) -> "dict[str, Any]":
    document = entry_for(served_catalog, probe).get("config_schema")
    assert document is not None, f"{probe} served a null config schema"
    return document


def test_a_class_the_app_imported_and_never_added_is_in_the_catalog(served_catalog):
    """What an agent reads to learn what a node could run, not what it is running.

    Its decorator registered it at import; nothing put it in the graph. Restore
    registration to the first add and it is invisible here, which is the gap
    that made an app's unused effects undiscoverable.
    """
    entry = entry_for(served_catalog, "ImportedButNeverAddedProbe")

    assert entry["config_schema"]["properties"]["width"]["type"] == "integer", (
        f"an unadded class must carry the same config schema an added one does: {entry}"
    )
    assert entry["runtime"] == "python"
    assert entry["entrypoint"] == (
        "processor_config_catalog_probes:ImportedButNeverAddedProbe"
    )


def test_a_processor_with_no_description_is_served_its_docstring(served_catalog):
    """The text the author already wrote reaches the agent reading the catalog."""
    assert entry_for(served_catalog, "ImportedButNeverAddedProbe")["description"] == (
        "An effect the app knows how to run and has not been asked to."
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
        "description": "This processor declares no configuration.",
        "additionalProperties": False,
    }


def test_every_helper_constructed_the_config_class_its_processor_named(
    catalog_app_output,
):
    """The half a served document cannot show: the object really arrived in the
    child, built from the mapping `rt.add` recorded."""
    constructed = {
        report["processor"]: report["config_type"]
        for report in (
            json.loads(match.group(1)) for match in CONSTRUCTED.finditer(catalog_app_output)
        )
    }

    assert constructed == {
        "TypedDictConfiguredProbe": "dict",
        "DataclassConfiguredProbe": "DataclassProbeConfig",
        "ModelConfiguredProbe": "ModelProbeConfig",
        "UnconfiguredProbe": "NoneType",
    }
