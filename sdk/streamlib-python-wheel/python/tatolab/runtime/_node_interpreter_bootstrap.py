# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only
# no-print-in-python ignores this file: bootstrap; the pre-install fatals below
# are written to raw stderr because the log channel does not exist yet.


from __future__ import annotations

import importlib
import json
import os
import platform
import signal
import stat
import sys
import sysconfig
import threading
import time
import traceback
from typing import Any, NoReturn, Optional

OLDEST_PYTHON_THE_LENT_RUNTIME_LOADS_IN = (3, 12)

DESCRIBE_ARGUMENT = "--describe"
DESCRIBE_INSTALLED_MODULES_ARGUMENT = "--describe-installed-modules"
CAST_A_SAVED_VALUE_ARGUMENT = "--cast-a-saved-value"
OUT_OF_STREAM_RUN_ARGUMENTS = (
    DESCRIBE_ARGUMENT,
    DESCRIBE_INSTALLED_MODULES_ARGUMENT,
    CAST_A_SAVED_VALUE_ARGUMENT,
)

#: The entry-point group an installed distribution declares its node classes in,
#: each entry's value a `module:qualname` import path.
INSTALLED_MODULE_NODES_ENTRY_POINT_GROUP = "tatolab.nodes"

_described_document_fd: Optional[int] = None


def _reserve_stdout_for_the_described_document() -> int:
    """Hand back a duplicate of fd 1 and point fd 1 and `sys.stdout` at stderr,
    so what an import prints cannot corrupt the one JSON document the parent
    parses off stdout. Reserved once; a second call returns the same duplicate."""
    global _described_document_fd
    if _described_document_fd is None:
        if sys.stdout is not None:
            sys.stdout.flush()
        _described_document_fd = os.dup(1)
        os.dup2(2, 1)
        sys.stdout = sys.stderr
    return _described_document_fd


def _this_interpreter_is_free_threaded() -> bool:
    return bool(sysconfig.get_config_var("Py_GIL_DISABLED"))


def _drop_this_bootstraps_own_directory_from_sys_path() -> None:
    """CPython puts the directory of a file run by path first on `sys.path`.
    Here that is the lent `tatolab/runtime/`, whose modules would otherwise
    shadow top-level names the project imports."""
    own_directory = os.path.realpath(os.path.dirname(os.path.abspath(__file__)))
    sys.path[:] = [
        entry
        for entry in sys.path
        if os.path.realpath(entry or os.curdir) != own_directory
    ]


def _refuse_an_interpreter_that_cannot_load_the_lent_runtime(import_error: str) -> NoReturn:
    refusal = (
        "[streamlib] this interpreter cannot load the lent runtime (tatolab.runtime), "
        "so it cannot run a node:\n"
        f"  interpreter: {sys.executable}\n"
        f"  implementation: {platform.python_implementation()}\n"
        f"  version: {' '.join(sys.version.split())}\n"
        f"  free-threaded: {'yes' if _this_interpreter_is_free_threaded() else 'no'}\n"
        f"  architecture: {platform.machine()}\n"
        f"  import error: {import_error.rstrip()}\n"
    )
    os.write(2, refusal.encode("utf-8", "backslashreplace"))
    sys.exit(1)


def _import_the_lent_runtime_or_refuse() -> None:
    if platform.python_implementation() != "CPython":
        _refuse_an_interpreter_that_cannot_load_the_lent_runtime(
            "not attempted: the runtime is a CPython extension module"
        )
    if sys.version_info < OLDEST_PYTHON_THE_LENT_RUNTIME_LOADS_IN:
        _refuse_an_interpreter_that_cannot_load_the_lent_runtime(
            "not attempted: the runtime needs CPython "
            + ".".join(str(part) for part in OLDEST_PYTHON_THE_LENT_RUNTIME_LOADS_IN)
            + " or newer"
        )
    if _this_interpreter_is_free_threaded():
        _refuse_an_interpreter_that_cannot_load_the_lent_runtime(
            "not attempted: the runtime is built for the GIL-enabled CPython"
        )
    try:
        importlib.import_module("tatolab.runtime._engine")
    except Exception as import_failure:
        _refuse_an_interpreter_that_cannot_load_the_lent_runtime(
            "".join(traceback.format_exception_only(type(import_failure), import_failure))
        )


