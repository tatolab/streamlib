// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A compute kernel created on the limited GPU capability in a node scope's body and in a computed
//! value's build, against a parent that answers each register and records each release.

use std::ffi::CString;
use std::sync::Arc;

use pyo3::prelude::*;
use pyo3::types::{PyDict, PyModule};

use super::ComputeKernelReleased;
use super::gpu_context::PythonGpuContextLimitedAccess;
use super::kernels::PythonKernelDispatchBatch;
use crate::python_helper_process_pixel_exchange::HelperProcessGpuExchangeClient;
use crate::python_node_scope::add_the_node_scope_to;

/// The parent as the helper's exchange client reaches it: each register answered with a kernel id
/// naming its source, each release recorded.
const STAND_IN_PARENT: &str = r#"
import threading
import time

registered = []
released = []

def escalate_request_to_parent(op):
    assert op["op"] == "register_compute_kernel", op
    registered.append((op["source"], threading.get_ident()))
    return {
        "result": "ok",
        "handle_id": f"kernel-of:{op['source']}",
        "bindings": []
        if op["source"].startswith("unbound")
        else [{"name": "filled_image", "kind": "storage_image"}],
    }

def release_to_parent_without_waiting(release_op):
    released.append(release_op)

def run_frames_until(scope, done, timeout_seconds=5.0):
    deadline = time.monotonic() + timeout_seconds
    answered = []
    while True:
        scope.run_frame(lambda: answered.append(done()))
        if answered and answered[-1]:
            return
        assert time.monotonic() < deadline, "the frame loop waited too long"
        time.sleep(0.002)
"#;

/// Run `source` with the node scope's names, `ComputeKernelReleased`, the stand-in parent's names,
/// `gpu`, a limited GPU capability whose exchange client reaches that parent, and `batch`, an
/// entered dispatch batch on the same client, in its globals.
fn run_against_a_stand_in_parent(source: &str) {
    Python::initialize();
    Python::attach(|python| {
        let module = PyModule::new(python, "computed_kernel_under_test").unwrap();
        add_the_node_scope_to(&module).unwrap();
        let globals = PyDict::new(python);
        globals.update(module.dict().as_mapping()).unwrap();
        globals
            .set_item(
                "ComputeKernelReleased",
                python.get_type::<ComputeKernelReleased>(),
            )
            .unwrap();
        python
            .run(
                &CString::new(STAND_IN_PARENT).unwrap(),
                Some(&globals),
                None,
            )
            .unwrap();
        let exchange_client = HelperProcessGpuExchangeClient::new(
            globals
                .get_item("escalate_request_to_parent")
                .unwrap()
                .unwrap()
                .unbind(),
            globals
                .get_item("release_to_parent_without_waiting")
                .unwrap()
                .unwrap()
                .unbind(),
            "/nonexistent/surface-share-of-a-test",
            Ok([0; 16]),
            "helper:computed-kernel-test".to_string(),
        );
        let exchange_client = Arc::new(exchange_client);
        let gpu = Bound::new(
            python,
            PythonGpuContextLimitedAccess::new_for_helper_process(Some(Arc::clone(
                &exchange_client,
            ))),
        )
        .unwrap();
        globals.set_item("gpu", gpu).unwrap();
        let batch = Bound::new(
            python,
            PythonKernelDispatchBatch {
                helper_process_exchange_client: Some(exchange_client),
                recording: Default::default(),
            },
        )
        .unwrap();
        globals.set_item("batch", batch).unwrap();
        if let Err(raised) = python.run(&CString::new(source).unwrap(), Some(&globals), None) {
            raised.print(python);
            panic!("the Python under test raised: {raised}");
        }
    });
}

#[test]
fn a_kernel_built_in_a_build_is_built_again_for_a_new_input_and_the_replaced_one_released() {
    run_against_a_stand_in_parent(
        r#"
declared = NodeScopeBeingDeclared()
width = declared.input(8)

def body():
    body_kernel = gpu.create_compute_kernel(source="body")
    return body_kernel, computed(lambda: gpu.create_compute_kernel(source=f"width {width.get()}"))

body_kernel, kernel = declared.declare(body)
scope = declared.start()
run_frames_until(scope, lambda: kernel.get().binding_names == ["filled_image"])
kernels_read_in_frames = []
scope.run_frame(lambda: kernels_read_in_frames.append(kernel.get()))
first_kernel = kernels_read_in_frames[0]

scope.write(width, 16)
run_frames_until(scope, lambda: kernel.get() is not first_kernel)
deadline = time.monotonic() + 5.0
while not released:
    assert time.monotonic() < deadline, "the replaced kernel was never released"
    time.sleep(0.002)

assert [source for source, _ in registered] == ["body", "width 8", "width 16"], registered
assert registered[1][1] != threading.get_ident(), "a build creates its kernel off the frame thread"
assert released == [{"op": "release_handle", "handle_id": "kernel-of:width 8"}], released
try:
    first_kernel.dispatch(bindings={}, group_count=(1, 1, 1))
except ComputeKernelReleased:
    pass
else:
    raise AssertionError("a released kernel dispatched")

scope.close()
assert released[1:] == [{"op": "release_handle", "handle_id": "kernel-of:width 16"}], released
body_kernel.close()
body_kernel.close()
assert released[2:] == [{"op": "release_handle", "handle_id": "kernel-of:body"}], released
"#,
    );
}

#[test]
fn a_kernel_nothing_closed_is_released_when_its_last_reference_goes() {
    run_against_a_stand_in_parent(
        r#"
kernel = gpu.create_compute_kernel(source="dropped")
del kernel
assert released == [{"op": "release_handle", "handle_id": "kernel-of:dropped"}], released
"#,
    );
}

#[test]
fn a_kernel_closed_after_a_batch_recorded_it_refuses_the_batch_as_released() {
    run_against_a_stand_in_parent(
        r#"
kernel = gpu.create_compute_kernel(source="unbound closed in a batch")
try:
    with batch:
        batch.dispatch(kernel, bindings={}, group_count=(1, 1, 1))
        kernel.close()
except ComputeKernelReleased:
    pass
else:
    raise AssertionError("a batch ran a kernel closed after it was recorded")
assert released == [{"op": "release_handle", "handle_id": "kernel-of:unbound closed in a batch"}], released
"#,
    );
}
