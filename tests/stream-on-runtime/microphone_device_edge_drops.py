# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""What `MicrophoneSource` reports it dropped at the device edge, read off `tatolabd`'s records.

The count is the source's own and `graph` does not render it, so its records'
`dropped_blocks` field is where a test reads it.
"""

import re

from runtime_process_under_test import RuntimeProcessUnderTest

MICROPHONE_SOURCE_RECORD_DROPPED_BLOCKS = re.compile(
    r"streamlib_media_builtins::microphone_source — .*\bdropped_blocks=(\d+)"
)


def assert_the_microphone_dropped_no_blocks_at_the_device_edge(
    stopped_tatolabd: RuntimeProcessUnderTest,
) -> None:
    """Every `MicrophoneSource` record carrying `dropped_blocks` says zero.

    Its teardown record always carries the field, so a run whose records name
    no count at all fails here rather than passing on silence.
    """
    dropped_block_counts = [
        int(match.group(1))
        for match in MICROPHONE_SOURCE_RECORD_DROPPED_BLOCKS.finditer(stopped_tatolabd.stderr_text)
    ]
    assert dropped_block_counts, (
        f"no MicrophoneSource record carried `dropped_blocks`, not even its teardown "
        f"record:\n{stopped_tatolabd.recent_stderr()}"
    )
    assert set(dropped_block_counts) == {0}, (
        f"the microphone dropped blocks at the device edge, so the run was not keeping up "
        f"(counts reported: {dropped_block_counts}):\n{stopped_tatolabd.recent_stderr()}"
    )