if __name__ == "__main__":
    _drop_this_bootstraps_own_directory_from_sys_path()
    # Before the lent runtime and `tatolab.stream` import, so nothing their
    # imports print reaches the document; only the interpreter's own site hooks
    # run earlier.
    if sys.argv[1:2] and sys.argv[1] in OUT_OF_STREAM_RUN_ARGUMENTS:
        _reserve_stdout_for_the_described_document()
    _import_the_lent_runtime_or_refuse()

from tatolab.stream import (
    log,
)
from tatolab.stream._function_node_declaration import (
    FunctionNode,
)

from tatolab.runtime._engine import (
    engine_build_id_compiled_into_this_extension,
    open_node_link_data_access_for_helper_process,
)
from tatolab.runtime._function_node_run_as_a_standalone_node import (
    the_node_class_a_promoted_function_node_runs_as,
)

if sys.platform == "darwin":
    from tatolab.runtime._engine import (
        watch_for_this_helper_processes_parent_going_away,
    )

from tatolab.runtime._helper_process_environment_variables import (
    ENGINE_BUILD_ID_ENV as ENGINE_BUILD_ID_ENV,
    ENTRYPOINT_ENV as ENTRYPOINT_ENV,
    ESCALATE_FD_ENV as ESCALATE_FD_ENV,
    NATIVE_ESCALATE_FD_ENV as NATIVE_ESCALATE_FD_ENV,
    NODE_RUN_ID_ENV as NODE_RUN_ID_ENV,
    RUNTIME_ID_ENV as RUNTIME_ID_ENV,
)
from tatolab.runtime._helper_process_lifecycle import (
    HelperProcessLifecycle as HelperProcessLifecycle,
)
from tatolab.runtime._helper_process_log_forwarding import (
    CapturedEngineLogRecordForwarder as CapturedEngineLogRecordForwarder,
    ParentProcessLogSink as ParentProcessLogSink,
    write_a_describe_time_log_record_to_stderr as write_a_describe_time_log_record_to_stderr,
)
from tatolab.runtime._hosted_node import (
    unwire_link_data_access as unwire_link_data_access,
    wire_link_data_access as wire_link_data_access,
)
from tatolab.runtime._not_ready_placeholder_frames import (
    TransparentFramesOnTheImageOutputsOfANodeNotReady as TransparentFramesOnTheImageOutputsOfANodeNotReady,
)
from tatolab.runtime._parent_process_bridge import (
    EscalateRequestError as EscalateRequestError,
    HelperProcessProtocolError as HelperProcessProtocolError,
    ParentProcessBridge as ParentProcessBridge,
    _PendingEscalateResponse as _PendingEscalateResponse,
)
from tatolab.runtime._function_node_saved_value_cast import (
    SAVED_VALUE_CAST_DOCUMENT_CAST_KEY,
    SAVED_VALUE_CAST_DOCUMENT_NOT_CAST_BECAUSE_KEY,
    SAVED_VALUE_CAST_DOCUMENT_NOT_RUN_BECAUSE_KEY,
    SAVED_VALUE_CAST_REQUEST_INPUT_KEY,
    SAVED_VALUE_CAST_REQUEST_VALUE_KEY,
    SavedValueCastDidNotRun,
    SavedValueNotCast as SavedValueNotCast,
    the_saved_value_a_function_node_casts as the_saved_value_a_function_node_casts,
)
from tatolab.runtime._node_class_description import (
    DESCRIBED_NODE_TYPE_KEYS_AND_THE_STAMPS_AN_OLDER_TATOLAB_STREAM_DOES_NOT_WRITE as DESCRIBED_NODE_TYPE_KEYS_AND_THE_STAMPS_AN_OLDER_TATOLAB_STREAM_DOES_NOT_WRITE,
    NodeClassNotDescribable as NodeClassNotDescribable,
    describe_one_node_class as describe_one_node_class,
    load_node_class as load_node_class,
)


