# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A write by port name refuses a keyword the family it names does not take, and a write naming
no family refuses all of them, before it reaches the port."""

import pytest

from tatolab.runtime import _engine
from tatolab.runtime._node_the_wheel_hosts import link_output_data_writer_of

pytestmark = pytest.mark.usefixtures(
    "private_iceoryx2_domain_for_this_test_process",
    "log_records_this_process_sends_its_stand_in_parent",
)


@pytest.fixture
def outputs():
    data_plane = _engine.open_node_link_data_access_for_helper_process()
    data_plane.declare_ports([], ["out"])
    return link_output_data_writer_of(
        _engine.open_runtime_context_full_access_for_helper_process(
            {}, data_plane, "runtime-under-test", "writer-under-test"
        )
    )


@pytest.mark.parametrize(
    ("family", "keyword", "value"),
    [
        (None, "codec", "avc1"),
        (None, "start_sample", 0),
        ("image", "sample_rate", 48_000),
        ("records", "sample_type", "f32"),
        ("packets", "names", ["level"]),
        ("array", "formats", ["rgba8_unorm"]),
        ("channels", "residency", "gpu"),
    ],
)
def test_a_keyword_the_family_does_not_take_is_refused_by_name(outputs, family, keyword, value):
    with pytest.raises(TypeError, match=f"takes no {keyword}="):
        outputs.write("out", b"\x00", family=family, **{keyword: value})


def test_a_family_with_no_wire_name_is_refused(outputs):
    with pytest.raises(ValueError, match="none of image, array, channels, packets, records"):
        outputs.write("out", b"\x00", family="video")
