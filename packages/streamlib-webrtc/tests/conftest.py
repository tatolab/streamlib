# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Fixtures shared by this extension's suites."""

import os
import shutil
import tempfile
from pathlib import Path
from typing import Iterator

import pytest

ICEORYX2_DOMAIN_ROOT_ENVIRONMENT_VARIABLE = "STREAMLIB_ICEORYX2_DOMAIN_ROOT"
DOMAIN_ROOT_NAME_PREFIX = "sl-iox2-"


def _remove_iceoryx2_domain_roots_whose_test_process_is_gone() -> None:
    """A root outlives its session: a node can still be dropping as the process
    exits, and iceoryx2 warns when its files vanish first. So a root is swept by
    a later session once the process named in it has gone."""
    for domain_root in Path(tempfile.gettempdir()).glob(f"{DOMAIN_ROOT_NAME_PREFIX}*"):
        try:
            owning_process_id = int(domain_root.name[len(DOMAIN_ROOT_NAME_PREFIX) :].split("-")[0])
            os.kill(owning_process_id, 0)
        except ProcessLookupError:
            shutil.rmtree(domain_root, ignore_errors=True)
        except (ValueError, PermissionError, OSError):
            continue


@pytest.fixture(scope="session")
def private_iceoryx2_domain_for_this_test_process() -> "Iterator[Path]":
    """Stands in for the parent runtime: gives this process's helper nodes an
    iceoryx2 domain of their own, handed over the way a parent hands a helper
    its root.

    A test that builds `ProcessorLinkDataAccess()` directly has no parent to
    hand it one, and a helper handed none refuses to start. Every node this
    process opens then shares the one domain, which is what lets a source and a
    destination built side by side reach each other, and no other process's
    nodes are in it. The root is short: iceoryx2's socket paths are budgeted.
    """
    _remove_iceoryx2_domain_roots_whose_test_process_is_gone()
    domain_root = Path(
        tempfile.mkdtemp(prefix=f"{DOMAIN_ROOT_NAME_PREFIX}{os.getpid()}-")
    )
    previous_domain_root = os.environ.get(ICEORYX2_DOMAIN_ROOT_ENVIRONMENT_VARIABLE)
    os.environ[ICEORYX2_DOMAIN_ROOT_ENVIRONMENT_VARIABLE] = str(domain_root)
    try:
        yield domain_root
    finally:
        if previous_domain_root is None:
            os.environ.pop(ICEORYX2_DOMAIN_ROOT_ENVIRONMENT_VARIABLE, None)
        else:
            os.environ[ICEORYX2_DOMAIN_ROOT_ENVIRONMENT_VARIABLE] = previous_domain_root