def _end_the_run_out_of_stream_without_finalizing_the_interpreter(exit_status: int) -> NoReturn:
    """Exit at once, the document already written.

    Finalizing would join every non-daemon thread and run every atexit handler
    a module the run imported left, and the parent refuses a describe or a cast
    that has not exited within its bound however complete its document. The
    parent kills the run's process group, so nothing the module started
    outlives it.
    """
    for standard_stream in (sys.__stdout__, sys.stderr):
        if standard_stream is not None:
            standard_stream.flush()
    os._exit(exit_status)


def _end_the_run_out_of_stream_once_its_parent_closes_stdin() -> None:
    """End the describe or cast and its process group when the stdin pipe its
    parent holds open reaches end-of-file: the parent is gone, and on macOS
    nothing else ends a run whose module blocks at import.

    Armed only on a pipe, so a run by hand with stdin on a terminal or
    `/dev/null` is not ended at once. Only a run leading its own process group,
    as the engine starts it, takes the group down with it.
    """
    try:
        if not stat.S_ISFIFO(os.fstat(0).st_mode):
            return
    except OSError:
        return

    def wait_for_the_end_of_stdin() -> None:
        try:
            while os.read(0, 4096):
                pass
        except OSError:
            return
        if os.getpgrp() == os.getpid():
            os.killpg(os.getpgrp(), signal.SIGKILL)
            # Darwin delivers a SIGKILL sent to the sender's own group after
            # killpg returns, where Linux delivers it before; an exit here would
            # race it, so wait for it to land.
            time.sleep(5)
        os._exit(1)

    threading.Thread(
        target=wait_for_the_end_of_stdin, name="describe-parent-watch", daemon=True
    ).start()


def describe_node_classes_onto_stdout(import_paths: "list[str]") -> int:
    """Print one JSON document describing each of `import_paths`; 1 if any was refused.

    Wire contract with the engine: stdout carries exactly
    `{"described_node_types": [...], "refused_node_types": [{"import_path": ...,
    "refusal": ...}]}`, and every refusal is also a block on stderr beginning
    `cannot describe <import path>: `.
    """
    described_document_fd = _reserve_stdout_for_the_described_document()
    described_node_types: "list[dict[str, Any]]" = []
    refused_node_types: "list[dict[str, str]]" = []
    for import_path in import_paths:
        try:
            described_node_types.append(describe_one_node_class(import_path))
        except NodeClassNotDescribable as refusal:
            refused_node_types.append({"import_path": import_path, "refusal": str(refusal)})
            sys.stderr.write(f"cannot describe {import_path}: {refusal}\n")
            sys.stderr.flush()
    described_document = json.dumps(
        {
            "described_node_types": described_node_types,
            "refused_node_types": refused_node_types,
        },
        allow_nan=False,
    )
    with os.fdopen(described_document_fd, "w", encoding="utf-8") as described_document_output:
        described_document_output.write(described_document + "\n")
    return 1 if refused_node_types else 0


def node_classes_installed_distributions_declare() -> "list[dict[str, str]]":
    """Every `tatolab.nodes` entry point an installed distribution declares,
    once per import path, in the order `importlib.metadata` finds them."""
    from importlib import metadata

    installed_modules: "list[dict[str, str]]" = []
    seen_import_paths: "set[str]" = set()
    for entry_point in metadata.entry_points(group=INSTALLED_MODULE_NODES_ENTRY_POINT_GROUP):
        import_path = entry_point.value.split("[", 1)[0].strip().replace(" ", "")
        if import_path in seen_import_paths:
            continue
        seen_import_paths.add(import_path)
        distribution = entry_point.dist
        installed_modules.append(
            {
                "import_path": import_path,
                "entry_point": entry_point.name,
                "distribution": distribution.name if distribution is not None else "",
                "distribution_version": (
                    distribution.version if distribution is not None else ""
                ),
            }
        )
    return installed_modules


def node_classes_the_projects_own_code_declares() -> "list[str]":
    """The import path of each node type the project's own code outside `imports/` declares,
    read from source as its node type listing reads it, the project being the working
    directory the bootstrap runs in."""
    from pathlib import Path

    from tatolab.stream._node_type_source_reader import (
        read_node_types_and_graph_generators_from_module_sources,
    )
    from tatolab.stream._project_node_type_listing_entry import project_module_sources

    read_from_source = read_node_types_and_graph_generators_from_module_sources(
        project_module_sources(Path.cwd())
    )
    return [node_type["type"] for node_type in read_from_source["node_types"]]


