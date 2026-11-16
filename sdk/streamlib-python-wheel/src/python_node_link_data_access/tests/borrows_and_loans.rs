// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::python_node_link_data_access::output_port_writes_and_loans::PythonBufferHeldForOneWrite;

/// A read hands out the sample it arrived in: a `memoryview` of it and a
/// DLPack tensor over it both start at the sample's own bytes, read-only,
/// and `bytes()` is the copy.
#[test]
fn a_borrowed_read_shares_the_samples_memory_through_memoryview_and_dlpack() {
    Python::initialize();
    Python::attach(|python| {
        let planes = one_wired_link_as_python_objects(python, "borrowshares");
        let source = planes.get_item("source").unwrap().unwrap();
        let destination = planes.get_item("destination").unwrap().unwrap();
        source
            .call_method1(
                "write_to_output_port",
                (
                    "frames_to_downstream",
                    PyBytes::new(python, b"one shared sample"),
                ),
            )
            .unwrap();
        let received = destination
            .call_method1("read_from_input_port", ("frames_from_upstream",))
            .unwrap();
        let (sample_address, is_a_channel_sample) = received
            .cast::<PythonPortPayloadBorrow>()
            .expect("a read hands back a PortPayloadBorrow")
            .get()
            .borrowed_payload_address_and_whether_it_is_a_channel_sample();
        assert!(is_a_channel_sample, "the read borrows the channel sample");

        let memoryview = python
            .import("builtins")
            .unwrap()
            .getattr("memoryview")
            .unwrap()
            .call1((&received,))
            .unwrap();
        let memoryview_buffer =
            PythonBufferHeldForOneWrite::of("frames_from_upstream", &memoryview).unwrap();
        assert_eq!(
            memoryview_buffer.bytes().as_ptr() as usize,
            sample_address,
            "memoryview(read) must be the sample's bytes, not a copy"
        );
        drop(memoryview_buffer);
        assert!(
            memoryview
                .getattr("readonly")
                .unwrap()
                .extract::<bool>()
                .unwrap()
        );

        let capsule = received.call_method0("__dlpack__").unwrap();
        // SAFETY: an unconsumed `__dlpack__` capsule carries a
        // `ManagedTensor` under the name `dltensor`.
        let tensor_data_address = unsafe {
            let managed_tensor =
                pyo3::ffi::PyCapsule_GetPointer(capsule.as_ptr(), c"dltensor".as_ptr())
                    as *const streamlib_adapter_cuda::dlpack::ManagedTensor;
            (*managed_tensor).dl_tensor.data as usize
        };
        assert_eq!(tensor_data_address, sample_address);
        assert_eq!(
            received
                .call_method0("__dlpack_device__")
                .unwrap()
                .extract::<(i32, i32)>()
                .unwrap(),
            (1, 0)
        );

        let copied = python
            .import("builtins")
            .unwrap()
            .getattr("bytes")
            .unwrap()
            .call1((&received,))
            .unwrap();
        assert_eq!(
            copied.cast::<PyBytes>().unwrap().as_bytes(),
            b"one shared sample"
        );
    });
}

/// `ctx.loan` publishes what its block wrote on a clean exit, gives the
/// sample back unpublished when the block raised, and a write takes any
/// contiguous buffer.
#[test]
fn a_loan_publishes_on_a_clean_exit_and_gives_the_sample_back_when_its_block_raised() {
    Python::initialize();
    Python::attach(|python| {
        let planes = one_wired_link_as_python_objects(python, "loanexit");
        python
            .run(
                cr#"
with source.loan_on_output_port("frames_to_downstream", 5, timestamp_ns=77) as out:
    assert len(out) == 5
    with memoryview(out) as view:
        view[:] = b"hello"
published, timestamp_ns = destination.read_from_input_port_with_timestamp("frames_from_upstream")
assert bytes(published) == b"hello", bytes(published)
assert timestamp_ns == 77, timestamp_ns

class TheBlockFailed(Exception):
    pass

try:
    with source.loan_on_output_port("frames_to_downstream", 4) as out:
        with memoryview(out) as view:
            view[:] = b"lost"
        raise TheBlockFailed()
except TheBlockFailed:
    pass
else:
    raise AssertionError("the block's exception must propagate")
assert destination.read_from_input_port("frames_from_upstream") is None

source.write_to_output_port("frames_to_downstream", bytearray(b"from a bytearray"))
source.write_to_output_port("frames_to_downstream", memoryview(b"xx from a view")[3:])
assert bytes(destination.read_from_input_port("frames_from_upstream")) == b"from a bytearray"
assert bytes(destination.read_from_input_port("frames_from_upstream")) == b"from a view"
"#,
                Some(&planes),
                Some(&planes),
            )
            .unwrap();
    });
}

/// Leaving a loan's block while an export of it lives publishes nothing
/// and raises `BufferError`; the port stays loaned, refusing a second loan
/// and a write, until the export goes.
#[test]
fn a_loan_left_exported_publishes_nothing_and_holds_its_port_until_the_export_goes() {
    Python::initialize();
    Python::attach(|python| {
        let planes = one_wired_link_as_python_objects(python, "loanexported");
        python
            .run(
                cr#"
def refused(call, refusal_type):
    try:
        call()
    except refusal_type as refusal:
        return str(refusal)
    raise AssertionError(f"{call} must raise {refusal_type.__name__}")

view = None
def leave_the_block_with_an_export_alive():
    global view
    with source.loan_on_output_port("frames_to_downstream", 4) as out:
        view = memoryview(out)
        view[:] = b"kept"

assert "live export" in refused(leave_the_block_with_an_export_alive, BufferError)
assert "has a loan open" in refused(
    lambda: source.loan_on_output_port("frames_to_downstream", 4), RuntimeError
)
assert "has a loan open" in refused(
    lambda: source.write_to_output_port("frames_to_downstream", b"x"), RuntimeError
)
assert destination.read_from_input_port("frames_from_upstream") is None

view.release()
with source.loan_on_output_port("frames_to_downstream", 2) as out:
    tensor = out.__dlpack__()
    del tensor
    with memoryview(out) as view:
        view[:] = b"ok"
assert bytes(destination.read_from_input_port("frames_from_upstream")) == b"ok"

def leave_the_block_with_a_tensor_alive():
    global tensor
    with source.loan_on_output_port("frames_to_downstream", 2) as out:
        tensor = out.__dlpack__()

assert "live export" in refused(leave_the_block_with_a_tensor_alive, BufferError)
del tensor
source.write_to_output_port("frames_to_downstream", b"after")
assert bytes(destination.read_from_input_port("frames_from_upstream")) == b"after"
"#,
                Some(&planes),
                Some(&planes),
            )
            .unwrap();
    });
}
