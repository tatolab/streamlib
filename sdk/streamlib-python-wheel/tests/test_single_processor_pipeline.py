# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`streamlib.testing.SingleProcessorTestPipeline`, driving a real processor.

The harness is public API a user writes their own tests against, so what is
worth breaking a build over is that it works end to end: a bag fed from the
test reaches a processor running in its own helper process, and what that
processor writes comes back to the test — across two process boundaries, in
order, losing nothing.
"""

import pytest

from single_processor_under_test import ConfiguredScaler, DoublingFilter
from streamlib.testing import SingleProcessorTestPipeline

pytestmark = [
    pytest.mark.requires_gpu,
    pytest.mark.skip(
        reason=(
            "#1759: the harness has no way to know the processor under test's helper has "
            "attached, so bags fed before it does are dropped by the link. The transport "
            "itself works — the same graph round-trips with a settle before the first feed"
        )
    ),
]


def test_a_fed_bag_reaches_the_processor_and_its_output_comes_back():
    """The whole point of the harness, in one round trip.

    The feeder and collector are native endpoints in the app process; the
    processor under test is a child interpreter. A bag crosses out and back.
    """
    with SingleProcessorTestPipeline(DoublingFilter) as pipeline:
        pipeline.feed("numbers_from_upstream", {"value": 21})
        assert pipeline.await_bag("numbers_to_downstream") == {"value": 42}


def test_every_fed_bag_comes_back_in_order():
    """`every_sample` on the collector is what makes an assertion honest: a
    dropped bag under a burst would let a broken processor look correct."""
    with SingleProcessorTestPipeline(DoublingFilter) as pipeline:
        for value in range(8):
            pipeline.feed("numbers_from_upstream", {"value": value})
        collected = pipeline.await_bags("numbers_to_downstream", 8)
        assert collected == [{"value": value * 2} for value in range(8)]


def test_the_processor_under_test_is_constructed_with_the_config():
    """`config=` reaches the processor's constructor in its own process."""
    with SingleProcessorTestPipeline(ConfiguredScaler, config={"factor": 5}) as pipeline:
        pipeline.feed("numbers_from_upstream", {"value": 3})
        assert pipeline.await_bag("numbers_to_downstream") == {"value": 15}


def test_a_port_the_processor_does_not_declare_is_named_in_the_error():
    with SingleProcessorTestPipeline(DoublingFilter) as pipeline:
        with pytest.raises(KeyError, match="numbers_to_downstream"):
            pipeline.feed("no_such_port", {"value": 1})


def test_a_processor_that_never_produces_fails_rather_than_hanging():
    """The failure a test is actually looking for is "nothing came back", so it
    has to arrive as an assertion rather than a wedged run."""
    with SingleProcessorTestPipeline(DoublingFilter) as pipeline:
        with pytest.raises(AssertionError, match="produced nothing"):
            pipeline.await_bag("numbers_to_downstream", timeout=2.0)


def test_two_pipelines_at_once_are_refused_by_name():
    """One engine owns the process's shutdown signals, so pipelines run one at
    a time — caught here rather than surfacing as a signals error."""
    with SingleProcessorTestPipeline(DoublingFilter):
        with pytest.raises(RuntimeError, match="one at a time"):
            with SingleProcessorTestPipeline(DoublingFilter):
                pass