def describe_installed_modules_onto_stdout() -> int:
    """Print one JSON document describing every class installed distributions
    declare in `tatolab.nodes`, then every class the project's own code declares
    that none of them does; always 0, since a class that cannot be described is
    reported in the document and never fails the stream.

    Wire contract with the engine: the `--describe` document, plus
    `"installed_modules": [{"import_path": ..., "entry_point": ...,
    "distribution": ..., "distribution_version": ...}]` naming where each
    described or refused installed class came from, and `"project_node_types":
    [import_path, ...]` naming each described or refused class of the project's
    own code.
    """
    described_document_fd = _reserve_stdout_for_the_described_document()
    described_node_types: "list[dict[str, Any]]" = []
    refused_node_types: "list[dict[str, str]]" = []
    try:
        installed_modules = node_classes_installed_distributions_declare()
    except Exception:
        installed_modules = []
        sys.stderr.write(
            f"cannot list the `{INSTALLED_MODULE_NODES_ENTRY_POINT_GROUP}` entry points:\n"
            f"{traceback.format_exc()}"
        )
    for installed_module in installed_modules:
        import_path = installed_module["import_path"]
        try:
            described_node_types.append(describe_one_node_class(import_path))
        except NodeClassNotDescribable as refusal:
            refused_node_types.append({"import_path": import_path, "refusal": str(refusal)})
            sys.stderr.write(
                f"cannot describe {import_path}, which {installed_module['distribution']} "
                f"declares in `{INSTALLED_MODULE_NODES_ENTRY_POINT_GROUP}`: {refusal}\n"
            )
    installed_import_paths = {module["import_path"] for module in installed_modules}
    try:
        project_node_types = [
            import_path
            for import_path in node_classes_the_projects_own_code_declares()
            if import_path not in installed_import_paths
        ]
    except Exception:
        project_node_types = []
        sys.stderr.write(
            f"cannot list the node types of the project's own code:\n{traceback.format_exc()}"
        )
    for import_path in project_node_types:
        try:
            described_node_types.append(describe_one_node_class(import_path))
        except NodeClassNotDescribable as refusal:
            refused_node_types.append({"import_path": import_path, "refusal": str(refusal)})
            sys.stderr.write(
                f"cannot describe {import_path}, which the project's own code declares: "
                f"{refusal}\n"
            )
    sys.stderr.flush()
    described_document = json.dumps(
        {
            "described_node_types": described_node_types,
            "refused_node_types": refused_node_types,
            "installed_modules": installed_modules,
            "project_node_types": project_node_types,
        },
        allow_nan=False,
    )
    with os.fdopen(described_document_fd, "w", encoding="utf-8") as described_document_output:
        described_document_output.write(described_document + "\n")
    return 0


def cast_a_saved_value_onto_stdout(import_path: str, request_file: str) -> int:
    """Print one JSON document of the saved value the conversion `import_path` names casts; 0.

    `request_file` is the path of a file holding `{"input": <the conversion's wired input>,
    "value": <the saved value>}`, which the engine writes so a saved value too long for one
    command-line argument still reaches the body, and removes once the cast answers.
    Wire contract with the engine: stdout carries exactly `{"cast": <the value cast>}`;
    `{"not_cast_because": <why, in words>}` when the body ran and gave no cast of the value;
    or `{"not_run_because": <why, in words>}` when the body cannot run out of stream or its
    module does not import. The keys are the `SAVED_VALUE_CAST_*_KEY` constants.
    """
    described_document_fd = _reserve_stdout_for_the_described_document()
    with open(request_file, encoding="utf-8") as request:
        asked = json.load(request)
    cast_document: "dict[str, Any]"
    try:
        conversion = load_node_class(import_path)
    except Exception as failure:
        traceback.print_exc()
        cast_document = {
            SAVED_VALUE_CAST_DOCUMENT_NOT_RUN_BECAUSE_KEY: (
                f"`{import_path}` did not load: {_in_one_line(failure)}"
            )
        }
    else:
        try:
            cast_document = {
                SAVED_VALUE_CAST_DOCUMENT_CAST_KEY: the_saved_value_a_function_node_casts(
                    conversion,
                    import_path,
                    asked[SAVED_VALUE_CAST_REQUEST_INPUT_KEY],
                    asked[SAVED_VALUE_CAST_REQUEST_VALUE_KEY],
                )
            }
        except SavedValueCastDidNotRun as not_run:
            cast_document = {SAVED_VALUE_CAST_DOCUMENT_NOT_RUN_BECAUSE_KEY: str(not_run)}
        except SavedValueNotCast as not_cast:
            cast_document = {SAVED_VALUE_CAST_DOCUMENT_NOT_CAST_BECAUSE_KEY: str(not_cast)}
        except Exception as failure:
            traceback.print_exc()
            cast_document = {
                SAVED_VALUE_CAST_DOCUMENT_NOT_RUN_BECAUSE_KEY: (
                    f"`{import_path}` gave no cast: {_in_one_line(failure)}"
                )
            }
    with os.fdopen(described_document_fd, "w", encoding="utf-8") as cast_document_output:
        cast_document_output.write(json.dumps(cast_document, allow_nan=False) + "\n")
    return 0


def _in_one_line(failure: BaseException) -> str:
    """`failure`'s type and message, as the last line of its traceback reads."""
    return "".join(traceback.format_exception_only(type(failure), failure)).strip()


def _required_environment(name: str) -> str:
    value = os.environ.get(name)
    if not value:
        raise HelperProcessProtocolError(
            f"{name} is not set; a helper process is only ever started by the "
            f"engine's spawn host, which always sets it"
        )
    return value


def _refuse_an_engine_built_other_than_the_parents() -> None:
    """Parent and helper import one wheel, so differing ids mean this process
    imported another build — which would otherwise surface as every iceoryx2
    service open failing on a corrupted service."""
    helper_engine_build_id = engine_build_id_compiled_into_this_extension()
    parent_engine_build_id = os.environ.get(ENGINE_BUILD_ID_ENV)
    if not parent_engine_build_id:
        raise HelperProcessProtocolError(
            f"{ENGINE_BUILD_ID_ENV} is not set, so this helper cannot tell whether the "
            f"engine it imported (build {helper_engine_build_id}) is its parent's; a "
            f"helper process is only ever started by the engine's spawn host, which "
            f"always sets it"
        )
    if parent_engine_build_id != helper_engine_build_id:
        raise HelperProcessProtocolError(
            f"this helper imported engine build {helper_engine_build_id} from "
            f"{os.path.dirname(os.path.abspath(__file__))!r}, but its parent is engine "
            f"build {parent_engine_build_id}. Parent and helper must import the same "
            f"streamlib wheel; a different streamlib is earlier on this process's "
            f"sys.path, or the wheel was rebuilt under a running app"
        )


def _write_a_bootstrap_fatal_to_raw_stderr(bootstrap_failure: object) -> None:
    # Pre-install fatal: there is no channel to report it on, so this goes to
    # raw stderr, which the parent captures.
    sys.stderr.write(f"[streamlib] {bootstrap_failure}\n")
    sys.stderr.flush()


def main(arguments: "list[str]") -> int:
    try:
        _refuse_an_engine_built_other_than_the_parents()
    except HelperProcessProtocolError as bootstrap_failure:
        _write_a_bootstrap_fatal_to_raw_stderr(bootstrap_failure)
        return 1
    if arguments[:1] == [DESCRIBE_ARGUMENT]:
        if len(arguments) == 1:
            _write_a_bootstrap_fatal_to_raw_stderr(
                f"`{DESCRIBE_ARGUMENT}` takes one or more import paths, `module:qualname`"
            )
            return 1
        _end_the_run_out_of_stream_once_its_parent_closes_stdin()
        log.install_helper_process_sink(write_a_describe_time_log_record_to_stderr)
        try:
            describe_exit_status = describe_node_classes_onto_stdout(arguments[1:])
        except BaseException:
            traceback.print_exc()
            describe_exit_status = 1
        _end_the_run_out_of_stream_without_finalizing_the_interpreter(describe_exit_status)
    if arguments == [DESCRIBE_INSTALLED_MODULES_ARGUMENT]:
        _end_the_run_out_of_stream_once_its_parent_closes_stdin()
        log.install_helper_process_sink(write_a_describe_time_log_record_to_stderr)
        try:
            describe_exit_status = describe_installed_modules_onto_stdout()
        except BaseException:
            traceback.print_exc()
            describe_exit_status = 1
        _end_the_run_out_of_stream_without_finalizing_the_interpreter(describe_exit_status)
    if arguments[:1] == [CAST_A_SAVED_VALUE_ARGUMENT] and len(arguments) == 3:
        _end_the_run_out_of_stream_once_its_parent_closes_stdin()
        log.install_helper_process_sink(write_a_describe_time_log_record_to_stderr)
        try:
            cast_exit_status = cast_a_saved_value_onto_stdout(arguments[1], arguments[2])
        except BaseException:
            traceback.print_exc()
            cast_exit_status = 1
        _end_the_run_out_of_stream_without_finalizing_the_interpreter(cast_exit_status)
    if arguments:
        _write_a_bootstrap_fatal_to_raw_stderr(
            f"a node interpreter takes no arguments, `{DESCRIBE_ARGUMENT}` and "
            f"import paths, `{DESCRIBE_INSTALLED_MODULES_ARGUMENT}` alone, or "
            f"`{CAST_A_SAVED_VALUE_ARGUMENT}`, an import path and a request file; it was given "
            f"{arguments!r}"
        )
        return 1
    run_one_node_until_its_parent_tears_it_down()
    return 0


def _keep_the_native_escalate_socket_from_what_the_node_starts() -> None:
    """Mark the inherited native escalate socket close-on-exec, as the bridge's own is."""
    raw_fd = os.environ.get(NATIVE_ESCALATE_FD_ENV)
    if not raw_fd:
        return
    try:
        os.set_inheritable(int(raw_fd), False)
    except (ValueError, OSError) as unusable:
        raise HelperProcessProtocolError(
            f"{NATIVE_ESCALATE_FD_ENV}={raw_fd!r} is not a descriptor this helper holds"
        ) from unusable


def run_one_node_until_its_parent_tears_it_down() -> None:
    try:
        import_path = _required_environment(ENTRYPOINT_ENV)
        node_run_id = _required_environment(NODE_RUN_ID_ENV)
        runtime_id = os.environ.get(RUNTIME_ID_ENV, "")
        bridge = ParentProcessBridge.open_from_inherited_fd()
        _keep_the_native_escalate_socket_from_what_the_node_starts()
    except HelperProcessProtocolError as bootstrap_failure:
        _write_a_bootstrap_fatal_to_raw_stderr(bootstrap_failure)
        sys.exit(1)

    bridge.start_reading()
    log_sink = ParentProcessLogSink(bridge, node_run_id)
    log.install_helper_process_sink(log_sink)

    # Before this helper opens anything: the engine's own records, iceoryx2's
    # included, reach nobody in a child until the capture is up, and what an
    # iceoryx2 node or port refuses on the way up is exactly what explains a
    # helper that never gets further.
    engine_log_forwarder = CapturedEngineLogRecordForwarder(log_sink)
    engine_log_forwarder.capture_and_start()

    if sys.platform == "darwin":
        try:
            watch_for_this_helper_processes_parent_going_away(bridge.parent_channel_fd())
        except RuntimeError as watch_failure:
            engine_log_forwarder.stop_after_forwarding_what_is_left()
            log.error(
                "the helper could not watch its parent, so it could outlive it",
                error=str(watch_failure),
            )
            sys.exit(1)

    try:
        node_class = load_node_class(import_path)
        link_data_access = open_node_link_data_access_for_helper_process()
        if isinstance(node_class, FunctionNode):
            node_class = the_node_class_a_promoted_function_node_runs_as(
                node_class, import_path, node_run_id, link_data_access
            )
    except Exception as startup_failure:
        engine_log_forwarder.stop_after_forwarding_what_is_left()
        log.error(
            "the helper could not load its node",
            entrypoint=import_path,
            error=str(startup_failure),
            traceback=traceback.format_exc(),
        )
        sys.exit(1)

    log.info("helper process started", entrypoint=import_path, pid=os.getpid())
    HelperProcessLifecycle(
        bridge, node_class, runtime_id, node_run_id, link_data_access
    ).run_until_the_parent_is_done()
    engine_log_forwarder.stop_after_forwarding_what_is_left()
    log.info("helper process exiting")


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
